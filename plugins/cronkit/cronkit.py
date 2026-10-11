#!/usr/bin/python3
"""cron 表达式工具（纯标准库）：解析 → 人话解读 + 下 N 次执行预览。

面板输 cron 开窗；`cron 0 9 * * 1-5` 带表达式直开直算。
支持 5 段标准语法（* , - /）与 @hourly/@daily/@weekly/@monthly/@yearly；
星期 0/7 = 周日；「日」与「周」同时受限时按 cron 语义取并集。
"""

import json
import sys
from datetime import datetime, timedelta

SPECIAL = {
    "@hourly": "0 * * * *", "@daily": "0 0 * * *", "@midnight": "0 0 * * *",
    "@weekly": "0 0 * * 0", "@monthly": "0 0 1 * *", "@yearly": "0 0 1 1 *",
    "@annually": "0 0 1 1 *",
}
RANGES = [(0, 59), (0, 23), (1, 31), (1, 12), (0, 7)]
WEEK = {0: "周日", 1: "周一", 2: "周二", 3: "周三", 4: "周四", 5: "周五", 6: "周六"}
UI_SEQ = 100


def send(o):
    sys.stdout.write(json.dumps(o, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def err_row(msg):
    return {"title": "✗ " + msg, "subtitle": "修正表达式后重试"}


def parse_field(f, lo, hi):
    vals = set()
    for part in f.split(","):
        part = part.strip()
        if not part:
            raise ValueError("存在空字段")
        if "/" in part:
            base, _, step_s = part.partition("/")
            step = int(step_s)
            if step < 1:
                raise ValueError("步长须 ≥ 1")
            start = lo if base in ("*", "") else int(base)
            vals.update(range(start, hi + 1, step))
        elif "-" in part:
            a, b = part.split("-", 1)
            a, b = int(a), int(b)
            if a > b:
                raise ValueError(f"区间起大于止：{a}-{b}")
            vals.update(range(a, b + 1))
        elif part == "*":
            vals.update(range(lo, hi + 1))
        else:
            vals.add(int(part))
    out = {v for v in vals if lo <= v <= hi}
    if not out:
        raise ValueError("取值全部越界")
    return out


def parse_expr(expr):
    fields = SPECIAL.get(expr.strip().lower(), expr.strip())
    parts = fields.split()
    if len(parts) != 5:
        raise ValueError("cron 应为 5 段：分 时 日 月 周")
    mins = parse_field(parts[0], 0, 59)
    hrs = parse_field(parts[1], 0, 23)
    doms = parse_field(parts[2], 1, 31)
    mons = parse_field(parts[3], 1, 12)
    dows = {0 if v == 7 else v for v in parse_field(parts[4], 0, 7)}
    return parts, mins, hrs, doms, mons, dows


def _set_text(vals, full, unit, fmt=None):
    if vals == full:
        return None
    if len(vals) == 1:
        v = next(iter(vals))
        return fmt(v) if fmt else f"{unit}{v}"
    shown = ",".join(str(v) for v in sorted(vals)[:6])
    more = "…" if len(vals) > 6 else ""
    return f"{unit}{'/' if False else ''}{shown}{more}"


def describe(parts, mins, hrs, doms, mons, dows):
    """人话解读：尽量组句，复杂结构退回分段描述。"""
    dom_star, dow_star = parts[2] == "*", parts[4] == "*"
    mon_txt = None if parts[3] == "*" else "、".join(f"{m}月" for m in sorted(mons))
    day_txt = None if dom_star else "、".join(f"{d}日" for d in sorted(doms)[:8])
    week_txt = None if dow_star else "、".join(WEEK[d] for d in sorted(dows))

    # 时间部分
    if len(hrs) == 24 and len(mins) == 59 + 1 - len({m for m in mins}) + len(mins) and len(mins) == 60:
        time_txt = "每分钟"
    elif len(mins) == 60:
        time_txt = "每小时（第 " + "、".join(str(h) for h in sorted(hrs)[:8]) + " 点左右每小时）" if False else "每小时"
    elif len(hrs) == 24:
        time_txt = "每小时第 " + "、".join(f"{m:02d} 分" for m in sorted(mins)[:8]) + " 分"
    elif len(hrs) == 1 and len(mins) == 1:
        time_txt = f"{next(iter(hrs)):02d}:{next(iter(mins)):02d}"
    elif len(hrs) == 1 and "/" in parts[0]:
        time_txt = f"每小时第 {sorted(mins)[0]:02d} 分起每 {len(mins)} 分钟"
    else:
        times = [f"{h:02d}:{m:02d}" for h in sorted(hrs)[:4] for m in sorted(mins)[:1]]
        time_txt = "、".join(times) + ("…" if len(hrs) > 4 else "")

    # 日期部分
    if dom_star and dow_star:
        when = "每天"
    elif dom_star:
        when = f"每{week_txt}"
    elif dow_star:
        when = f"每月{day_txt}"
    else:
        when = f"{mon_txt or '每月'} {day_txt} 或 {week_txt}"
    head = f"{when} {time_txt}" if when != "每天" or True else time_txt
    return head.replace("每天 每天", "每天")


def next_runs(expr, count=5):
    parts, mins, hrs, doms, mons, dows = parse_expr(expr)
    dom_star, dow_star = parts[2] == "*", parts[4] == "*"
    t = datetime.now().replace(second=0, microsecond=0) + timedelta(minutes=1)
    limit = t + timedelta(days=366 * 4)
    out = []
    while t < limit and len(out) < count:
        if t.month not in mons:
            # 跳到下月 1 日 00:00
            t = (t.replace(day=1) + timedelta(days=32)).replace(day=1, hour=0, minute=0)
            continue
        dom_ok = t.day in doms
        dow_ok = (t.weekday() + 1) % 7 in dows
        if dom_star and dow_star:
            day_ok = True
        elif dom_star:
            day_ok = dow_ok
        elif dow_star:
            day_ok = dom_ok
        else:
            day_ok = dom_ok or dow_ok
        if not day_ok:
            t = (t + timedelta(days=1)).replace(hour=0, minute=0)
            continue
        if t.hour in hrs and t.minute in mins:
            out.append(t)
        t += timedelta(minutes=1)
    if len(out) < count:
        raise ValueError("4 年内没有匹配的执行时间（如 2 月 30 日）")
    return out


def rows_for(expr, count=5):
    parts, mins, hrs, doms, mons, dows = parse_expr(expr)
    rows = [{"title": "📋 " + describe(parts, mins, hrs, doms, mons, dows),
             "subtitle": "表达式解读"}]
    for t in next_runs(expr, count):
        rows.append({"title": t.strftime("%Y-%m-%d %H:%M (%a)"),
                     "subtitle": "下次执行 · 点「复制」",
                     "copy": t.strftime("%Y-%m-%d %H:%M:%S")})
    return rows


def ui_spec(ui_id, expr, count, rows):
    spec = {
        "title": "cron 表达式",
        "fields": [
            {"key": "expr", "kind": "text", "label": "表达式（5 段：分 时 日 月 周；支持 @daily 等）",
             "value": expr, "placeholder": "如 0 9 * * 1-5"},
            {"key": "count", "kind": "number", "label": "预览条数",
             "min": 1, "max": 10, "value": count},
        ],
        "actions": [{"key": "run", "label": "解析"}],
    }
    if rows:
        spec["output"] = {"rows": rows}
    return {"type": "ui.show", "ui_id": ui_id, "spec": spec}


def main():
    global UI_SEQ
    for line in sys.stdin:
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        t = msg.get("type")

        if t == "init":
            send({"type": "ready"})

        elif t == "shutdown":
            return

        elif t == "query":
            text = (msg.get("text") or "").strip()
            low = text.lower()
            if low in ("cron", "crontab", "定时"):
                UI_SEQ += 1
                send(ui_spec(UI_SEQ, "", 5, []))
            elif low.startswith(("cron ", "crontab ")):
                expr = text.split(None, 1)[1].strip()
                UI_SEQ += 1
                try:
                    rows = rows_for(expr, 5)
                except Exception as e:
                    rows = [err_row(str(e))]
                send(ui_spec(UI_SEQ, expr, 5, rows))

        elif t == "ui.event":
            ev = msg.get("event") or ""
            if ev == "action:run":
                values = msg.get("values") or {}
                expr = (values.get("expr") or "").strip()
                count = int(values.get("count", 5))
                rows = []
                if expr:
                    try:
                        rows = rows_for(expr, max(1, min(10, count)))
                    except Exception as e:
                        rows = [err_row(str(e))]
                send(ui_spec(msg.get("ui_id", 0), expr, count, rows))


if __name__ == "__main__":
    main()
