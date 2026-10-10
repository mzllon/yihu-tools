#!/usr/bin/env python3
"""快捷键管理插件（M5 开放 API v2 旗舰示例：真插件形态）。

管理 GNOME「自定义快捷键」（设置 → 键盘 → 自定义快捷键，即
gsettings org.gnome.settings-daemon.plugins.media-keys.custom-keybindings）。
宿主无此内置功能——完全靠开放 API 实现：

- 查：settings.read@… 读列表与各条目（name/command/binding）
- 增/改/删：settings.write@… 编排写列表 + 条目键（reset 清旧条目），
  「摘除 → 写回」触发 gsd-media-keys 重新抓键（yihu-panel 同款机制）
- 反馈：notify

交互（呼出面板内）：
  hotkey / 快捷键 / kj         列出全部自定义快捷键 + 帮助
  key add <键> | 命令 | 名称    新增（名称可省），如
                              key add <Super>e | nautilus | 打开文件管理器
  key del <关键词>             匹配（键/名称/命令），给出确认行，回车执行
  key edit <关键词> | 新键 | 命令 | 名称   修改匹配的第一条

插件沙箱内无 gsettings，所有读写经宿主能力代理；manifest 声明
schema 白名单，安装页徽章明示，逐请求审计。
"""

import ast
import json
import sys

SCHEMA = "org.gnome.settings-daemon.plugins.media-keys"
SUB = SCHEMA + ".custom-keybinding"
LIST_KEY = "custom-keybindings"
LIST_PREFIX = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/"

HELP = [
    "key add <键> | 命令 | 名称 —— 新增（例：key add <Super>e | nautilus | 打开文件管理器）",
    "key del <关键词> —— 删除匹配的快捷键",
    "key edit <关键词> | 新键 | 命令 | 名称 —— 修改匹配的第一条",
]


def send(o):
    sys.stdout.write(json.dumps(o, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def _results(qid, rows):
    """rows: [(title, subtitle)]；payload=!none（不复制）。"""
    items = [
        {"title": t[:80], "subtitle": s[:110],
         "icon": "input-keyboard-symbolic", "payload": "!none"}
        for t, s in rows
    ]
    send({"type": "results", "query_id": qid, "items": items})


def fmt_binding(b):
    """<Super>e → Win+E（展示用）"""
    return (str(b).replace("<Super>", "Win+").replace("<Control>", "Ctrl+")
            .replace("<Primary>", "Ctrl+").replace("<Alt>", "Alt+")
            .replace("<Shift>", "Shift+"))


def ast_repr(items):
    return "[" + ", ".join("'" + str(i) + "'" for i in items) + "]"


# ---- 能力请求编排 ----------------------------------------------------------
# 两种活动（按 qid 隔离）：
#   READ[qid]  = {"cb", "entries": {path: {k: v}}, "paths": []}
#   QUEUE[qid] = {"ops": [(cap, params)], "tag"}；ok → 发下一个；全完 → notify
# PENDING[request_id] = (qid, slot)；slot=None 表示写队列推进，其余为读取项。

READ = {}
QUEUE = {}
PENDING = {}
_seq = 1000


def _next_id():
    global _seq
    _seq += 1
    return _seq


def read_all(qid, cb):
    READ[qid] = {"cb": cb, "entries": {}, "paths": []}
    _req_read(qid, "list", SCHEMA, LIST_KEY)


def _req_read(qid, slot, schema, key):
    rid = _next_id()
    PENDING[rid] = (qid, slot)
    send({"type": "capability_request", "id": rid,
          "capability": "settings.read", "params": {"schema": schema, "key": key}})


def start_queue(qid, ops, tag):
    QUEUE[qid] = {"ops": list(ops), "tag": tag}
    _advance(qid)


def _advance(qid):
    q = QUEUE.get(qid)
    if not q:
        return
    if not q["ops"]:
        del QUEUE[qid]
        send({"type": "capability_request", "id": _next_id(),
              "capability": "notify",
              "params": {"summary": "一呼快捷键", "body": q["tag"] + " 完成"}})
        return
    cap, params = q["ops"][0]
    rid = _next_id()
    PENDING[rid] = (qid, None)
    send({"type": "capability_request", "id": rid,
          "capability": cap, "params": params})


def on_response(msg):
    rid = msg.get("id")
    if rid not in PENDING:
        return
    qid, slot = PENDING.pop(rid)

    if slot is None:  # 写队列推进
        q = QUEUE.get(qid)
        if not q:
            return
        if not msg.get("ok"):
            del QUEUE[qid]
            _results(qid, [("❌ " + q["tag"] + "失败：" + str(msg.get("error", "?")), "")])
            return
        q["ops"].pop(0)
        _advance(qid)
        return

    st = READ.get(qid)
    if not st:
        return
    if not msg.get("ok"):
        READ.pop(qid, None)
        _results(qid, [("❌ 读取失败：" + str(msg.get("error", "?")), "")])
        return
    try:
        value = ast.literal_eval(json.loads(msg.get("data") or '""'))
    except Exception:
        value = msg.get("data", "")

    if slot == "list":
        st["paths"] = [p for p in value if isinstance(p, str)]
        for p in st["paths"]:
            for k in ("name", "command", "binding"):
                _req_read(qid, p + "|" + k, SUB, k)
        if not st["paths"]:
            READ.pop(qid, None)
            st["cb"](qid, [], {})
        return

    path, key = slot.split("|", 1)
    st["entries"].setdefault(path, {})[key] = value
    done = all(
        key in st["entries"].get(p, {})
        for p in st["paths"]
        for key in ("name", "command", "binding")
    )
    if done:
        READ.pop(qid, None)
        st["cb"](qid, st["paths"], st["entries"])


# ---- 指令处理 ---------------------------------------------------------------

EXEC = {"qid": 0, "ops": None, "tag": "", "args": None}


def handle(qid, text):
    parts = text.split()
    if parts[0] == "key":
        sub = text[4:].strip()
        if sub.startswith("add "):
            plan_add(qid, sub[4:])
        elif sub.startswith("del "):
            read_all(qid, lambda q, ps, es: plan_del(q, sub[4:].strip().lower(), ps, es))
        elif sub.startswith("edit "):
            seg = [x.strip() for x in sub[5:].split("|")]
            if len(seg) < 3:
                _results(qid, [("格式：key edit <关键词> | 新键 | 命令 | 名称",
                                "例：key edit nautilus | <Super>f | nautilus | 文件")])
                return
            read_all(qid, lambda q, ps, es: plan_edit(
                q, seg[0].lower(), seg[1], seg[2],
                seg[3] if len(seg) > 3 else seg[2], ps, es))
        else:
            _results(qid, [(h, "") for h in HELP])
    else:
        read_all(qid, show_list)


def show_list(qid, paths, entries):
    if not paths:
        _results(qid, [("（还没有自定义快捷键）",
                        "新增：key add <Super>e | nautilus | 打开文件管理器")])
        return
    rows = []
    for p in paths:
        e = entries.get(p, {})
        rows.append((fmt_binding(e.get("binding", "?")) + " → " + e.get("name", ""),
                     e.get("command", "") + "　（" + p.rstrip("/").rsplit("/", 1)[-1] + "）"))
    rows.append(("新增：key add <键> | 命令 | 名称",
                 "删除：key del <关键词>　修改：key edit <关键词> | 新键 | 命令 | 名称"))
    _results(qid, rows)


def entry_of(paths, entries, kw):
    for p in paths:
        e = entries.get(p, {})
        blob = (e.get("binding", "") + " " + e.get("name", "") + " "
                + e.get("command", "")).lower()
        if kw in blob:
            return p, e
    return None


def plan_add(qid, body):
    seg = [x.strip() for x in body.split("|")]
    if len(seg) < 2:
        _results(qid, [("格式：key add <键> | 命令 | 名称",
                        "例：key add <Super>e | nautilus | 打开文件管理器")])
        return
    binding, command = seg[0], seg[1]
    name = seg[2] if len(seg) > 2 else command
    EXEC["qid"] = qid
    EXEC["args"] = (binding, command, name)
    read_all(qid, _plan_add_go)


def _plan_add_go(qid, paths, entries):
    binding, command, name = EXEC["args"]
    used = {p.rstrip("/").rsplit("custom", 1)[-1] for p in paths}
    n = 0
    while str(n) in used:
        n += 1
    new_path = LIST_PREFIX + "custom" + str(n) + "/"
    # 摘除 → 写条目 → 写回：强制 gsd-media-keys 重新抓键
    ops = [
        ("settings.write", {"schema": SCHEMA, "key": LIST_KEY,
                            "value": ast_repr(paths), "op": "set"}),
        ("settings.write", {"schema": SUB + ":" + new_path, "key": "name",
                            "value": name, "op": "set"}),
        ("settings.write", {"schema": SUB + ":" + new_path, "key": "command",
                            "value": command, "op": "set"}),
        ("settings.write", {"schema": SUB + ":" + new_path, "key": "binding",
                            "value": binding, "op": "set"}),
        ("settings.write", {"schema": SCHEMA, "key": LIST_KEY,
                            "value": ast_repr(paths + [new_path]), "op": "set"}),
    ]
    EXEC.update(qid=qid, ops=ops, tag="新增快捷键")
    _results(qid, [("✅ 回车确认：新增 " + fmt_binding(binding) + " → " + name,
                    "命令 " + command)])


def plan_del(qid, kw, paths, entries):
    hit = entry_of(paths, entries, kw)
    if not hit:
        _results(qid, [("❌ 没有匹配「" + kw + "」的快捷键", "")])
        return
    p, e = hit
    keep = ast_repr([x for x in paths if x != p])
    ops = [("settings.write", {"schema": SCHEMA, "key": LIST_KEY,
                               "value": keep, "op": "set"})]
    for k in ("name", "command", "binding"):
        ops.append(("settings.write", {"schema": SUB + ":" + p, "key": k, "op": "reset"}))
    EXEC.update(qid=qid, ops=ops, tag="删除快捷键")
    _results(qid, [("✅ 回车确认：删除 " + fmt_binding(e.get("binding", "?")) + " → " + e.get("name", ""),
                    e.get("command", ""))])


def plan_edit(qid, kw, new_binding, new_command, new_name, paths, entries):
    hit = entry_of(paths, entries, kw)
    if not hit:
        _results(qid, [("❌ 没有匹配「" + kw + "」的快捷键", "")])
        return
    p, e = hit
    # 摘除 → 改条目 → 写回
    ops = [
        ("settings.write", {"schema": SCHEMA, "key": LIST_KEY,
                            "value": ast_repr([x for x in paths if x != p]), "op": "set"}),
        ("settings.write", {"schema": SUB + ":" + p, "key": "name",
                            "value": new_name, "op": "set"}),
        ("settings.write", {"schema": SUB + ":" + p, "key": "command",
                            "value": new_command, "op": "set"}),
        ("settings.write", {"schema": SUB + ":" + p, "key": "binding",
                            "value": new_binding, "op": "set"}),
        ("settings.write", {"schema": SCHEMA, "key": LIST_KEY,
                            "value": ast_repr(paths), "op": "set"}),
    ]
    EXEC.update(qid=qid, ops=ops, tag="修改快捷键")
    _results(qid, [("✅ 回车确认：改为 " + fmt_binding(new_binding) + " → " + new_name,
                    "命令 " + new_command)])


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        t = msg.get("type")
        if t == "init":
            send({"type": "ready"})
        elif t == "capability_response":
            on_response(msg)
        elif t == "query":
            qid = msg.get("id", 0)
            text = msg.get("text", "").strip()
            if text.startswith("key "):
                handle(qid, text)
            elif text and any(k in text.lower() for k in ("hotkey", "快捷键", "kj")):
                handle(qid, text)
        elif t == "activate":
            # 确认行回车：执行当前 EXEC 计划（每次 query 重算，防误触）
            if EXEC.get("ops"):
                qid = EXEC["qid"]
                ops = EXEC["ops"]
                tag = EXEC["tag"]
                EXEC["ops"] = None
                start_queue(qid, ops, tag)
        elif t == "shutdown":
            sys.exit(0)


if __name__ == "__main__":
    main()
