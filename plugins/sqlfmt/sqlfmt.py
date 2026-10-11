#!/usr/bin/env python3
"""SQL 格式化（M5 UI 插件层 v2）：面板输入 sql 打开工具窗。

纯标准库轻量实现（不解析完整 SQL 语法，够日常可读）：
- 关键字大写，主子句（FROM/WHERE/JOIN…）换行；
- AND/OR 独行并多缩一层；
- 子查询括号内整体缩进一层；IN (1,2,3) 这类值列表保持同行；
- 字符串/注释原样保留。「压缩」= 去注释 + 收空白。

textarea 贴 SQL → 「格式化」「压缩」按钮不关窗原地出结果，
行内「复制」由宿主直写剪贴板（零权限声明）。
"""

import json
import sys

HINTS = {"sql", "sqlfmt"}

# 主子句：前面换行
MAJOR = {
    "FROM", "WHERE", "GROUP BY", "ORDER BY", "HAVING", "LIMIT", "OFFSET",
    "VALUES", "SET", "UNION", "UNION ALL", "EXCEPT", "INTERSECT",
    "JOIN", "INNER JOIN", "LEFT JOIN", "RIGHT JOIN", "FULL JOIN",
    "CROSS JOIN", "LEFT OUTER JOIN", "RIGHT OUTER JOIN", "FULL OUTER JOIN",
}
# 多词关键字合并（词法层）
MERGES = {
    ("GROUP", "BY"): "GROUP BY", ("ORDER", "BY"): "ORDER BY",
    ("UNION", "ALL"): "UNION ALL",
    ("LEFT", "JOIN"): "LEFT JOIN", ("RIGHT", "JOIN"): "RIGHT JOIN",
    ("INNER", "JOIN"): "INNER JOIN", ("CROSS", "JOIN"): "CROSS JOIN",
    ("FULL", "JOIN"): "FULL JOIN",
    ("LEFT", "OUTER"): "LEFT OUTER", ("RIGHT", "OUTER"): "RIGHT OUTER",
    ("FULL", "OUTER"): "FULL OUTER",
    ("LEFT OUTER", "JOIN"): "LEFT OUTER JOIN",
    ("RIGHT OUTER", "JOIN"): "RIGHT OUTER JOIN",
    ("FULL OUTER", "JOIN"): "FULL OUTER JOIN",
}
# 输出大写的常用关键字（字符串整段成 token，词法层天然不受影响）
KEYWORDS = {
    "SELECT", "FROM", "WHERE", "AND", "OR", "NOT", "IN", "IS", "NULL",
    "LIKE", "BETWEEN", "EXISTS", "AS", "ON", "ASC", "DESC", "DISTINCT",
    "CASE", "WHEN", "THEN", "ELSE", "END", "INSERT", "INTO", "VALUES",
    "UPDATE", "SET", "DELETE", "CREATE", "TABLE", "DROP", "ALTER",
    "INDEX", "VIEW", "JOIN", "LEFT", "RIGHT", "INNER", "OUTER", "CROSS",
    "FULL", "UNION", "ALL", "GROUP", "ORDER", "BY", "HAVING", "LIMIT",
    "OFFSET", "PRIMARY", "KEY", "FOREIGN", "REFERENCES", "DEFAULT",
    "COUNT", "SUM", "AVG", "MIN", "MAX",
}


def send(o):
    sys.stdout.write(json.dumps(o, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def join_line(tokens):
    """行内智能拼接：逗号/右括号/分号贴前词，左括号贴后词。"""
    out = ""
    for t in tokens:
        if out and t not in (",", ")", ";") and not out.endswith("("):
            out += " "
        out += t
    return out


# ---- 词法 ------------------------------------------------------------------

def tokenize(sql):
    """→ [(kind, text)]，kind ∈ word/str/comment/punc。字符串与注释整段保留。"""
    toks = []
    i, n = 0, len(sql)
    while i < n:
        c = sql[i]
        if c.isspace():
            i += 1
        elif sql.startswith("--", i):
            j = sql.find("\n", i)
            j = n if j < 0 else j
            toks.append(("comment", sql[i:j].rstrip()))
            i = j
        elif sql.startswith("/*", i):
            j = sql.find("*/", i + 2)
            j = n if j < 0 else j + 2
            toks.append(("comment", sql[i:j]))
            i = j
        elif c in "'\"`":
            j = i + 1
            while j < n:
                if sql[j] == c:
                    if j + 1 < n and sql[j + 1] == c:  # 双写转义
                        j += 2
                        continue
                    break
                j += 1
            j = min(j + 1, n)
            toks.append(("str", sql[i:j]))
            i = j
        elif c == "[":
            j = sql.find("]", i)
            j = n if j < 0 else j + 1
            toks.append(("str", sql[i:j]))
            i = j
        elif c in "(),;":
            toks.append(("punc", c))
            i += 1
        else:
            j = i
            while j < n and not sql[j].isspace() and sql[j] not in "'\"`[(),;":
                j += 1
            toks.append(("word", sql[i:j]))
            i = j
    # 多词关键字合并（大小写不敏感）
    out = []
    i = 0
    while i < len(toks):
        k, t = toks[i]
        if k == "word":
            up = t.upper()
            nxt = toks[i + 1][1].upper() if i + 1 < len(toks) and toks[i + 1][0] == "word" else None
            if (up, nxt) in MERGES:
                merged = MERGES[(up, nxt)]
                # LEFT OUTER → LEFT OUTER JOIN 三连
                if i + 2 < len(toks) and toks[i + 2][0] == "word" \
                        and (merged, toks[i + 2][1].upper()) in MERGES:
                    merged = MERGES[(merged, toks[i + 2][1].upper())]
                    out.append(("word", merged))
                    i += 3
                else:
                    out.append(("word", merged))
                    i += 2
                continue
        out.append((k, t))
        i += 1
    return out


# ---- 格式化 ----------------------------------------------------------------

def format_sql(sql):
    toks = tokenize(sql)
    lines = []
    cur = []            # 当前行 token
    cur_indent = 0
    depth = 0           # 子查询缩进层级
    stack = []          # 每层 "(" 是否子查询

    def flush():
        if cur:
            lines.append("  " * cur_indent + join_line(cur))
            cur.clear()

    def peek_word(idx):
        return toks[idx][1].upper() if idx < len(toks) and toks[idx][0] == "word" else None

    for idx, (kind, tok) in enumerate(toks):
        if kind == "comment":
            flush()
            lines.append("  " * depth + tok)
        elif kind == "str":
            cur.append(tok)
        elif kind == "punc":
            if tok == ",":
                cur.append(",")
                # 顶层或子查询里的逗号换行；值列表逗号保持同行
                if not stack or stack[-1]:
                    flush()
                    cur_indent = depth + 1
            elif tok == "(":
                # 向后看穿注释：下一个 word 是 SELECT → 子查询。
                # 「FROM (」保持同行，括号内整体缩进一层。
                j = idx + 1
                while j < len(toks) and toks[j][0] == "comment":
                    j += 1
                is_sub = peek_word(j) == "SELECT"
                stack.append(is_sub)
                if is_sub:
                    cur.append("(")
                    flush()
                    depth += 1
                    cur_indent = depth
                else:
                    cur.append("(")
            elif tok == ")":
                if stack and stack.pop():
                    flush()
                    depth = max(0, depth - 1)
                    cur_indent = depth
                    cur.append(")")
                else:
                    cur.append(")")
            elif tok == ";":
                cur.append(";")
                flush()
        else:  # word
            up = tok.upper()
            if up == "SELECT":
                flush()
                cur_indent = depth
                cur.append("SELECT")
            elif up in MAJOR:
                flush()
                cur_indent = depth
                cur.append(up)
            elif up in ("AND", "OR"):
                flush()
                cur_indent = depth + 1
                cur.append(up)
            elif up in KEYWORDS:
                cur.append(up)
            else:
                cur.append(tok)
    flush()
    return "\n".join(lines)


def compact_sql(sql):
    out = []
    for kind, tok in tokenize(sql):
        if kind == "comment":
            continue
        if out and tok not in (",", ")", ";") and out[-1] != "(":
            out.append(" ")
        out.append(tok)
    return "".join(out)


# ---- 协议 ------------------------------------------------------------------

UI_SEQ = 100


def ui_spec(ui_id, sql, rows):
    spec = {
        "title": "SQL 格式化",
        "fields": [
            {
                "key": "sql",
                "kind": "textarea",
                "label": "SQL（Ctrl+V 粘贴）",
                "value": sql,
                "lines": 8,
            },
        ],
        "actions": [
            {"key": "fmt", "label": "格式化"},
            {"key": "compact", "label": "压缩"},
        ],
    }
    if rows:
        spec["output"] = {"rows": rows}
    return {"type": "ui.show", "ui_id": ui_id, "spec": spec}


def rows_fmt(sql):
    return [{"title": "格式化 SQL", "subtitle": "点「复制」取全文",
             "copy": sql, "preview": sql}]


def rows_compact(sql):
    return [{"title": "压缩 SQL", "subtitle": "点「复制」取全文",
             "copy": sql, "preview": sql}]


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
            # 只响应 sql 词头（查询全量广播，非本插件词头静默）
            if low in HINTS:
                UI_SEQ += 1
                send(ui_spec(UI_SEQ, "", []))
            elif low.startswith("sql "):
                rest = text[4:].strip()
                UI_SEQ += 1
                rows = rows_fmt(format_sql(rest)) if rest else []
                send(ui_spec(UI_SEQ, rest, rows))

        elif t == "ui.event":
            ev = msg.get("event") or ""
            if ev == "action:fmt":
                sql = (msg.get("values") or {}).get("sql", "")
                send(ui_spec(msg.get("ui_id", 0), sql, rows_fmt(format_sql(sql))))
            elif ev == "action:compact":
                sql = (msg.get("values") or {}).get("sql", "")
                send(ui_spec(msg.get("ui_id", 0), sql, rows_compact(compact_sql(sql))))


if __name__ == "__main__":
    main()
