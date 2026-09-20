#!/usr/bin/env bash
# Migrate legacy user data to the Yihu namespace.
# Dry-run is the default. Formal migration keeps a timestamped rollback copy.
set -euo pipefail

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
DRY_RUN=1
if [[ "${1:-}" == "--apply" ]]; then
  DRY_RUN=0
elif [[ "${1:-}" != "" && "${1:-}" != "--dry-run" ]]; then
  printf 'Usage: %s [--dry-run|--apply]\n' "$0" >&2
  exit 2
fi

home=${HOME:?HOME is required}
config_home=${XDG_CONFIG_HOME:-$home/.config}
state_home=${XDG_STATE_HOME:-$home/.local/state}
old_dir="$config_home/minitools"
new_dir="$config_home/yihu"
backup_root="$state_home/yihu/migrations"
timestamp=$(date +%Y%m%d-%H%M%S)
backup="$backup_root/$timestamp"

known=(autodark.conf panel.conf panel_history.json radio_favorites.json plugins_state.json)

sha256_file() {
  sha256sum -- "$1" | awk '{print $1}'
}

report() { printf '%s\n' "$*"; }

if [[ $DRY_RUN -eq 1 ]]; then
  report "模式：dry-run（不修改文件、不删除数据）"
else
  report "模式：apply"
  mkdir -p -- "$backup"
  chmod 700 -- "$backup_root" "$backup"
  {
    printf 'source=%s\n' "$old_dir"
    printf 'target=%s\n' "$new_dir"
    printf 'backup=%s\n' "$backup"
    printf 'timestamp=%s\n' "$timestamp"
  } > "$backup/MIGRATION"
fi

if [[ ! -d "$old_dir" ]]; then
  report "旧配置目录不存在：$old_dir"
else
  report "旧配置目录：$old_dir"
fi

for name in "${known[@]}"; do
  src="$old_dir/$name"
  dst="$new_dir/$name"
  if [[ ! -e "$src" ]]; then
    continue
  fi
  if [[ -e "$dst" ]]; then
    src_hash=$(sha256_file "$src")
    dst_hash=$(sha256_file "$dst")
    if [[ "$src_hash" == "$dst_hash" ]]; then
      report "已相同，保留两份：$name ($src_hash)"
    else
      report "冲突，跳过不覆盖：$name"
    fi
    continue
  fi
  report "迁移：$src -> $dst"
  if [[ $DRY_RUN -eq 0 ]]; then
    mkdir -p -- "$new_dir"
    cp -p -- "$src" "$dst"
    src_hash=$(sha256_file "$src")
    dst_hash=$(sha256_file "$dst")
    [[ "$src_hash" == "$dst_hash" ]] || { report "校验失败：$name" >&2; exit 1; }
    printf '%s\t%s\t%s\n' "$name" "$src_hash" "$(stat -c '%a' "$src")" >> "$backup/MIGRATION"
    mv -- "$src" "$backup/$name"
  fi
done

# 迁移空目录本身只在 apply 且确认没有未知文件时处理。
if [[ $DRY_RUN -eq 0 && -d "$old_dir" ]]; then
  shopt -s nullglob dotglob
  leftovers=("$old_dir"/*)
  shopt -u nullglob dotglob
  if [[ ${#leftovers[@]} -eq 0 ]]; then
    rmdir -- "$old_dir"
    report "删除空旧配置目录：$old_dir"
  else
    report "旧目录仍有未识别文件，保留不动：$old_dir"
  fi
fi

# 清理明确陈旧的 PID/stamp；未知缓存不碰。
old_pid="$home/.cache/minitools/nautilus-copy.pid"
if [[ -f "$old_pid" ]]; then
  pid=$(tr -dc '0-9' < "$old_pid" || true)
  alive=0
  if [[ "$pid" =~ ^[0-9]+$ ]] && [[ -d "/proc/$pid" ]]; then
    alive=1
  fi
  if [[ $alive -eq 0 ]]; then
    report "陈旧 PID 文件：$old_pid"
    if [[ $DRY_RUN -eq 0 ]]; then
      mkdir -p -- "$backup/cache"
      cp -p -- "$old_pid" "$backup/cache/nautilus-copy.pid"
      rm -f -- "$old_pid"
    fi
  else
    report "PID 仍存活，保留：$old_pid ($pid)"
  fi
fi

old_stamp="$home/.local/share/systemd/timers/stamp-minitools-autodark.timer"
if [[ -e "$old_stamp" ]]; then
  report "旧 AutoDark timer stamp：$old_stamp"
  if [[ $DRY_RUN -eq 0 ]]; then
    mkdir -p -- "$backup/cache"
    cp -p -- "$old_stamp" "$backup/cache/stamp-minitools-autodark.timer"
    rm -f -- "$old_stamp"
  fi
fi

retired="$home/.local/share/com.ubuntuminitools.sysdash"
if [[ -d "$retired" ]]; then
  if pgrep -f '(^|/)sysdash($| )' >/dev/null 2>&1; then
    report "退役 SysDash 仍在运行，保留：$retired"
  else
    report "退役 SysDash 数据：$retired"
    if [[ $DRY_RUN -eq 0 ]]; then
      mkdir -p -- "$backup/retired"
      mv -- "$retired" "$backup/retired/sysdash"
    fi
  fi
fi

if [[ $DRY_RUN -eq 0 ]]; then
  report "迁移备份保留于：$backup"
else
  report "dry-run 完成：没有任何文件被修改或删除"
fi
