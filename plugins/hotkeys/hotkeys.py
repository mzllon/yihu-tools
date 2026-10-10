#!/usr/bin/env python3
"""快捷键管理插件（M5 UI 插件层旗舰示例：原生模板表单）。

管理 GNOME「自定义快捷键」。UI 交互版（2026-10-10，按用户反馈推翻
文本指令行）——插件发 ui.show 声明式表单，宿主渲染原生窗口：

- 「新增」→ 表单：名称文本框 + **点击按下组合键**的快捷键录入 +
  **应用列表下拉**（点选"文件管理器"即可，不必知道 nautilus）
- 「修改」→ 同表单预填当前值
- 「删除」→ 确认行回车（列表内完成，无需额外 UI）

用户全程不接触 <Super>e 语法和命令行。所有读写经宿主能力代理
（settings.read/write schema 白名单），安装页徽章明示。
"""

import ast
import json
import sys

SCHEMA = "org.gnome.settings-daemon.plugins.media-keys"
SUB = SCHEMA + ".custom-keybinding"
LIST_KEY = "custom-keybindings"
LIST_PREFIX = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/"


def send(o):
    sys.stdout.write(json.dumps(o, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def fmt_binding(b):
    return (str(b).replace("<Super>", "Win+").replace("<Control>", "Ctrl+")
            .replace("<Primary>", "Ctrl+").replace("<Alt>", "Alt+")
            .replace("<Shift>", "Shift+"))


def ast_repr(items):
    return "[" + ", ".join("'" + str(i) + "'" for i in items) + "]"


READ = {}      # qid -> {"cb", "entries", "paths"}
QUEUE = {}     # qid -> {"ops", "tag"}
PENDING = {}   # request_id -> (qid, slot)
_seq = 2000


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

    if slot is None:
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


# ---- UI 计划（activate 触发 ui.show） --------------------------------------

UI_PLAN = None  # {"ui_id", "kind", "args"}


def ui_form(qid, ui_id, kind, args):
    """构建声明式表单 spec。"""
    if kind == "add":
        spec = {
            "title": "新增快捷键",
            "submit": "保存",
            "fields": [
                {"key": "name", "kind": "text", "label": "名称",
                 "placeholder": "打开文件管理器"},
                {"key": "binding", "kind": "hotkey", "label": "快捷键（点击后按下组合键）"},
                {"key": "app", "kind": "app_select", "label": "动作：选择应用"},
                {"key": "command", "kind": "text", "label": "或输入命令",
                 "placeholder": "nautilus（高级）"},
            ],
        }
    else:  # edit
        path, e = args
        spec = {
            "title": "修改快捷键（" + fmt_binding(e.get("binding", "?")) + "）",
            "submit": "保存",
            "fields": [
                {"key": "name", "kind": "text", "label": "名称",
                 "value": e.get("name", "")},
                {"key": "binding", "kind": "hotkey", "label": "快捷键（点击后按下新组合键）",
                 "value": e.get("binding", "")},
                {"key": "app", "kind": "app_select", "label": "动作：选择应用"},
                {"key": "command", "kind": "text", "label": "或输入命令",
                 "value": e.get("command", "")},
            ],
        }
    send({"type": "ui.show", "ui_id": ui_id, "spec": spec})


def on_ui_submit(ui_id, values):
    """表单提交：校验 → 编排 gsettings 写队列。"""
    if not UI_PLAN:
        return
    kind, args = UI_PLAN["kind"], UI_PLAN.get("args")
    qid = UI_PLAN["qid"]

    name = values.get("name", "").strip() or "自定义快捷键"
    binding = values.get("binding", "").strip()
    command = (values.get("command", "").strip()
               or values.get("app", "").strip())

    if not binding:
        send({"type": "capability_request", "id": _next_id(),
              "capability": "notify",
              "params": {"summary": "一呼快捷键", "body": "请先按下快捷键组合"}})
        return
    if not command:
        send({"type": "capability_request", "id": _next_id(),
              "capability": "notify",
              "params": {"summary": "一呼快捷键", "body": "请选择应用或输入命令"}})
        return

    if kind == "edit":
        path, _e = args
        ops = [
            ("settings.write", {"schema": SCHEMA, "key": LIST_KEY,
                                "value": ast_repr([x for x in _all_paths.get(qid, []) if x != path]),
                                "op": "set"}),
            ("settings.write", {"schema": SUB + ":" + path, "key": "name",
                                "value": name, "op": "set"}),
            ("settings.write", {"schema": SUB + ":" + path, "key": "command",
                                "value": command, "op": "set"}),
            ("settings.write", {"schema": SUB + ":" + path, "key": "binding",
                                "value": binding, "op": "set"}),
            ("settings.write", {"schema": SCHEMA, "key": LIST_KEY,
                                "value": ast_repr(_all_paths.get(qid, [])), "op": "set"}),
        ]
        start_queue(qid, ops, "修改快捷键")
    else:
        # 新增：需要先读列表分配 customN——激活时已读（UI_PLAN["paths"]）
        paths = _all_paths.get(qid, [])
        used = {p.rstrip("/").rsplit("custom", 1)[-1] for p in paths}
        n = 0
        while str(n) in used:
            n += 1
        new_path = LIST_PREFIX + "custom" + str(n) + "/"
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
        start_queue(qid, ops, "新增快捷键")


_all_paths = {}  # qid -> paths（edit/add 提交时用）


def _results(qid, rows, payloads=None):
    items = []
    for i, (t, s_) in enumerate(rows):
        p = "!none"
        if payloads:
            p = payloads[i] if i < len(payloads) else "!none"
        items.append({"title": t[:90], "subtitle": s_[:110],
                      "icon": "input-keyboard-symbolic", "payload": p})
    send({"type": "results", "query_id": qid, "items": items})


def show_list(qid, paths, entries):
    _all_paths[qid] = paths
    rows = []
    payloads = []
    for p in paths:
        e = entries.get(p, {})
        rows.append((fmt_binding(e.get("binding", "?")) + " → " + e.get("name", ""),
                     e.get("command", "")))
        payloads.append("edit:" + p)
    rows.append(("➕ 新增快捷键（回车打开表单）", "按下的组合键即绑定 · 动作从应用列表点选"))
    payloads.append("new")
    _results(qid, rows, payloads)


def plan_del(qid, kw, paths, entries):
    hit = None
    for p in paths:
        e = entries.get(p, {})
        blob = (e.get("binding", "") + " " + e.get("name", "") + " "
                + e.get("command", "")).lower()
        if kw in blob:
            hit = (p, e)
            break
    if not hit:
        _results(qid, [("❌ 没有匹配「" + kw + "」的快捷键", "")])
        return
    p, e = hit
    keep = ast_repr([x for x in paths if x != p])
    ops = [("settings.write", {"schema": SCHEMA, "key": LIST_KEY,
                               "value": keep, "op": "set"})]
    for k in ("name", "command", "binding"):
        ops.append(("settings.write", {"schema": SUB + ":" + p, "key": k, "op": "reset"}))
    start_queue(qid, ops, "删除快捷键")


def handle_list_activate(qid, payload):
    """列表行激活：edit:<path> → 预填表单；new → 先读列表再开新增表单
    （新增提交要分配 customN，必须知道现有条目）。"""
    global UI_PLAN
    if payload == "new":
        read_all(qid, _add_form)
    elif payload.startswith("edit:"):
        path = payload[5:]
        read_all(qid, lambda q, ps, es: _edit_form(q, path, ps, es))


def _add_form(qid, paths, entries):
    global UI_PLAN
    _all_paths[qid] = paths
    UI_PLAN = {"qid": qid, "kind": "add", "ui_id": _next_id()}
    ui_form(qid, UI_PLAN["ui_id"], "add", None)


def _edit_form(qid, path, paths, entries):
    global UI_PLAN
    e = entries.get(path, {})
    UI_PLAN = {"qid": qid, "kind": "edit", "args": (path, e), "ui_id": _next_id()}
    _all_paths[qid] = paths
    ui_form(qid, UI_PLAN["ui_id"], "edit", (path, e))


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
        elif t == "ui.event":
            if msg.get("event") == "submit":
                on_ui_submit(msg.get("ui_id"), msg.get("values") or "{}")
        elif t == "query":
            qid = msg.get("id", 0)
            text = msg.get("text", "").strip().lower()
            if text.startswith("key del "):
                read_all(qid, lambda q, ps, es: plan_del(q, text[8:].strip(), ps, es))
            elif text and any(k in text for k in ("hotkey", "快捷键", "kj")):
                read_all(qid, show_list)
        elif t == "activate":
            payload = msg.get("payload") or ""
            if payload.startswith("edit:") or payload == "new":
                handle_list_activate(msg.get("id", 0), payload.lstrip("!"))
        elif t == "shutdown":
            sys.exit(0)


if __name__ == "__main__":
    main()
