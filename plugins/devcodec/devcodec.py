#!/usr/bin/python3
"""开发者编码工具箱（M5 UI 插件层 v2 首个工具箱插件）。

面板输入 codec / 编码 打开工具窗：select 选操作 + textarea 贴文本 +
「转换」按钮（不关窗原地出结果）→ output 结果行，行内「复制」由宿主
直写剪贴板（与列表激活复制同一宿主语义，本插件零权限声明）。

快捷路径：首词命中别名（b64 / hash / jwt / d2h …）即带着操作与输入
直接打开并算好结果。支持多行文本（哈希按整段算）。
"""

import base64
import hashlib
import json
import sys
import urllib.parse

OPS = [
    ("b64enc", "Base64 编码"),
    ("b64dec", "Base64 解码"),
    ("urlenc", "URL 编码"),
    ("urldec", "URL 解码"),
    ("hash", "哈希 MD5 / SHA-1 / SHA-256"),
    ("hex2dec", "HEX → 十进制"),
    ("dec2hex", "十进制 → HEX"),
    ("dec2bin", "十进制 → 二进制"),
    ("bin2dec", "二进制 → 十进制"),
    ("upper", "转大写"),
    ("lower", "转小写"),
    ("jwt", "JWT 解码"),
]

# 首词别名 → 操作（query 快捷路径）
ALIASES = {
    "b64": "b64enc", "b64enc": "b64enc", "base64": "b64enc",
    "b64d": "b64dec", "b64dec": "b64dec",
    "urlenc": "urlenc", "urlencode": "urlenc",
    "urldec": "urldec", "urldecode": "urldec",
    "hash": "hash", "md5": "hash", "sha": "hash", "sha1": "hash", "sha256": "hash",
    "hex2dec": "hex2dec", "h2d": "hex2dec",
    "dec2hex": "dec2hex", "d2h": "dec2hex",
    "dec2bin": "dec2bin", "d2b": "dec2bin",
    "bin2dec": "bin2dec", "b2d": "bin2dec",
    "upper": "upper", "大写": "upper",
    "lower": "lower", "小写": "lower",
    "jwt": "jwt",
}

HINTS = {"codec", "devcodec", "编码", "编码工具箱", "工具箱", "开发者工具"}

UI_SEQ = 100


def send(o):
    sys.stdout.write(json.dumps(o, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def row(title, subtitle, value):
    return {"title": title, "subtitle": subtitle, "copy": value, "preview": value}


def err_row(msg):
    return {"title": "✗ " + msg, "subtitle": "修改输入后重试"}


def _b64pad(s):
    return s + "=" * (-len(s) % 4)


def compute(op, text):
    """执行操作 → 结果行列表。所有失败都转成可用错误行，不让插件崩。"""
    try:
        if op == "b64enc":
            out = base64.b64encode(text.encode("utf-8")).decode("ascii")
            return [row("Base64 编码结果", f"{len(out)} 字符", out)]
        if op == "b64dec":
            out = base64.b64decode(_b64pad(text.strip()), validate=True).decode("utf-8")
            return [row("Base64 解码结果", f"{len(out)} 字符", out)]
        if op == "urlenc":
            out = urllib.parse.quote(text, safe="")
            return [row("URL 编码结果", f"{len(out)} 字符", out)]
        if op == "urldec":
            out = urllib.parse.unquote(text)
            return [row("URL 解码结果", f"{len(out)} 字符", out)]
        if op == "hash":
            b = text.encode("utf-8")
            md5, s1, s256 = (
                hashlib.md5(b).hexdigest(),
                hashlib.sha1(b).hexdigest(),
                hashlib.sha256(b).hexdigest(),
            )
            return [
                row("MD5", "32 位十六进制", md5),
                row("SHA-1", "40 位十六进制", s1),
                row("SHA-256", "64 位十六进制", s256),
            ]
        if op == "hex2dec":
            s = text.strip().lower().removeprefix("0x").replace(" ", "")
            out = str(int(s, 16))
            return [row("HEX → 十进制", "回车或点「复制」", out)]
        if op == "dec2hex":
            out = "0x" + format(int(text.strip(), 10), "X")
            return [row("十进制 → HEX", "回车或点「复制」", out)]
        if op == "dec2bin":
            out = format(int(text.strip(), 10), "b")
            return [row("十进制 → 二进制", "回车或点「复制」", out)]
        if op == "bin2dec":
            s = text.strip().removeprefix("0b").replace(" ", "")
            out = str(int(s, 2))
            return [row("二进制 → 十进制", "回车或点「复制」", out)]
        if op == "upper":
            out = text.upper()
            return [row("转大写", f"{len(out)} 字符", out)]
        if op == "lower":
            out = text.lower()
            return [row("转小写", f"{len(out)} 字符", out)]
        if op == "jwt":
            parts = text.strip().split(".")
            if len(parts) != 3:
                return [err_row("JWT 应为三段式 header.payload.signature")]
            head = json.loads(base64.urlsafe_b64decode(_b64pad(parts[0])))
            payload = json.loads(base64.urlsafe_b64decode(_b64pad(parts[1])))
            hs = json.dumps(head, ensure_ascii=False, indent=2, sort_keys=True)
            ps = json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True)
            note = "未验证签名，仅解码"
            if "exp" in payload:
                note += f" · exp={payload['exp']}"
            return [
                row("Header", note, hs),
                row("Payload", "点击复制 JSON", ps),
            ]
        return [err_row(f"未知操作 {op}")]
    except Exception as e:
        msg = str(e).split("\n")[0]
        return [err_row(f"{msg}（输入不匹配 {op}？）")]


def ui_spec(ui_id, op, text, rows):
    spec = {
        "title": "开发者编码工具箱",
        "fields": [
            {
                "key": "op",
                "kind": "select",
                "label": "操作",
                "choices": [{"label": label, "value": value} for value, label in OPS],
                "value": op,
            },
            {
                "key": "input",
                "kind": "textarea",
                "label": "输入（支持多行，Ctrl+V 粘贴）",
                "value": text,
                "lines": 6,
            },
        ],
        "actions": [{"key": "run", "label": "转换"}],
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
            # 查询全量广播：只响应本插件词头，其余静默（面板/其他插件处理）
            if low in HINTS:
                UI_SEQ += 1
                send(ui_spec(UI_SEQ, "b64enc", "", []))
            else:
                head, _, tail = text.partition(" ")
                op = ALIASES.get(head.lower().strip())
                if op:
                    UI_SEQ += 1
                    rows = compute(op, tail) if tail.strip() else []
                    send(ui_spec(UI_SEQ, op, tail, rows))

        elif t == "ui.event":
            ev = msg.get("event") or ""
            if ev == "action:run":
                values = msg.get("values") or {}
                op = values.get("op", "b64enc")
                text = values.get("input", "")
                rows = compute(op, text)
                send(ui_spec(msg.get("ui_id", 0), op, text, rows))


if __name__ == "__main__":
    main()
