#!/usr/bin/python3
"""便签插件（Markdown 格式）：列表管理 + 钉成桌面置顶小窗。

- 便签内容 = 本插件数据目录 stickies/<id>.md（Markdown 文件）；
- 「📌 钉出」→ 宿主 WebKit 便签窗（所见即所得），桌面编辑自动存回同一
  文件，列表打开即为最新内容；重复钉出 = 置前已开的窗；
- 删除 = 删记录 + 删文件 + 关窗（sticky.close）。

面板输 sticky / 便签 / note 开窗。
"""

import json
import os
import sys
import time

DATA = os.path.join(os.environ.get("HOME", "/data"), "notes.json")
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
    return [x for x in arr if isinstance(x, dict) and x.get("id")]


def save(items):
    tmp = DATA + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(items, f, ensure_ascii=False, indent=1)
    os.replace(tmp, DATA)


def note_path(nid):
    return os.path.join(STICKIES, f"{nid}.md")


def rows_for(items):
    rows = [{"title": "📝 便签（Markdown）",
             "subtitle": "📌 钉出后常驻桌面 · 桌面编辑自动保存"}]
    for x in sorted(items, key=lambda a: -a.get("ts", 0)):
        preview = x["text"].replace("\n", " ")[:40]
        rows.append({
            "title": f"🗒 {preview}" + ("…" if len(x["text"]) > 40 else ""),
            "subtitle": time.strftime("%m-%d %H:%M", time.localtime(x.get("ts", 0)))
                        + " · 钉出中/可再钉",
            "actions": [
                {"key": f"pin:{x['id']}", "label": "📌 钉出"},
                {"key": f"del:{x['id']}", "label": "🗑"},
            ],
        })
    if not items:
        rows.append({"title": "（暂无便签）",
                     "subtitle": "输入内容后点「➕ 新建便签」，支持 Markdown"})
    return rows


def ui_spec(ui_id, items, msg_row=None):
    return {"type": "ui.show", "ui_id": ui_id, "spec": {
        "title": "便签",
        "fields": [
            {"key": "text", "kind": "textarea", "label": "新便签（Markdown 语法）",
             "value": "", "lines": 4},
        ],
        "actions": [{"key": "add", "label": "➕ 新建便签"}],
        "output": {"rows": rows_for(items)},
    }}


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
            if text in ("sticky", "stickies", "便签", "note", "notes"):
                UI_SEQ += 1
                send(ui_spec(UI_SEQ, load()))

        elif t == "capability_response":
            # sticky.pin/close 回执：成功静默，失败提示
            if not msg.get("ok"):
                UI_SEQ += 1
                send(ui_spec(UI_SEQ, load(),
                             err_row(str(msg.get("error", "便签操作失败")))))

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
                nid = format(time.time_ns(), "x")
                os.makedirs(STICKIES, exist_ok=True)
                with open(note_path(nid), "w", encoding="utf-8") as f:
                    f.write(text)
                items.append({"id": nid, "text": text, "ts": int(time.time())})
                save(items)
                refresh()

            elif ev.startswith("action:pin:"):
                nid = ev.split(":", 2)[2]
                path = note_path(nid)
                item = next((x for x in items if x["id"] == nid), None)
                if not os.path.exists(path):
                    # 文件缺失（被外部清理）→ 以记录内容重建
                    if item:
                        os.makedirs(STICKIES, exist_ok=True)
                        with open(path, "w", encoding="utf-8") as f:
                            f.write(item["text"])
                    else:
                        refresh("便签不存在")
                        continue
                # 桌面编辑可能已更新文件——以文件为准刷回列表
                if item:
                    item["text"] = open(path, encoding="utf-8").read()
                    item["ts"] = int(os.path.getmtime(path))
                    save(items)
                send({"type": "capability_request", "id": 6000 + (int(time.time()) % 1000),
                      "capability": "sticky.pin", "params": {"id": nid}})
                refresh()

            elif ev.startswith("action:del:"):
                nid = ev.split(":", 2)[2]
                items = [x for x in items if x["id"] != nid]
                save(items)
                path = note_path(nid)
                if os.path.exists(path):
                    os.remove(path)
                send({"type": "capability_request", "id": 6001,
                      "capability": "sticky.close", "params": {"id": nid}})
                refresh()


if __name__ == "__main__":
    main()
