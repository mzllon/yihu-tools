#!/usr/bin/python3
"""待办插件：列表勾选完成/还原、增删（纯本地，零权限）。

面板输 todo / 待办 / 任务 开窗。数据存插件数据目录 /data/todos.json。
"""

import json
import os
import sys
import time

DATA = os.path.join(os.environ.get("HOME", "/data"), "todos.json")
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


def rows_for(items):
    rows = [{"title": "✅ 待办", "subtitle": "行内按钮：完成 / 还原 / 删除"}]
    todos = sorted(items, key=lambda a: (a["done"], -a.get("ts", 0)))
    for x in todos:
        mark = "✓" if x["done"] else "□"
        toggle = "undo" if x["done"] else "done"
        rows.append({
            "title": f"{mark} {x['text'][:60]}",
            "subtitle": ("已完成" if x["done"] else "进行中") + " · "
                        + time.strftime("%m-%d %H:%M", time.localtime(x.get("ts", 0))),
            "actions": [
                {"key": f"{toggle}:{x['id']}", "label": "↺ 还原" if x["done"] else "✓ 完成"},
                {"key": f"del:{x['id']}", "label": "🗑"},
            ],
        })
    if not todos:
        rows.append({"title": "（暂无待办）", "subtitle": "输入内容后点「➕ 加为待办」"})
    return rows


def ui_spec(ui_id, items, msg_row=None):
    spec = {
        "title": "待办",
        "fields": [
            {"key": "text", "kind": "textarea", "label": "新待办",
             "value": "", "lines": 2},
        ],
        "actions": [{"key": "add", "label": "➕ 加为待办"}],
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
            if text in ("todo", "todos", "待办", "任务"):
                UI_SEQ += 1
                send(ui_spec(UI_SEQ, load()))

        elif t == "ui.event":
            ev = msg.get("event") or ""
            values = msg.get("values") or {}
            items = load()

            def refresh(err=None):
                global UI_SEQ
                UI_SEQ += 1
                spec = ui_spec(UI_SEQ, items)
                if err:
                    spec["spec"]["output"] = {"rows": [err_row(err)]
                                              + spec["spec"]["output"]["rows"]}
                send(spec)

            if ev == "action:add":
                text = (values.get("text") or "").strip()
                if not text:
                    refresh("内容为空")
                    continue
                items.append({"id": format(time.time_ns(), "x"), "text": text,
                              "done": False, "ts": int(time.time())})
                save(items)
                refresh()

            elif ev.startswith("action:done:") or ev.startswith("action:undo:"):
                nid = ev.split(":", 2)[2]
                for x in items:
                    if x["id"] == nid:
                        x["done"] = ev.startswith("action:done:")
                save(items)
                refresh()

            elif ev.startswith("action:del:"):
                nid = ev.split(":", 2)[2]
                items = [x for x in items if x["id"] != nid]
                save(items)
                refresh()


if __name__ == "__main__":
    main()
