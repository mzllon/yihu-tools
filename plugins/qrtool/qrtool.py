#!/usr/bin/python3
"""二维码工具（M5 第六批）：生成 + 识别。

- 生成：textarea 贴文本 → 宿主 qrcode 纯 Rust 编码 → PNG 自动「钉出」
  置顶窗 + 写剪贴板（宿主自有副作用，插件只发 qr.generate）；
- 识别：剪贴板里的图片 → 宿主 zbarimg 解码 → 文本回传 → 自动复制。
  未装 zbar-tools 时宿主回复装包提示（sudo apt install zbar-tools）。
- 截图选区识别：区域截图覆盖层按 Q（宿主内建，不经过本插件）。

面板输 qr / 二维码 开窗；`qr <文本>` 带内容直开。
"""

import json
import sys

HINTS = {"qr", "qrcode", "二维码"}
UI_SEQ = 100


def send(o):
    sys.stdout.write(json.dumps(o, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def err_row(msg):
    return {"title": "✗ " + msg, "subtitle": "按说明处理后重试"}


def ui_spec(ui_id, text, rows):
    spec = {
        "title": "二维码",
        "fields": [
            {"key": "input", "kind": "textarea", "label": "生成内容（链接/文本/WiFi 配置…）",
             "value": text, "lines": 4},
        ],
        "actions": [
            {"key": "gen", "label": "生成并钉出"},
            {"key": "decode", "label": "识别剪贴板图片"},
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
            text = (msg.get("text") or "").strip()
            low = text.lower()
            if low in HINTS:
                UI_SEQ += 1
                send(ui_spec(UI_SEQ, "", []))
            elif low.startswith(("qr ", "二维码 ")):
                content = text.split(None, 1)[1].strip()
                UI_SEQ += 1
                if content:
                    rid = 5000 + msg.get("id", 0)
                    send({"type": "capability_request", "id": rid,
                          "capability": "qr.generate",
                          "params": {"text": content[:2048]}})
                    send(ui_spec(UI_SEQ, content,
                                 [{"title": "⏳ 生成中…", "subtitle": "生成后自动钉出 + 复制"}]))
                else:
                    send(ui_spec(UI_SEQ, "", []))

        elif t == "capability_response":
            cap = "gen" if False else None
            # 单一 pending 语义：按 id 段区分（5xxx 生成 / 6xxx 识别）
            rid = msg.get("id", 0)
            ok = msg.get("ok")
            data = msg.get("data")
            if 5000 <= rid < 6000:
                UI_SEQ += 1
                if ok:
                    rows = [{"title": "✓ 已生成：贴图窗已钉出，二维码图片已进剪贴板",
                             "subtitle": "拖动可移动 · 滚轮缩放 · 双击复制 · Esc 关"}]
                else:
                    rows = [err_row(str(msg.get("error", "生成失败")))]
                send(ui_spec(UI_SEQ, CURRENT.get("text", ""), rows))
            elif 6000 <= rid < 6400:  # 6400+ = clipboard.write 回执，忽略
                UI_SEQ += 1
                if ok and isinstance(data, str) and data:
                    rows = [
                        {"title": f"✓ 识别成功（{len(data)} 字符）",
                         "subtitle": "已自动复制 · 点「复制」再取一次", "copy": data},
                    ]
                    # 识别结果回写剪贴板
                    send({"type": "capability_request", "id": 6500,
                          "capability": "clipboard.write", "params": {"text": data}})
                else:
                    rows = [err_row(str(msg.get("error", "识别失败")))]
                send(ui_spec(UI_SEQ, CURRENT.get("text", ""), rows))

        elif t == "ui.event":
            ev = msg.get("event") or ""
            values = msg.get("values") or {}
            if ev == "action:gen":
                text = (values.get("input") or "").strip()
                CURRENT["text"] = text
                if not text:
                    UI_SEQ += 1
                    send(ui_spec(UI_SEQ, text, [err_row("输入为空")]))
                else:
                    send({"type": "capability_request", "id": 5001,
                          "capability": "qr.generate", "params": {"text": text[:2048]}})
                    UI_SEQ += 1
                    send(ui_spec(UI_SEQ, text,
                                 [{"title": "⏳ 生成中…", "subtitle": "生成后自动钉出 + 复制"}]))
            elif ev == "action:decode":
                CURRENT["text"] = values.get("input", "")
                send({"type": "capability_request", "id": 6001,
                      "capability": "qr.decode", "params": {}})
                UI_SEQ += 1
                send(ui_spec(UI_SEQ, CURRENT.get("text", ""),
                             [{"title": "⏳ 识别中…", "subtitle": "读取剪贴板图片并解码"}]))


if __name__ == "__main__":
    CURRENT = {"text": ""}
    main()
