#!/usr/bin/env bash
# MiniTools：Nautilus 右键脚本 —— 复制选中项的绝对路径
# 安装位置：~/.local/share/nautilus/scripts/复制绝对路径
#
# 剪贴板策略：GNOME Wayland 下剪贴板属主必须走 data-control 协议，
# 无焦点的普通进程无法直接设置（实测 mutter 会拒绝），因此本脚本
# 唯一可靠的写入途径是 wl-clipboard 的 wl-copy。未安装时弹通知提示。
#
#   sudo apt install wl-clipboard
set -euo pipefail

sel="${NAUTILUS_SCRIPT_SELECTED_FILE_PATHS:-}"

# 无选中时回退为当前目录（file:// URI 需要解码）
if [ -z "$sel" ] && [ -n "${NAUTILUS_SCRIPT_CURRENT_URI:-}" ]; then
  sel=$(python3 -c 'import sys,urllib.parse; print(urllib.parse.unquote(sys.argv[1][7:]))' \
    "$NAUTILUS_SCRIPT_CURRENT_URI")
fi
[ -n "$sel" ] || exit 0

if command -v wl-copy >/dev/null 2>&1; then
  printf '%s\n' "$sel" | wl-copy
else
  notify-send --app-name=一呼 --icon=dialog-warning \
    "复制绝对路径" "缺少 wl-clipboard，请执行：sudo apt install wl-clipboard" || true
  exit 1
fi
