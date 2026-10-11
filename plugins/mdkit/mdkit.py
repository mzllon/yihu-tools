#!/usr/bin/python3
"""Markdown 工具（M5 第六批）：编辑 + 浏览器预览 + 转 HTML + 笔记保存。

- textarea 编辑 →「浏览器预览」宿主 pulldown-cmark 渲染成完整 HTML 页
  写临时文件并打开（CommonMark + 表格/删除线/任务列表，源码安全转义）；
- 「转 HTML」渲染结果直接复制；「保存」写插件数据目录 /data/notes/，
  列表行可随时载入/删除；
- 面板输 md 开窗；`md <文件名>` 载入已有笔记。
"""

import json
import os
import re
import sys
import time

NOTES_DIR = os.path.join(os.environ.get("HOME", "/data"), "notes")
UI_SEQ = 100


def send(o):
    sys.stdout.write(json.dumps(o, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def err_row(msg):
    return {"title": "✗ " + msg, "subtitle": "处理后重试"}


def list_notes():
    os.makedirs(NOTES_DIR, exist_ok=True)
    names = sorted(
        (f for f in os.listdir(NOTES_DIR) if f.endswith(".md")),
        key=lambda f: -os.path.getmtime(os.path.join(NOTES_DIR, f)),
    )
    return names


def safe_name(name):
    name = (name or "").strip()
    name = re.sub(r"[\\/:*?\"<>|]", "", name)[:60]
    if not name:
        name = time.strftime("notes-%Y%m%d-%H%M%S")
    if not name.endswith(".md"):
        name += ".md"
    return name


def note_rows():
    rows = []
    for f in list_notes():
        rows.append({
            "title": f[:-3],
            "subtitle": time.strftime("%Y-%m-%d %H:%M", time.localtime(
                os.path.getmtime(os.path.join(NOTES_DIR, f)))),
            "actions": [
                {"key": f"load:{f[:-3]}", "label": "📂 载入"},
                {"key": f"del:{f[:-3]}", "label": "🗑 删"},
            ],
        })
    return rows


def ui_spec(ui_id, name, text, rows):
    spec = {
        "title": "Markdown 工具",
        "fields": [
            {"key": "name", "kind": "text", "label": "笔记名（保存用，可留空自动命名）",
             "value": name, "placeholder": "如 meeting-1011"},
            {"key": "text", "kind": "textarea", "label": "Markdown（Ctrl+V 粘贴）",
             "value": text, "lines": 12},
        ],
        "actions": [
            {"key": "preview", "label": "🌐 浏览器预览"},
            {"key": "tohtml", "label": "📋 转 HTML"},
            {"key": "save", "label": "💾 保存"},
        ],
    }
    if rows is not None:
        spec["output"] = {"rows": rows}
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
            text = (msg.get("text") or "").strip()
            low = text.lower()
            if low in ("md", "markdown"):
                UI_SEQ += 1
                send(ui_spec(UI_SEQ, "", "", note_rows()))
            elif low.startswith(("md ", "markdown ")):
                name = text.split(None, 1)[1].strip()
                path = os.path.join(NOTES_DIR, safe_name(name))
                content = ""
                if os.path.isfile(path):
                    content = open(path, encoding="utf-8").read()
                UI_SEQ += 1
                send(ui_spec(UI_SEQ, safe_name(name)[:-3], content, note_rows()))

        elif t == "capability_response":
            rid = msg.get("id", 0)
            ok = msg.get("ok")
            UI_SEQ += 1
            if ok:
                data = msg.get("data") or ""
                if data:
                    rows = [{"title": "✓ HTML 已渲染", "subtitle": "点「复制」取全文",
                             "copy": data, "preview": ""}]
                else:
                    rows = [{"title": "✓ 已在浏览器打开预览",
                             "subtitle": "修改后可再次预览"}]
            else:
                rows = [err_row(str(msg.get("error", "渲染失败")))]
            send(ui_spec(UI_SEQ, CURRENT.get("name", ""), CURRENT.get("text", ""), rows))

        elif t == "ui.event":
            ev = msg.get("event") or ""
            values = msg.get("values") or {}
            name = (values.get("name") or "").strip()
            text = values.get("text", "")
            CURRENT["name"], CURRENT["text"] = name, text

            if ev == "action:preview" or ev == "action:tohtml":
                if not text.strip():
                    UI_SEQ += 1
                    send(ui_spec(UI_SEQ, name, text, [err_row("内容为空")]))
                    continue
                mode = "preview" if ev == "action:preview" else "html"
                send({"type": "capability_request", "id": 5100,
                      "capability": "md.render",
                      "params": {"mode": mode, "text": text[:1048576]}})
                UI_SEQ += 1
                send(ui_spec(UI_SEQ, name, text,
                             [{"title": "⏳ 渲染中…", "subtitle": "pulldown-cmark 宿主代渲染"}]))

            elif ev == "action:save":
                fname = safe_name(name)
                os.makedirs(NOTES_DIR, exist_ok=True)
                path = os.path.join(NOTES_DIR, fname)
                tmp = path + ".tmp"
                with open(tmp, "w", encoding="utf-8") as f:
                    f.write(text)
                os.replace(tmp, path)
                UI_SEQ += 1
                send(ui_spec(UI_SEQ, fname[:-3], text,
                             [{"title": f"✓ 已保存 {fname}（{len(text)} 字符）",
                               "subtitle": "列表行可载入/删除"}] + note_rows()))

            elif ev.startswith("action:load:"):
                fname = safe_name(ev[len("action:load:"):])
                path = os.path.join(NOTES_DIR, fname)
                content = ""
                if os.path.isfile(path):
                    content = open(path, encoding="utf-8").read()
                UI_SEQ += 1
                send(ui_spec(UI_SEQ, fname[:-3], content, note_rows()))

            elif ev.startswith("action:del:"):
                fname = safe_name(ev[len("action:del:"):])
                path = os.path.join(NOTES_DIR, fname)
                if os.path.isfile(path):
                    os.remove(path)
                UI_SEQ += 1
                send(ui_spec(UI_SEQ, name, text, note_rows()))


if __name__ == "__main__":
    CURRENT = {"name": "", "text": ""}
    main()
