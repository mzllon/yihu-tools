#!/usr/bin/python3
"""JSON 工具：复制一段 JSON → 面板输入 json → 格式化/压缩/校验，回车复制结果。

演示 M5 开放 API v2 的 clipboard.read 能力代理回环：插件在沙箱里读不到
剪贴板，向宿主发 capability_request，宿主校验 manifest 声明后以
capability_response.data 回传文本。激活复制由宿主激活语义自动完成
（payload 不带 `!` 前缀 = 回车自动复制），插件自身无需 clipboard.write。

查询触发词：json / json格式化（读剪贴板）；另外内容嗅探——输入本身
形如 JSON（{ 或 [ 开头且可解析）时回推荐行，回车直接把原文格式化/
压缩（activate 带原文，不再绕剪贴板）。
"""

import json
import sys

# 触发词（小写化后精确匹配）
TRIGGERS = {"json", "json格式化", "json 格式化"}

LAST = {}  # qid -> 原文（嗅探推荐行激活时直出结果）


def send(obj):
    sys.stdout.write(json.dumps(obj, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def _parses(text):
    """能否解析为 JSON（嗅探用，不抛异常）。"""
    try:
        json.loads(text)
        return True
    except Exception:
        return False


def rows_for(text):
    """解析剪贴板文本，产出结果行。payload = 要复制回剪贴板的内容。"""
    try:
        obj = json.loads(text)
    except Exception as e:
        # 校验失败也要给出可用信息：定位片段 + 原文回显
        msg = str(e).split("\n")[0]
        return [
            {"title": f"✗ 不是合法 JSON：{msg}", "subtitle": "修正后重试", "payload": "!"},
        ]
    pretty = json.dumps(obj, ensure_ascii=False, indent=2)
    compact = json.dumps(obj, ensure_ascii=False, separators=(",", ":"))
    kind = type(obj).__name__
    rows = [
        {
            "title": "✓ 合法 JSON · 格式化",
            "subtitle": f"{kind} · {len(pretty)} 字符，回车复制",
            "payload": pretty,
        },
        {
            "title": "压缩",
            "subtitle": f"{len(compact)} 字符，回车复制",
            "payload": compact,
        },
    ]
    return rows


def main():
    pending = {}  # request_id -> query_id
    for line in sys.stdin:
        try:
            msg = json.loads(line)
        except Exception:
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
                # 触发：请宿主代读剪贴板（manifest 已声明 clipboard.read）
                rid = 1000 + msg["id"]
                pending[rid] = msg["id"]
                send({
                    "type": "capability_request",
                    "id": rid,
                    "capability": "clipboard.read",
                    "params": {},
                })
            elif text[:1] in ("{", "[") and _parses(text):
                # 内容嗅探：输入本身就是 JSON → 推荐行，回车直出结果
                LAST[qid] = text
                send({
                    "type": "results",
                    "query_id": qid,
                    "items": [{
                        "title": "✓ 检测到 JSON 文本 · 回车格式化",
                        "subtitle": "原文直接格式化/压缩",
                        "payload": "!open",
                    }],
                })

        elif t == "activate":
            if (msg.get("payload") or "") == "open":
                clip = LAST.pop(msg.get("id", 0), "")
                if clip:
                    send({"type": "results", "query_id": msg.get("id", 0),
                          "items": rows_for(clip)})

        elif t == "capability_response":
            rid = msg.get("id")
            qid = pending.pop(rid, None)
            if qid is None:
                continue
            if not msg.get("ok"):
                send({
                    "type": "results",
                    "query_id": qid,
                    "items": [{
                        "title": f"读取剪贴板失败：{msg.get('error', '?')}",
                        "subtitle": "检查 manifest 是否声明 clipboard.read",
                        "payload": "!",
                    }],
                })
                continue
            # data = 宿主 JSON 编码的剪贴板文本
            clip = msg.get("data")
            if not isinstance(clip, str) or not clip.strip():
                send({
                    "type": "results",
                    "query_id": qid,
                    "items": [{
                        "title": "剪贴板是空的或没有文本",
                        "payload": "!",
                    }],
                })
                continue
            send({"type": "results", "query_id": qid, "items": rows_for(clip)})


if __name__ == "__main__":
    main()
