#!/usr/bin/python3
"""开发者编码工具箱（M5 UI 插件层 v2 首个工具箱插件）。

三条进入路径：
1. 子模块一键直达：codec/编码 开窗；首词命中别名（base / b64 / hash / jwt /
   hex / bin / d2h / 大写 …）带着操作与输入直开直算。
2. 内容推荐：查询全量广播下嗅探输入形态——Base64（解码后须为合法 UTF-8
   文本，压误报）/ JWT（eyJ 三段式）/ 二进制 / HEX——回推荐行，回车由
   activate 带原文打开对应工具。payload 用 `!` 前缀（宿主不复制，且激活
   后宿主延迟收起给回包留生命窗）。
3. 窗内转换：select 选操作 + textarea 贴文本 + 「转换」不关窗原地出结果；
   行内「复制」由宿主直写剪贴板（零权限声明）。
"""

import base64
import hashlib
import json
import re
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

# 首词别名 → 操作（子模块一键直达）
ALIASES = {
    "b64": "b64enc", "b64enc": "b64enc", "base": "b64enc", "base64": "b64enc",
    "b64d": "b64dec", "b64dec": "b64dec",
    "url": "urlenc", "urlenc": "urlenc", "urlencode": "urlenc",
    "urldec": "urldec", "urldecode": "urldec",
    "hash": "hash", "md5": "hash", "sha": "hash", "sha1": "hash", "sha256": "hash",
    "哈希": "hash",
    "hex2dec": "hex2dec", "h2d": "hex2dec",
    "hex": "dec2hex", "dec2hex": "dec2hex", "d2h": "dec2hex",
    "bin": "dec2bin", "dec2bin": "dec2bin", "d2b": "dec2bin",
    "bin2dec": "bin2dec", "b2d": "bin2dec",
    "upper": "upper", "大写": "upper",
    "lower": "lower", "小写": "lower",
    "jwt": "jwt",
    "解码": "b64dec",
}

HINTS = {"codec", "devcodec", "编码", "编码工具箱", "工具箱", "开发者工具"}

UI_SEQ = 100
LAST = {}  # qid -> 原文（嗅探推荐行激活时带原文开窗）


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
            s = text.strip()
            if s.lower().startswith("0x"):
                # 贴了 HEX 进「给我 HEX」操作：自动反向
                return compute("hex2dec", s)
            out = "0x" + format(int(s, 10), "X")
            return [row("十进制 → HEX", "回车或点「复制」", out)]
        if op == "dec2bin":
            s = text.strip()
            if s.lower().startswith("0b"):
                return compute("bin2dec", s)
            out = format(int(s, 10), "b")
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


def sniff(text):
    """内容形态嗅探 → (op, 中文标签) | None。纯函数判断，零 IO。"""
    s = text.strip()
    if not s:
        return None
    # JWT：三段 base64url，首段 eyJ 开头（{" 的 Base64）
    if re.fullmatch(r"eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{4,}\.[A-Za-z0-9_-]*", s):
        return ("jwt", "检测到 JWT 令牌")
    # Base64：字母表匹配 + 试解码须为合法 UTF-8 文本（压掉乱码误报）
    if len(s) >= 16 and re.fullmatch(r"[A-Za-z0-9+/_-]+={0,2}", s):
        t = s.replace("-", "+").replace("_", "/")
        try:
            base64.b64decode(_b64pad(t), validate=True).decode("utf-8")
        except Exception:
            pass
        else:
            return ("b64dec", "检测到 Base64 文本")
    # 二进制：8 位一组
    if re.fullmatch(r"[01]{8,}(?:\s+[01]{8,})*", s):
        return ("bin2dec", "检测到二进制串")
    # HEX：≥8 位且至少含一个 a-f 字母（纯数字交给计算器/时间戳，不抢）
    if re.fullmatch(r"0x[0-9a-fA-F]{4,}|[0-9a-fA-F]{8,}", s) and (
        s.lower().startswith("0x") or re.search(r"[a-fA-F]", s)
    ):
        return ("hex2dec", "检测到 HEX 串")
    return None


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
            qid = msg.get("id", 0)
            text = (msg.get("text") or "").strip()
            low = text.lower()
            # 查询全量广播：只响应本插件词头/形态，其余静默
            if low in HINTS:
                UI_SEQ += 1
                send(ui_spec(UI_SEQ, "b64enc", "", []))
            elif text:
                head, _, tail = text.partition(" ")
                op = ALIASES.get(head.lower().strip())
                if op:
                    UI_SEQ += 1
                    rows = compute(op, tail) if tail.strip() else []
                    send(ui_spec(UI_SEQ, op, tail, rows))
                else:
                    hit = sniff(text)
                    if hit:
                        op, label = hit
                        LAST[qid] = text
                        send({
                            "type": "results",
                            "query_id": qid,
                            "items": [{
                                "title": f"🔑 {label} · 回车打开工具",
                                "subtitle": "内容自动带入对应操作",
                                "icon": "applications-utilities",
                                "payload": "!open:" + op,
                            }],
                        })

        elif t == "ui.event":
            ev = msg.get("event") or ""
            if ev == "action:run":
                values = msg.get("values") or {}
                op = values.get("op", "b64enc")
                text = values.get("input", "")
                rows = compute(op, text)
                send(ui_spec(msg.get("ui_id", 0), op, text, rows))

        elif t == "activate":
            payload = msg.get("payload") or ""
            if payload.startswith("open:"):
                op = payload[5:]
                text = LAST.pop(msg.get("id", 0), "")
                rows = compute(op, text) if text.strip() else []
                UI_SEQ += 1
                send(ui_spec(UI_SEQ, op, text, rows))


if __name__ == "__main__":
    main()
