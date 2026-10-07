"""一呼：Nautilus 右键「复制绝对路径」「加入一呼搜索」（顶级菜单项）。

需要安装 python3-nautilus（sudo apt install python3-nautilus），
随后将本文件放到 ~/.local/share/nautilus-python/extensions/。

实现说明：复制动作在 Nautilus 进程内完成——用户右键时 Nautilus
持有键盘焦点，此时设置剪贴板符合 Wayland 的属主规则（无焦点的
外部进程会被 mutter 拒绝，这是实测结论）。写入失败/成功均通过
系统通知反馈，便于排查。

「加入一呼搜索」（M4 二期 files 触发）：把所选文件经面板 DBus
SelectFiles 注入上下文并呼出面板；面板只把 context.files 转发给
manifest 声明了 selected_files.read 的插件会话，收起即失效。
"""

import subprocess
import urllib.parse

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Gdk", "4.0")
from gi.repository import Gdk, GLib, Nautilus  # noqa: E402

from gi.repository import Gio, GObject  # noqa: E402


def _notify(msg: str) -> None:
    try:
        subprocess.Popen(
            ["notify-send", "--app-name=一呼", "--expire-time=3000", msg],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    except Exception:
        pass


def _copy_to_clipboard(text: str) -> bool:
    display = Gdk.Display.get_default()
    if display is None:
        return False
    clipboard = display.get_clipboard()
    provider = Gdk.ContentProvider.new_for_bytes(
        "text/plain;charset=utf-8", GLib.Bytes.new(text.encode("utf-8"))
    )
    return clipboard.set_content(provider)


class CopyAbsolutePathExtension(GObject.GObject, Nautilus.MenuProvider):
    def get_file_items(self, files):
        paths = [
            urllib.parse.unquote(f.get_uri()[len("file://"):])
            for f in files
            if f.get_uri_scheme() == "file"
        ]
        if not paths:
            return []
        copy_item = Nautilus.MenuItem(
            name="Yihu::CopyAbsolutePath",
            label="复制绝对路径",
            tip="复制所选文件的绝对路径到剪贴板",
        )
        copy_item.connect("activate", lambda *_: self._activate_copy(paths))
        ctx_item = Nautilus.MenuItem(
            name="Yihu::AddPanelContext",
            label="加入一呼搜索",
            tip="把所选文件作为上下文注入呼出面板",
        )
        ctx_item.connect("activate", lambda *_: self._activate_context(paths))
        return [copy_item, ctx_item]

    def _activate_copy(self, paths):
        text = "\n".join(paths)
        try:
            if _copy_to_clipboard(text):
                _notify(f"已复制 {len(paths)} 项绝对路径")
            else:
                _notify("复制失败：无法写入剪贴板")
        except Exception as e:  # 异常在 Nautilus 内不可见，转为通知
            _notify(f"复制失败：{e}")

    def _activate_context(self, paths):
        try:
            proxy = Gio.DBusProxy.new_for_bus_sync(
                Gio.BusType.SESSION,
                Gio.DBusProxyFlags.NONE,
                None,
                "tools.yihu.Panel",
                "/tools/yihu/Panel",
                "tools.yihu.Panel",
                None,
            )
            proxy.call_sync(
                "SelectFiles",
                GLib.Variant("(as)", ([p for p in paths],)),
                Gio.DBusCallFlags.NONE,
                2000,
                None,
            )
            proxy.call_sync("Toggle", None, Gio.DBusCallFlags.NONE, 2000, None)
            _notify(f"已加入一呼上下文 {len(paths)} 项")
        except Exception as e:
            _notify(f"加入上下文失败（面板未运行？）：{e}")
