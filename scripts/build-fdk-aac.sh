#!/usr/bin/env bash
# 构建带 SBR/PS 的完整版 libfdk-aac，装到电台插件数据目录。
#
# 为什么需要它：Debian/Ubuntu 的 libfdk-aac2 来自 fdk-aac-free 源码包，
# SBR/PS 解码器因专利授权被整体剥除（HE-AAC 只能解出 22050Hz 发闷的
# 核心，且无任何报错）。电台直播流实测是 HE-AAC v1，必须有完整版。
# 个人机器自建自用；加载逻辑见 plugins/radio/src/fdk.rs（探针会拒绝
# free 版，所以本脚本是 Debian/Ubuntu 上的必选项）。
#
# 源码来源：crates.io 上的 fdk-aac-sys crate 内嵌了 fdk-aac 2.0.2 完整
# 源码（与系统 ABI 一致），直接下载解包编译，无需 git/网络仓库。
#
# 用法：scripts/build-fdk-aac.sh
# 产物：~/.local/share/yihu/plugins/radio/lib/libfdk-aac.so.2（~1MB）
set -euo pipefail

CRATE=fdk-aac-sys
CRATE_VER=0.5.0
FDK_VER=2.0.2
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

DEST="${XDG_DATA_HOME:-$HOME/.local/share}/yihu/plugins/radio/lib"
command -v g++ >/dev/null || { echo "缺 g++（sudo apt install g++）"; exit 1; }

echo "==> 下载 $CRATE $CRATE_VER（内嵌 fdk-aac $FDK_VER 完整源码）"
curl -fsSL --retry 3 -o "$WORK/crate.crate" \
    "https://static.crates.io/crates/$CRATE/$CRATE-$CRATE_VER.crate"
tar -xzf "$WORK/crate.crate" -C "$WORK"
SRC="$WORK/$CRATE-$CRATE_VER/aac"
test -d "$SRC/libSBRdec" || { echo "源码包里没有 libSBRdec，异常"; exit 1; }

echo "==> 编译（解码器 + SBR/PS，排除 arm/mips/x86 专用文件）"
cd "$WORK"
SRCS=$(find "$SRC"/libAACdec/src "$SRC"/libSBRdec/src "$SRC"/libSACdec/src \
           "$SRC"/libDRCdec/src "$SRC"/libMpegTPDec/src "$SRC"/libSYS/src \
           "$SRC"/libFDK/src "$SRC"/libArithCoding/src "$SRC"/libPCMutils/src \
           -name '*.cpp' -not -path '*/arm/*' -not -path '*/x86/*' \
           -not -path '*/mips/*' -not -path '*/riscv/*')
INC=$(for d in "$SRC"/*/include; do echo -n "-I$d "; done)
# shellcheck disable=SC2086
g++ -shared -fPIC -O2 -DNDEBUG $INC -o "$WORK/libfdk-aac.so.2" $SRCS

echo "==> 验证 SBR 符号存在"
nm -D --defined-only "$WORK/libfdk-aac.so.2" | grep -q aacDecoder_Open
echo "    aacDecoder_* 导出正常（$(du -h "$WORK/libfdk-aac.so.2" | cut -f1)）"

echo "==> 安装到 $DEST"
mkdir -p "$DEST"
install -m 0755 "$WORK/libfdk-aac.so.2" "$DEST/libfdk-aac.so.2"

echo "完成。电台插件会优先加载本库；收听即验证（日志出现「电台出声：44100Hz」）。"
