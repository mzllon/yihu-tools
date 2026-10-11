#!/usr/bin/python3
"""待办便签（M5 第六批追加）：待办勾选 + Markdown 便签钉桌。

一个插件两种形态，数据同源（/data/quicknotes.json）：
- 待办 = 状态管理：面板列表勾选完成/还原、增删；
- 便签 = 桌面常驻：条目钉成置顶小窗（sticky.pin，宿主 WebKit 渲染
  Markdown），桌面上的编辑自动存回本插件数据目录 stickies/<id>.md，
  下次列表打开即为最新内容；「📌 钉出」对已钉出的条目 = 置前。

面板输 todo / 待办 / 便签 开窗。
"""

import json
import os
import sys
import time

DATA = os.path.join(os.environ.get("HOME", "/data"), "quicknotes.json")
STICKIES = os.path.join(os.environ.get("HOME", "/data"), "stickies")
UI_SEQ = 100


def send(o):
    sys.stdout.write(json.dumps(o, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def err_row(msg):
    return {"title": "✗ " + msg, "subtitle": "处理后重试"}


def load():
    try:
        arr = json.load(open(DATA, encoding="utf-8"))
    except Exception:
        return []
    return [x for x in arr if isinstance(x, dict) and x.get("id") and x.get("text")]


def save(items):
    tmp = DATA + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(items, f, ensure_ascii=False, indent=1)
    os.replace(tmp, DATA)


def new_id():
    return format(time.time_ns(), "x")


def rows_for(items):
    rows = [{"title": "✅ 待办", "subtitle": "回车行内按钮：完成 / 删除"}]
    todos = [x for x in items if x["type"] == "todo"]
    for x in sorted(todos, key=lambda a: (a["done"], -a.get("ts", 0))):
        mark = "✓" if x["done"] else "□"
        toggle = "undo" if x["done"] else "done"
        rows.append({
            "title": f"{mark} {x['text'][:60]}",
            "subtitle": ("已完成" if x["done"] else "进行中") + " · "
                        + time.strftime("%m-%d %H:%M", time.localtime(x.get("ts", 0)))
                        + " · 点按钮操作",
            "actions": [
                {"key": f"{toggle}:{x['id']}", "label": "↺ 还原" if x["done"] else "✓ 完成"},
                {"key": f"del:{x['id']}", "label": "🗑"},
            ],
        })
    if not todos:
        rows.append({"title": "（暂无待办）", "subtitle": "输入内容后点「➕ 加为待办」"})
    rows.append({"title": "📝 便签（Markdown）", "subtitle": "📌 钉出后常驻桌面 · 编辑自动保存"})
    notes = [x for x in items if x["type"] == "note"]
    for x in sorted(notes, key=lambda a: -a.get("ts", 0)):
        preview = x["text"].replace("\n", " ")[:40]
        rows.append({
            "title": f"🗒 {preview}" + ("…" if len(x["text"]) > 40 else ""),
            "subtitle": "钉出中/可再钉 · " + time.strftime("%m-%d %H:%M", time.localtime(x.get("ts", 0))),
            "actions": [
                {"key": f"pin:{x['id']}", "label": "📌 钉出"},
                {"key": f"del:{x['id']}", "label": "🗑"},
            ],
        })
    if not notes:
        rows.append({"title": "（暂无便签）", "subtitle": "输入内容后点「➕ 加为便签」，支持 Markdown"})
    return rows


def ui_spec(ui_id, items, msg_row=None):
    spec = {
        "title": "待办便签",
        "fields": [
            {"key": "kind", "kind": "select", "label": "类型",
             "choices": [{"label": "待办", "value": "todo"},
                          {"label": "便签（Markdown）", "value": "note"}],
             "value": "todo"},
            {"key": "text", "kind": "textarea", "label": "内容（便签支持 Markdown 语法）",
             "value": "", "lines": 3},
        ],
        "actions": [
            {"key": "add-todo", "label": "➕ 加为待办"},
            {"key": "add-note", "label": "📝 加为便签"},
        ],
        "output": {"rows": rows_for(items)},
    }
    return {"type": "ui.show", "ui_id": ui_id, "spec": spec}


def main():
    global UI_SEQ
    for line in sys.stdin:
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        t = msg.get("type")

        if t == "init":
            send({"type": "ready"})

        elif t == "shutdown":
            return

        elif t == "query":
            text = (msg.get("text") or "").strip().lower()
            if text in ("todo", "待办", "便签", "quicknotes", "todos"):
                UI_SEQ += 1
                send(ui_spec(UI_SEQ, load()))

        elif t == "capability_response":
            # sticky.pin / sticky.close 回执：静默成功，失败提示
            UI_SEQ += 1
            if not msg.get("ok"):
                send(ui_spec(UI_SEQ, load(), err_row(str(msg.get("error", "便签操作失败")))))

        elif t == "ui.event":
            ev = msg.get("event") or ""
            values = msg.get("values") or {}
            items = load()

            def refresh(err=None):
                global UI_SEQ
                spec = ui_spec(UI_SEQ, items)
                if err:
                    spec["spec"]["output"] = {"rows": [err_row(err)] + spec["spec"]["output"]["rows"]}
                send(spec)

            if ev in ("action:add-todo", "action:add-note"):
                text = (values.get("text") or "").strip()
                kind = "todo" if ev == "action:add-todo" else "note"
                if not text:
                    refresh("内容为空")
                    continue
                item = {"id": new_id(), "type": kind, "text": text,
                        "done": False, "ts": int(time.time())}
                items.append(item)
                save(items)
                if kind == "note":
                    # 便签内容落盘为 md 文件（钉窗与列表共用）
                    os.makedirs(STICKIES, exist_ok=True)
                    with open(os.path.join(STICKIES, f"{item['id']}.md"), "w",
                              encoding="utf-8") as f:
                        f.write(text)
                refresh()

            elif ev.startswith("action:done:") or ev.startswith("action:undo:"):
                nid = ev.split(":", 2)[2]
                for x in items:
                    if x["id"] == nid and x["type"] == "todo":
                        x["done"] = ev.startswith("action:done:")
                save(items)
                refresh()

            elif ev.startswith("action:pin:"):
                nid = ev.split(":", 2)[2]
                item = next((x for x in items if x["id"] == nid), None)
                if not item:
                    refresh("便签不存在")
                    continue
                os.makedirs(STICKIES, exist_ok=True)
                path = os.path.join(STICKIES, f"{nid}.md")
                if not os.path.exists(path):
                    with open(path, "w", encoding="utf-8") as f:
                        f.write(item["text"])
                # 桌面编辑可能已更新文件——以文件为准刷回列表
                item["text"] = open(path, encoding="utf-8").read()
                save(items)
                send({"type": "capability_request", "id": 6000 + (int(time.time()) % 1000),
                      "capability": "sticky.pin", "params": {"id": nid}})
                refresh()

            elif ev.startswith("action:del:"):
                nid = ev.split(":", 2)[2]
                item = next((x for x in items if x["id"] == nid), None)
                items = [x for x in items if x["id"] != nid]
                save(items)
                if item and item["type"] == "note":
                    p = os.path.join(STICKIES, f"{nid}.md")
                    if os.path.exists(p):
                        os.remove(p)
                    send({"type": "capability_request", "id": 6001,
                          "capability": "sticky.close", "params": {"id": nid}})
                refresh()


if __name__ == "__main__":
    main()
