#!/usr/bin/python3
"""CSV ⇄ JSON 转换（纯标准库 csv/json）。

面板输 csv 开窗；支持 CSV→JSON（首行表头可选）与 JSON→CSV
（对象数组取键并集）。分隔符可自动识别（, ; Tab 按首行出现次数）。
"""

import csv
import io
import json
import sys

UI_SEQ = 100


def send(o):
    sys.stdout.write(json.dumps(o, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def err_row(msg):
    return {"title": "✗ " + msg, "subtitle": "修正输入后重试"}


def detect_delim(text):
    """按首个非空行的 , ; Tab 计数取最多者（缺省逗号）。"""
    first = next((l for l in text.splitlines() if l.strip()), "")
    counts = {",": first.count(","), ";": first.count(";"), "\t": first.count("\t")}
    best = max(counts, key=counts.get)
    return best if counts[best] > 0 else ","


def csv_to_json(text, delim, header):
    rows = list(csv.reader(io.StringIO(text), delimiter=delim))
    rows = [r for r in rows if any(c.strip() for c in r)]
    if not rows:
        raise ValueError("没有数据行")
    if header:
        keys = [k.strip() or f"col{i}" for i, k in enumerate(rows[0])]
        objs = []
        for r in rows[1:]:
            obj = {}
            for i, k in enumerate(keys):
                obj[k] = r[i] if i < len(r) else ""
            objs.append(obj)
        return json.dumps(objs, ensure_ascii=False, indent=2), len(objs), len(keys)
    return json.dumps(rows, ensure_ascii=False, indent=2), len(rows), len(rows[0])


def json_to_csv(text, header):
    obj = json.loads(text)
    buf = io.StringIO()
    if isinstance(obj, list) and obj and all(isinstance(x, dict) for x in obj):
        keys = []
        for x in obj:
            for k in x:
                if k not in keys:
                    keys.append(k)
        w = csv.writer(buf, lineterminator="\n")
        if header:
            w.writerow(keys)
        for x in obj:
            w.writerow([x.get(k, "") for k in keys])
        return buf.getvalue(), len(obj), len(keys)
    if isinstance(obj, list) and obj and all(isinstance(x, list) for x in obj):
        w = csv.writer(buf, lineterminator="\n")
        for r in obj:
            w.writerow(r)
        return buf.getvalue(), len(obj), len(obj[0]) if obj else 0
    if isinstance(obj, dict):
        w = csv.writer(buf, lineterminator="\n")
        w.writerow(list(obj.keys()))
        w.writerow([json.dumps(v, ensure_ascii=False) if isinstance(v, (list, dict)) else v
                    for v in obj.values()])
        return buf.getvalue(), 1, len(obj)
    raise ValueError("JSON 应为对象数组 / 数组的数组 / 单个对象")


def ui_spec(ui_id, rows):
    return {"type": "ui.show", "ui_id": ui_id, "spec": {
        "title": "CSV ⇄ JSON",
        "fields": [
            {"key": "mode", "kind": "select", "label": "方向",
             "choices": [{"label": "CSV → JSON", "value": "c2j"},
                          {"label": "JSON → CSV", "value": "j2c"}],
             "value": "c2j"},
            {"key": "input", "kind": "textarea", "label": "输入（Ctrl+V 粘贴）",
             "value": "", "lines": 9},
            {"key": "delim", "kind": "select", "label": "分隔符（仅 CSV 侧生效）",
             "choices": [{"label": "自动识别", "value": "auto"},
                          {"label": "逗号 ,", "value": ","},
                          {"label": "分号 ;", "value": ";"},
                          {"label": "Tab", "value": "\t"}],
             "value": "auto"},
            {"key": "header", "kind": "switch", "label": "首行是表头", "value": True},
        ],
        "actions": [{"key": "run", "label": "转换"}],
        "output": {"rows": rows},
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
            low = (msg.get("text") or "").strip().lower()
            if low in ("csv", "csvjson", "csv2json", "json2csv", "转换"):
                UI_SEQ += 1
                send(ui_spec(UI_SEQ, []))

        elif t == "ui.event":
            ev = msg.get("event") or ""
            if ev == "action:run":
                values = msg.get("values") or {}
                mode = values.get("mode", "c2j")
                text = values.get("input", "")
                delim = values.get("delim", "auto")
                header = bool(values.get("header"))
                rows = []
                if not text.strip():
                    rows = [err_row("输入为空")]
                else:
                    try:
                        if delim == "auto":
                            delim = detect_delim(text)
                        if mode == "c2j":
                            out, n_rows, n_cols = csv_to_json(text, delim, header)
                            label = "JSON"
                        else:
                            out, n_rows, n_cols = json_to_csv(text, header)
                            label = "CSV"
                        rows = [
                            {"title": f"✓ {label} · {n_rows} 行 × {n_cols} 列",
                             "subtitle": "点「复制」取全文", "copy": out, "preview": out},
                        ]
                    except Exception as e:
                        rows = [err_row(str(e).splitlines()[0])]
                spec = ui_spec(msg.get("ui_id", 0), rows)
                # 保留用户输入，不重置
                for f in spec["spec"]["fields"]:
                    if f["key"] == "input":
                        f["value"] = text
                    elif f["key"] == "mode":
                        f["value"] = mode
                    elif f["key"] == "delim":
                        f["value"] = delim
                    elif f["key"] == "header":
                        f["value"] = header
                send(spec)


if __name__ == "__main__":
    main()
