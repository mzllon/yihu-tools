#!/usr/bin/env python3
"""沙箱探针插件（协议 v0：stdio 行式 JSON）。

任意 query 触发一组边界自检：每项检查「沙箱下应有的行为」，
符合预期 → 标题 ✅，不符合（越权成功）→ 标题 ❌。激活=复制详情。

M4 安全收口的验收工具（docs/插件基座安全模型与发布策略.md §6.2）：
静态审计不能证明隔离有效，探针在真实运行环境里验证边界。
"""

import json
import os
import socket
import sys

OK, BAD = "✅", "❌"

# 已发出的能力请求：request_id → (query_id, 预期结果)
#   expect="grant" —— 已声明能力（clipboard.write），宿主应授权
#   expect="deny"  —— 未声明能力（notify），宿主应拒绝
pending = {}


def check(name, should, run):
    """执行一项检查：run() 返回 (预期成立?, 详情)。永不抛出。"""
    try:
        ok, detail = run()
    except Exception as e:  # 预期中的失败（PermissionError/FileNotFoundError…）
        ok, detail = should == "deny", f"{type(e).__name__}: {e}"
    title = f"{OK if ok else BAD} {name}"
    return {
        "title": title,
        "subtitle": detail[:120],
        "icon": "security-high-symbolic" if ok else "dialog-error-symbolic",
        "payload": f"{name}｜{detail}",
    }


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
        if msg.get("type") == "init":
            out.write(json.dumps({"type": "ready"}) + "\n")
            out.flush()
        elif msg.get("type") == "query":
            text = msg.get("text", "")
            qid = msg.get("id", 0)
            if text.startswith("copy:"):
                # 已声明能力：宿主应授权并真实写入剪贴板
                _request(out, qid, "clipboard.write", {"text": text[5:]}, "grant",
                         "宿主写剪贴板（已声明）")
            elif text.startswith("notify:"):
                # 未声明能力：宿主应拒绝（manifest 只声明 clipboard.write）
                _request(out, qid, "notify", {"summary": "探针越权"}, "deny",
                         "桌面通知（未声明）")
            else:
                _checklist(out, qid)
        elif msg.get("type") == "capability_response":
            req = pending.pop(msg.get("id"), None)
            if req is None:
                continue
            qid, expect, name = req
            ok = bool(msg.get("ok"))
            if expect == "grant":
                good, detail = ok, (msg.get("error") or "已由宿主执行")
                if ok:
                    detail = "宿主已执行（剪贴板可粘贴验证）"
            else:
                good = not ok
                detail = msg.get("error") or "被宿主拒绝"
                if not ok:
                    detail = f"宿主拒绝：{detail}"
            title = f"{OK if good else BAD} 能力代理：{name}"
            out.write(
                json.dumps(
                    {"type": "results", "query_id": qid,
                     "items": [{"title": title, "subtitle": detail[:120],
                                "icon": "security-high-symbolic" if good else "dialog-error-symbolic",
                                "payload": f"{name}｜{detail}"}]},
                    ensure_ascii=False,
                )
                + "\n"
            )
            out.flush()
        elif msg.get("type") == "activate":
            pass  # v0 激活由宿主完成（复制 payload）


def _request(out, qid, capability, params, expect, name):
    """发起能力请求；结果在 capability_response 回来后给出。"""
    rid = 9000 + len(pending) + 1
    while rid in pending:
        rid += 1
    pending[rid] = (qid, expect, name)
    out.write(
        json.dumps({"type": "capability_request", "id": rid,
                    "capability": capability, "params": params},
                   ensure_ascii=False)
        + "\n"
    )
    out.flush()


def _checklist(out, qid):
    items = [
        check("读插件目录（应可读）", "allow", lambda: _readable("/plugin/manifest.toml")),
        check("写插件目录（应被拒）", "deny", _write_plugin_dir),
        check("写数据目录（应可写）", "allow", _write_data_dir),
        check("读宿主家目录（应不可见）", "deny", lambda: _listable("/home")),
        check("读 /etc/shadow（应不可见）", "deny", lambda: _readable("/etc/shadow")),
        check("联网（应被隔离）", "deny", _net),
        check("连宿主 D-Bus（应被隔离）", "deny", _dbus),
        check("直接写剪贴板 xclip（应失败）", "deny", _xclip),
    ]
    out.write(
        json.dumps(
            {"type": "results", "query_id": qid, "items": items},
            ensure_ascii=False,
        )
        + "\n"
    )
    out.flush()


def _xclip():
    import subprocess
    r = subprocess.run(["xclip", "-selection", "clipboard"], input=b"x",
                       capture_output=True, timeout=3)
    if r.returncode == 0:
        return False, "xclip 写剪贴板成功——能力代理被绕过！"
    return True, "无 xclip/无显示服务，绕过路径不可用"


def _readable(path):
    with open(path, "rb") as f:
        n = len(f.read(4096))
    return True, f"可读 {path}（前 {n} 字节）"


def _write_plugin_dir():
    p = "/plugin/.probe-write-test"
    with open(p, "w") as f:
        f.write("x")
    os.unlink(p)
    return False, "写入插件目录成功——插件目录不是只读，沙箱白名单失效！"


def _write_data_dir():
    p = os.path.join(os.environ.get("HOME", "/data"), ".probe-write-test")
    with open(p, "w") as f:
        f.write("ok")
    os.unlink(p)
    return True, f"数据目录可写（HOME={os.environ.get('HOME')}）"


def _listable(path):
    names = os.listdir(path)
    return not names, f"列出 {path}: {names}" if names else f"{path} 为空（未挂载宿主目录）"


def _net():
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.settimeout(1.5)
    try:
        s.connect(("1.1.1.1", 80))
        return False, "外连 1.1.1.1:80 成功——网络未隔离！"
    finally:
        s.close()


def _dbus():
    uid = os.getuid()
    path = f"/run/user/{uid}/bus"
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(1.5)
    try:
        s.connect(path)
        return False, f"连接 {path} 成功——宿主总线未隔离！"
    finally:
        s.close()


if __name__ == "__main__":
    main()
