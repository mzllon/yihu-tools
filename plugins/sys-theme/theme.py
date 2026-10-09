#!/usr/bin/env python3
"""系统主题切换插件（M5 开放 API v2 示例：settings.write 吃狗粮）。

搜索「深色/浅色」给出切换行；回车经宿主能力代理写
org.gnome.desktop.interface color-scheme——插件沙箱内没有
DBus/gsettings，全靠 settings.write@schema 声明制授权：
- manifest 声明 settings.write@org.gnome.desktop.interface
- 中心安装页明示「改系统设置 org.gnome.desktop.interface」
- 宿主逐次校验 schema ∈ 白名单 + 审计留痕
"""

import json
import sys


def send(o):
    sys.stdout.write(json.dumps(o, ensure_ascii=False) + "\n")
    sys.stdout.flush()


ROWS = [
    ("切换到深色模式", "settings.write 经宿主执行", "prefer-dark", "weather-clear-night-symbolic"),
    ("切换到浅色模式", "settings.write 经宿主执行", "default", "weather-clear-symbolic"),
]
SCHEMA = "org.gnome.desktop.interface"
KEY = "color-scheme"
seq = 0
pending = {}


def main():
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
            send({"type": "ready"})
        elif t == "query":
            text = msg.get("text", "").strip().lower()
            items = []
            for title, sub, value, icon in ROWS:
                hay = (title + " dark light 深色 浅色 zhuti theme").lower()
                if not text or text.split()[0] in hay:
                    items.append({"title": title, "subtitle": sub, "icon": icon,
                                  "payload": value})
            if items:
                send({"type": "results", "query_id": msg.get("id", 0), "items": items})
        elif t == "activate":
            value = msg.get("payload") or ""
            if value not in ("prefer-dark", "default"):
                continue
            seq += 1
            pending[seq] = value
            send({"type": "capability_request", "id": seq,
                  "capability": "settings.write",
                  "params": {"schema": SCHEMA, "key": KEY, "value": value}})
        elif t == "capability_response":
            rid = msg.get("id")
            if rid in pending:
                value = pending.pop(rid)
                if not msg.get("ok"):
                    sys.stderr.write(f"sys-theme: 切换失败: {msg.get('error')}\n")
        elif t == "shutdown":
            sys.exit(0)


if __name__ == "__main__":
    main()
