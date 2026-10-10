#!/usr/bin/env bash
# 把示例插件安装进一呼的插件注册表（~/.local/share/yihu/plugins）。
# 前置：cargo build --release -p ts-convert 已完成。
set -euo pipefail
ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
REG="${XDG_DATA_HOME:-$HOME/.local/share}/yihu/plugins"

# Rust 示例：时间戳转换
if [[ ! -f "$ROOT/target/release/ts-convert" ]]; then
  echo "先构建：cargo build --release -p ts-convert" >&2
  exit 1
fi
mkdir -p "$REG/ts-convert"
install -m755 "$ROOT/target/release/ts-convert" "$REG/ts-convert/ts-convert"
install -m644 "$ROOT/plugins/ts-convert/manifest.toml" "$REG/ts-convert/manifest.toml"

# Python 示例：密码生成
mkdir -p "$REG/passgen"
install -m755 "$ROOT/plugins/passgen/passgen.py" "$REG/passgen/passgen.py"
install -m644 "$ROOT/plugins/passgen/manifest.toml" "$REG/passgen/manifest.toml"

# 沙箱探针：验证 bwrap 白名单边界（M4 安全收口）
mkdir -p "$REG/sandbox-probe"
install -m755 "$ROOT/plugins/sandbox-probe/probe.py" "$REG/sandbox-probe/probe.py"
install -m644 "$ROOT/plugins/sandbox-probe/manifest.toml" "$REG/sandbox-probe/manifest.toml"

# 截图插件：搜索行 + screenshot.take 能力代理（宿主 portal 执行）
mkdir -p "$REG/screenshot"
install -m755 "$ROOT/plugins/screenshot/shot.py" "$REG/screenshot/shot.py"
install -m644 "$ROOT/plugins/screenshot/manifest.toml" "$REG/screenshot/manifest.toml"

# 系统主题切换（M5 开放 API v2 示例：settings.write 声明制）
mkdir -p "$REG/sys-theme"
install -m755 "$ROOT/plugins/sys-theme/theme.py" "$REG/sys-theme/theme.py"
install -m644 "$ROOT/plugins/sys-theme/manifest.toml" "$REG/sys-theme/manifest.toml"

# 快捷键管理（真插件旗舰示例：settings.read/write 编排，宿主无对应内置）
mkdir -p "$REG/hotkeys"
install -m755 "$ROOT/plugins/hotkeys/hotkeys.py" "$REG/hotkeys/hotkeys.py"
install -m644 "$ROOT/plugins/hotkeys/manifest.toml" "$REG/hotkeys/manifest.toml"

echo "已安装示例插件：ts-convert、passgen、sandbox-probe、screenshot、sys-theme、hotkeys（$REG）"
