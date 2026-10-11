#!/usr/bin/env python3
"""随机密码生成器（M5 UI 插件层 v2，取代 0.1 的纯结果行交互）。

面板输入 pass / 密码 打开工具窗：长度与个数数字框、四类字符集开关、
风格下拉（随机字符 / 数字 PIN / 易读短语）→「生成」不关窗原地刷新。
每类选中字符集至少贡献一位（Fisher–Yates 用 secrets 洗牌）；
易读短语 = 音节词 + 数字尾缀，好念好口头转述。
行内「复制」由宿主直写剪贴板（零权限声明）。`pass 20` 预填长度。
"""

import json
import secrets
import string
import sys

SETS = [
    ("upper", string.ascii_uppercase),
    ("lower", string.ascii_lowercase),
    ("digit", string.digits),
    ("sym", "!@#$%^&*"),
]
STYLES = [
    ("random", "随机字符"),
    ("pin", "数字 PIN"),
    ("phrase", "易读短语"),
]
CONS = "bcdfghjklmnpqrstvwxz"
VOW = "aeiou"

UI_SEQ = 100


def send(o):
    sys.stdout.write(json.dumps(o, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def shuffle(chars):
    """Fisher–Yates，secrets 洗牌。"""
    for i in range(len(chars) - 1, 0, -1):
        j = secrets.randbelow(i + 1)
        chars[i], chars[j] = chars[j], chars[i]
    return chars


def gen_random(length, on_sets):
    pools = [p for key, p in SETS if on_sets.get(key)]
    if not pools:
        return None
    length = max(length, len(pools))  # 每类至少一位
    chars = [secrets.choice(p) for p in pools]
    pool = "".join(pools)
    chars += [secrets.choice(pool) for _ in range(length - len(chars))]
    return "".join(shuffle(chars))


def gen_pin(length):
    return "".join(secrets.choice(string.digits) for _ in range(max(4, length)))


def gen_phrase(length, on_sets):
    """音节词（辅元辅，首字母大写）+ 数字尾缀；好念、口头转述不拼错。"""
    target = max(8, length)
    syllables = []
    total = 0
    while total < target:
        syl = secrets.choice(CONS) + secrets.choice(VOW) + secrets.choice(CONS)
        syllables.append(syl.capitalize())
        total += 3
    word = "-".join(syllables)
    tail = "".join(secrets.choice(string.digits) for _ in range(2))
    if on_sets.get("sym"):
        tail += secrets.choice("!@#$%^&*")
    return word + "-" + tail


def rows_for(style, length, count, on_sets):
    if style == "random" and not any(on_sets.values()):
        return [{"title": "✗ 至少选择一类字符", "subtitle": "打开上方字符集开关后重试"}]
    label = dict(STYLES)[style]
    rows = []
    for i in range(max(1, min(10, int(count)))):
        if style == "pin":
            pwd = gen_pin(length)
            sub = f"{len(pwd)} 位数字"
        elif style == "phrase":
            pwd = gen_phrase(length, on_sets)
            sub = "音节短语 · 长度近似"
        else:
            pwd = gen_random(length, on_sets)
            sub = f"{length} 位 · 每类字符至少一位"
        rows.append({"title": pwd, "subtitle": f"{label} · {sub} · 点「复制」", "copy": pwd})
    return rows


def ui_spec(ui_id, length, count, style, on_sets, rows):
    spec = {
        "title": "随机密码生成器",
        "fields": [
            {"key": "length", "kind": "number", "label": "长度",
             "min": 6, "max": 64, "value": length},
            {"key": "count", "kind": "number", "label": "生成个数",
             "min": 1, "max": 10, "value": count},
            {"key": "style", "kind": "select", "label": "风格",
             "choices": [{"label": label, "value": value} for value, label in STYLES],
             "value": style},
            {"key": "upper", "kind": "switch", "label": "大写字母 A-Z", "value": on_sets["upper"]},
            {"key": "lower", "kind": "switch", "label": "小写字母 a-z", "value": on_sets["lower"]},
            {"key": "digit", "kind": "switch", "label": "数字 0-9", "value": on_sets["digit"]},
            {"key": "sym", "kind": "switch", "label": "符号 !@#$%^&*", "value": on_sets["sym"]},
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
            # 查询全量广播：只响应 pass 词头（pass 20 预填长度）
            if low in ("pass", "密码", "passgen"):
                UI_SEQ += 1
                send(ui_spec(UI_SEQ, 16, 3, "random",
                             {"upper": True, "lower": True, "digit": True, "sym": True}, []))
            elif low.startswith(("pass ", "密码 ")):
                rest = text.split(None, 1)[1].strip()
                length = 16
                if rest.isdigit():
                    length = max(6, min(64, int(rest)))
                UI_SEQ += 1
                send(ui_spec(UI_SEQ, length, 3, "random",
                             {"upper": True, "lower": True, "digit": True, "sym": True}, []))

        elif t == "ui.event":
            ev = msg.get("event") or ""
            if ev == "action:gen":
                values = msg.get("values") or {}
                length = int(values.get("length", 16))
                count = int(values.get("count", 3))
                style = values.get("style", "random")
                on_sets = {k: bool(values.get(k)) for k in ("upper", "lower", "digit", "sym")}
                rows = rows_for(style, length, count, on_sets)
                send(ui_spec(msg.get("ui_id", 0), length, count, style, on_sets, rows))


if __name__ == "__main__":
    main()
