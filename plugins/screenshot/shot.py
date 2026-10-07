#!/usr/bin/env python3
"""截图插件（协议 v1：stdio 行式 JSON）。

搜索「截图 / screenshot / jietu」给出三种截屏方式，回车激活后经宿主
能力代理（screenshot.take）执行——插件进程在沙箱内无显示服务与总线，
截屏由宿主走 xdg-desktop-portal 完成（Wayland 合规路径）：

- full：非交互全屏，保存后系统通知路径；
- full-clip：全屏并复制图片到剪贴板；
- area：打开 GNOME 截图工具（区域/窗口/录屏由用户选择）。

payload 用 `!` 前缀标记动作型（宿主不自动复制其内容）。
完成/失败经 capability_response 异步回包，系统通知兜底告知。
"""

import json
import sys

ROWS = [
    ("全屏截图", "保存到图片目录并通知", "full"),
    ("全屏截图（复制到剪贴板）", "图片进剪贴板，不落盘提示", "full-clip"),
    ("截图 · 区域/窗口/录屏", "打开系统截图工具选择", "area"),
]


def send(o):
    sys.stdout.write(json.dumps(o, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def main():
    seq = 0
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
            if not any(k in text for k in ("截图", "截屏", "screenshot", "jietu")):
                continue
            qid = msg.get("id", 0)
            send({
                "type": "results",
                "query_id": qid,
                "items": [
                    {"title": title, "subtitle": sub, "icon": "camera-photo-symbolic",
                     "payload": "!" + payload}
                    for title, sub, payload in ROWS
                ],
            })
        elif t == "activate":
            payload = (msg.get("payload") or "").lstrip("!")
            mode = "area" if payload == "area" else "full"
            clipboard = payload == "full-clip"
            seq += 1
            send({
                "type": "capability_request",
                "id": seq,
                "capability": "screenshot.take",
                "params": {"mode": mode, "clipboard": clipboard},
            })
            # 结果异步回包；系统通知兜底，插件无需再发结果行
        elif t == "capability_response":
            ok = msg.get("ok")
            if not ok:
                err = msg.get("error", "")
                # 回包失败仅记日志（通知兜底已由宿主负责的取消/超时覆盖场景外）
                sys.stderr.write(f"screenshot: 请求失败: {err}\n")
        elif t == "shutdown":
            sys.exit(0)


if __name__ == "__main__":
    main()
