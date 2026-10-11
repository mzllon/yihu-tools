#!/usr/bin/env python3
"""随机数生成器（M5 UI 插件层 v2）：整数 / 浮点 / UUID / 硬币 / 骰子。

面板输入 rand / 随机数 打开工具窗；`rand 1-100` 带范围直开。
整数用 secrets（密码学安全）——抽奖、抽样场景结果不可预测。
「生成」不关窗原地刷新，每行可复制。零权限声明。
"""

import json
import secrets
import sys
import uuid as uuidlib

TYPES = [
    ("int", "整数"),
    ("float", "浮点（2 位小数）"),
    ("uuid", "UUID"),
    ("coin", "抛硬币"),
    ("dice", "掷骰子"),
]

UI_SEQ = 100


def send(o):
    sys.stdout.write(json.dumps(o, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def gen_one(kind, lo, hi):
    if kind == "float":
        a, b = (lo, hi) if lo <= hi else (hi, lo)
        return f"{secrets.randbelow(10_000_001) / 1e7 * (b - a) + a:.2f}"
    if kind == "uuid":
        return str(uuidlib.uuid4())
    if kind == "coin":
        return random_coin()
    if kind == "dice":
        return str(secrets.randbelow(6) + 1)
    # int：范围取整，倒序自动交换
    a, b = int(lo), int(hi)
    if a > b:
        a, b = b, a
    return str(a + secrets.randbelow(b - a + 1))


def random_coin():
    return "正面" if secrets.randbelow(2) else "反面"


def rows_for(kind, lo, hi, count):
    rows = []
    for i in range(max(1, min(20, int(count)))):
        v = gen_one(kind, lo, hi)
        sub = {
            "int": "密码学安全随机 · 点「复制」",
            "float": "0.00–1.00 均匀映射到范围 · 点「复制」",
            "uuid": "UUID v4 · 点「复制」",
            "coin": "点「复制」",
            "dice": "1–6 · 点「复制」",
        }[kind]
        rows.append({"title": v, "subtitle": f"第 {i + 1} 个 · {sub}", "copy": v})
    return rows


def ui_spec(ui_id, lo, hi, count, kind, rows):
    spec = {
        "title": "随机数生成器",
        "fields": [
            {"key": "lo", "kind": "number", "label": "最小值",
             "min": -1_000_000_000, "max": 1_000_000_000, "value": lo},
            {"key": "hi", "kind": "number", "label": "最大值",
             "min": -1_000_000_000, "max": 1_000_000_000, "value": hi},
            {"key": "count", "kind": "number", "label": "生成个数",
             "min": 1, "max": 20, "value": count},
            {"key": "kind", "kind": "select", "label": "类型",
             "choices": [{"label": label, "value": value} for value, label in TYPES],
             "value": kind},
        ],
        "actions": [{"key": "gen", "label": "生成"}],
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
            text = (msg.get("text") or "").strip()
            low = text.lower()
            # 查询全量广播：只响应 rand 词头（rand A-B 带范围、uuid 直达）
            if low in ("rand", "随机数", "random"):
                UI_SEQ += 1
                send(ui_spec(UI_SEQ, 1, 100, 5, "int", []))
            elif low == "uuid":
                UI_SEQ += 1
                send(ui_spec(UI_SEQ, 1, 100, 5, "uuid",
                             rows_for("uuid", 1, 100, 5)))
            elif low.startswith(("rand ", "随机数 ")):
                rest = text.split(None, 1)[1].strip()
                lo, hi, kind = 1, 100, "int"
                if "-" in rest:
                    a, _, b = rest.partition("-")
                    try:
                        lo, hi = int(a.strip()), int(b.strip())
                    except ValueError:
                        lo, hi = 1, 100
                UI_SEQ += 1
                send(ui_spec(UI_SEQ, lo, hi, 5, kind, []))

        elif t == "ui.event":
            ev = msg.get("event") or ""
            if ev == "action:gen":
                values = msg.get("values") or {}
                kind = values.get("kind", "int")
                lo, hi = values.get("lo", 1), values.get("hi", 100)
                count = values.get("count", 5)
                rows = rows_for(kind, lo, hi, count)
                send(ui_spec(msg.get("ui_id", 0), lo, hi, count, kind, rows))


if __name__ == "__main__":
    main()
