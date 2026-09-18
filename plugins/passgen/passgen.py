#!/usr/bin/env python3
"""密码生成插件（协议 v0：stdio 行式 JSON）。

查询：`pass 20` / `密码 32` → 生成 3 个指定长度的随机密码（激活=复制）。
"""

import json
import secrets
import string
import sys

ALPHABET = string.ascii_letters + string.digits + "!@#$%^&*"


def gen(n: int) -> str:
    return "".join(secrets.choice(ALPHABET) for _ in range(n))


def main() -> None:
    out = sys.stdout
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        t = msg.get("type")
        if t == "init":
            out.write(json.dumps({"type": "ready"}) + "\n")
            out.flush()
        elif t == "query":
            text = (msg.get("text") or "").strip()
            n = 16
            for tok in text.split():
                if tok.isdigit():
                    n = max(8, min(64, int(tok)))
            items = [
                {
                    "title": gen(n),
                    "subtitle": f"{n} 位密码 · 回车复制",
                    "icon": "",
                    "payload": pwd,
                }
                for pwd in (gen(n) for _ in range(3))
            ]
            out.write(
                json.dumps(
                    {"type": "results", "query_id": msg.get("id", 0), "items": items},
                    ensure_ascii=False,
                )
                + "\n"
            )
            out.flush()
        elif t == "activate":
            pass  # v0 激活由宿主完成（复制 payload）


if __name__ == "__main__":
    main()
