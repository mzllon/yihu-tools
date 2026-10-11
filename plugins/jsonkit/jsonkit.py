#!/usr/bin/python3
"""JSON 工具（UI 版，M5 UI 插件层 v2）：格式化 / 压缩 / 校验 / 搜索。

三条进入路径：
1. 面板输 json / json格式化 → 宿主代读剪贴板（clipboard.read 能力代理）
   → 开窗预填原文（能解析则直接带格式化结果，失败则带校验错误行）；
2. 内容嗅探：输入本身形如 JSON（{ [ 开头且可解析）→ 推荐行，回车带原文开窗；
3. 窗内：textarea 贴 JSON + 「格式化」「压缩」「搜索」不关窗原地出结果。
   校验贯穿其中——解析失败给 ✗ 行带解析器出错信息。

搜索：按 key/value 子串匹配（大小写不敏感），列命中路径（$.users[0].name
记法，非常标识符键用 ["..."]）与值预览，行内「复制」取值；key 命中的
对象/数组复制整个子树（pretty JSON）。
"""

import json
import re
import sys

TRIGGERS = {"json", "json格式化", "json 格式化"}
MAX_SEARCH_ROWS = 50

UI_SEQ = 100
LAST = {}    # qid -> 原文（嗅探推荐行激活时带原文开窗）
pending = {}  # capability request id -> qid


def send(obj):
    sys.stdout.write(json.dumps(obj, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def row(title, subtitle, value):
    return {"title": title, "subtitle": subtitle, "copy": value, "preview": value}


def err_row(msg):
    return {"title": "✗ " + msg, "subtitle": "修正后重试"}


def _parses(text):
    try:
        json.loads(text)
        return True
    except Exception:
        return False


def rows_fmt(text):
    try:
        obj = json.loads(text)
    except Exception as e:
        return [err_row(f"不是合法 JSON：{str(e).splitlines()[0]}")]
    pretty = json.dumps(obj, ensure_ascii=False, indent=2)
    kind = type(obj).__name__
    return [row("格式化 JSON", f"{kind} · {len(pretty)} 字符 · 点「复制」取全文", pretty)]


def rows_compact(text):
    try:
        obj = json.loads(text)
    except Exception as e:
        return [err_row(f"不是合法 JSON：{str(e).splitlines()[0]}")]
    compact = json.dumps(obj, ensure_ascii=False, separators=(",", ":"))
    return [row("压缩 JSON", f"{len(compact)} 字符 · 点「复制」取全文", compact)]


def _path_join(base, key):
    if isinstance(key, str) and re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", key):
        return f"{base}.{key}"
    return f"{base}[{json.dumps(key, ensure_ascii=False)}]"


def _display(v):
    """命中项的值预览（subtitle 用）。"""
    if isinstance(v, str):
        return v
    if isinstance(v, dict):
        return f"{{…}} {len(v)} 个键"
    if isinstance(v, list):
        return f"[…] {len(v)} 项"
    return json.dumps(v)


def _copy_text(v):
    """命中项的复制内容：标量取值本身，容器取整个子树（pretty）。"""
    if isinstance(v, str):
        return v
    if isinstance(v, (dict, list)):
        return json.dumps(v, ensure_ascii=False, indent=2)
    return json.dumps(v)


def rows_search(text, q):
    try:
        obj = json.loads(text)
    except Exception as e:
        return [err_row(f"不是合法 JSON：{str(e).splitlines()[0]}")]
    q_low = (q or "").strip().lower()
    if not q_low:
        return [err_row("输入搜索关键字（key 或值的一部分）")]

    hits = []  # (path, value)

    def walk(node, path):
        if len(hits) > MAX_SEARCH_ROWS:
            return
        if isinstance(node, dict):
            for k, v in node.items():
                p = _path_join(path, k)
                if q_low in str(k).lower():
                    hits.append((p, v))
                walk(v, p)
        elif isinstance(node, list):
            for i, v in enumerate(node):
                walk(v, f"{path}[{i}]")
        else:
            sv = _display(node)
            if q_low in sv.lower():
                hits.append((path, node))

    walk(obj, "$")
    if not hits:
        return [err_row(f"没有匹配「{q.strip()}」的 key 或值")]
    overflow = len(hits) > MAX_SEARCH_ROWS
    rows = []
    for p, v in hits[:MAX_SEARCH_ROWS]:
        disp = _display(v)
        if len(disp) > 70:
            disp = disp[:70] + "…"
        rows.append({
            "title": p,
            "subtitle": f"{disp} · 点「复制」取值",
            "copy": _copy_text(v),
        })
    if overflow:
        rows.append({"title": f"… 匹配过多，仅显示前 {MAX_SEARCH_ROWS} 条",
                     "subtitle": "细化关键字后重试"})
    return rows


def ui_spec(ui_id, text, rows):
    spec = {
        "title": "JSON 工具",
        "fields": [
            {
                "key": "input",
                "kind": "textarea",
                "label": "JSON（Ctrl+V 粘贴）",
                "value": text,
                "lines": 8,
            },
            {
                "key": "q",
                "kind": "text",
                "label": "搜索 key 或值（子串，大小写不敏感）",
                "placeholder": "如 name、42、true",
            },
        ],
        "actions": [
            {"key": "fmt", "label": "格式化"},
            {"key": "compact", "label": "压缩"},
            {"key": "search", "label": "搜索"},
        ],
    }
    if rows:
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
            qid = msg.get("id", 0)
            text = (msg.get("text") or "").strip()
            if text.lower() in TRIGGERS:
                # 剪贴板路径：宿主代读 → 开窗预填（声明 clipboard.read）
                rid = 1000 + qid
                pending[rid] = qid
                send({
                    "type": "capability_request",
                    "id": rid,
                    "capability": "clipboard.read",
                    "params": {},
                })
            elif text[:1] in ("{", "[") and _parses(text):
                # 内容嗅探：输入本身就是 JSON → 推荐行，回车带原文开窗
                LAST[qid] = text
                send({
                    "type": "results",
                    "query_id": qid,
                    "items": [{
                        "title": "✓ 检测到 JSON 文本 · 回车打开工具",
                        "subtitle": "原文带入格式化/压缩/搜索",
                        "payload": "!open",
                    }],
                })

        elif t == "capability_response":
            rid = msg.get("id")
            qid = pending.pop(rid, None)
            if qid is None:
                continue
            clip = msg.get("data") if msg.get("ok") else None
            clip = clip if isinstance(clip, str) and clip.strip() else ""
            UI_SEQ += 1
            # 预填原文：能解析直接带格式化结果；失败则带校验错误行（或空板提示）
            rows = rows_fmt(clip) if clip else [err_row("剪贴板是空的或没有文本")]
            send(ui_spec(UI_SEQ, clip, rows))

        elif t == "ui.event":
            ev = msg.get("event") or ""
            values = msg.get("values") or {}
            text = values.get("input", "")
            if ev == "action:fmt":
                rows = rows_fmt(text)
            elif ev == "action:compact":
                rows = rows_compact(text)
            elif ev == "action:search":
                rows = rows_search(text, values.get("q", ""))
            else:
                continue
            send(ui_spec(msg.get("ui_id", 0), text, rows))

        elif t == "activate":
            if (msg.get("payload") or "") == "open":
                clip = LAST.pop(msg.get("id", 0), "")
                UI_SEQ += 1
                rows = rows_fmt(clip) if clip.strip() else []
                send(ui_spec(UI_SEQ, clip, rows))


if __name__ == "__main__":
    main()
