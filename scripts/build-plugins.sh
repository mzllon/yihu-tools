#!/usr/bin/env bash
# 构建全部 Rust 官方插件并把二进制拷到各自插件目录（install_from_dir 的入口约定）。
# 用法：scripts/build-plugins.sh [release]
set -euo pipefail
cd "$(dirname "$0")/.."
PROFILE=${1:-debug}
FLAG=""
if [ "$PROFILE" = "release" ]; then FLAG="--release"; fi
PLUGINS=(cronkit csvkit randgen passgen sqlfmt jsonkit devcodec ts-convert)
cargo build $FLAG "${PLUGINS[@]/#/-p}"
for p in "${PLUGINS[@]}"; do
  cp "target/$PROFILE/$p" "plugins/$p/$p"
  echo "  $p ✓"
done
echo "Rust 插件构建完成（二进制已就位于各插件目录）"
