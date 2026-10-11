#!/usr/bin/env bash
# 电台收藏迁移：中心配置 → 电台插件数据目录（M5 电台插件化，硬性规则 #8）。
#
# 电台从中心内置页改为独立插件后，插件沙箱读不到 ~/.config，收藏必须
# 搬进插件数据目录（~/.local/share/yihu/plugin-data/radio/）。
# 默认 dry-run 只预览；--apply 执行；迁移前留时间戳备份，可直接回滚。
#
# 用法：
#   scripts/migrate-radio-favorites.sh           # 预览
#   scripts/migrate-radio-favorites.sh --apply   # 执行迁移
# 回滚：
#   cp "备份文件" ~/.config/yihu/radio_favorites.json
set -euo pipefail

SRC="${XDG_CONFIG_HOME:-$HOME/.config}/yihu/radio_favorites.json"
DST="${XDG_DATA_HOME:-$HOME/.local/share}/yihu/plugin-data/radio/favorites.json"
STAMP="$(date +%Y%m%d-%H%M%S)"
BACKUP="${SRC}.bak-${STAMP}"

if [ ! -f "$SRC" ]; then
  echo "无旧收藏文件（$SRC），无需迁移。"
  exit 0
fi

echo "源：$SRC（$(wc -c <"$SRC") 字节）"
echo "目标：$DST"
echo "备份：$BACKUP（迁移后保留，用于回滚）"
python3 - "$SRC" <<'EOF' || true
import json, sys
try:
    favs = json.load(open(sys.argv[1], encoding="utf-8"))
    n = len(favs) if isinstance(favs, list) else 0
    print(f"收藏条目：{n} 条")
except Exception as e:
    print(f"警告：文件不是合法 JSON（{e}），将原样搬运")
EOF

if [ "${1:-}" != "--apply" ]; then
  echo "（dry-run 预览，未做任何改动；加 --apply 执行）"
  exit 0
fi

mkdir -p "$(dirname "$DST")"
chmod 700 "$(dirname "$DST")"
cp "$SRC" "$BACKUP"
mv "$SRC" "$DST"
chmod 600 "$DST"
echo "迁移完成：$DST"
echo "回滚：cp \"$BACKUP\" \"$SRC\""
