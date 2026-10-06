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

echo "已安装示例插件：ts-convert、passgen、sandbox-probe（$REG）"
