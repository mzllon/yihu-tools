#!/usr/bin/env bash
# quicknotes 拆分为 todo / stickynotes 两插件：数据迁移。
# 待办 → todo/todos.json；便签记录 → stickynotes/notes.json；
# 便签 md 文件 → stickynotes/stickies/。默认 dry-run；--apply 执行；
# 源目录整体备份，回滚一行命令。此迁移只跑一次（源目录迁移后改名）。
set -euo pipefail

SRC="${XDG_DATA_HOME:-$HOME/.local/share}/yihu/plugin-data/quicknotes"
DST_TODO="${XDG_DATA_HOME:-$HOME/.local/share}/yihu/plugin-data/todo"
DST_NOTE="${XDG_DATA_HOME:-$HOME/.local/share}/yihu/plugin-data/stickynotes"
STAMP="$(date +%Y%m%d-%H%M%S)"

if [ ! -d "$SRC" ]; then
  echo "无 quicknotes 数据目录，无需迁移。"
  exit 0
fi

echo "源：$SRC"
if [ "${1:-}" != "--apply" ]; then
  echo "（dry-run 预览，加 --apply 执行）"
  exit 0
fi

mv "$SRC" "${SRC}.bak-${STAMP}"
mkdir -p "$DST_TODO" "$DST_NOTE/stickies"
chmod 700 "$DST_TODO" "$DST_NOTE" "$DST_NOTE/stickies" 2>/dev/null || true

python3 - "$SRC.bak-$STAMP" "$DST_TODO" "$DST_NOTE" <<'PYEOF'
import json, os, sys
src, dst_todo, dst_note = sys.argv[1:4]
qf = os.path.join(src, "quicknotes.json")
items = json.load(open(qf, encoding="utf-8")) if os.path.exists(qf) else []
todos = [x for x in items if x.get("type") == "todo"]
notes = [x for x in items if x.get("type") == "note"]
json.dump(todos, open(os.path.join(dst_todo, "todos.json"), "w", encoding="utf-8"),
          ensure_ascii=False, indent=1)
json.dump(notes, open(os.path.join(dst_note, "notes.json"), "w", encoding="utf-8"),
          ensure_ascii=False, indent=1)
print(f"待办 {len(todos)} 条、便签记录 {len(notes)} 条已拆分")
PYEOF

if [ -d "$SRC/stickies" ]; then
  cp -a "$SRC/stickies/." "$DST_NOTE/stickies/"
fi
echo "迁移完成。回滚：mv \"${SRC}.bak-${STAMP}\" \"$SRC\""
