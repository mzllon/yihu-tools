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
            items = [
                check("读插件目录（应可读）", "allow", lambda: _readable("/plugin/manifest.toml")),
                check("写插件目录（应被拒）", "deny", _write_plugin_dir),
                check("写数据目录（应可写）", "allow", _write_data_dir),
                check("读宿主家目录（应不可见）", "deny", lambda: _listable("/home")),
                check("读 /etc/shadow（应不可见）", "deny", lambda: _readable("/etc/shadow")),
                check("联网（应被隔离）", "deny", _net),
                check("连宿主 D-Bus（应被隔离）", "deny", _dbus),
            ]
            out.write(
                json.dumps(
                    {"type": "results", "query_id": msg.get("id", 0), "items": items},
                    ensure_ascii=False,
                )
                + "\n"
            )
            out.flush()
        elif msg.get("type") == "activate":
            pass  # v0 激活由宿主完成（复制 payload）


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
