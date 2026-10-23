//! sqlfmt WASM 版：SQL 格式化/压缩（词法器完整移植自 Rust 原生版）。
//! 协议：查询文本 = "fmt\x1f<sql>" 或 "compact\x1f<sql>"。

use yihu_wasm_runtime::{QueryHandler, Row};

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Word(String),
    Str(String),
    Comment(String),
    Punc(char),
}


const MAJOR: &[&str] = &[
    "FROM", "WHERE", "GROUP BY", "ORDER BY", "HAVING", "LIMIT", "OFFSET",
    "VALUES", "SET", "UNION", "UNION ALL", "EXCEPT", "INTERSECT", "JOIN",
    "INNER JOIN", "LEFT JOIN", "RIGHT JOIN", "FULL JOIN", "CROSS JOIN",
    "LEFT OUTER JOIN", "RIGHT OUTER JOIN", "FULL OUTER JOIN",
];

const MERGES2: &[(&str, &str)] = &[
    ("GROUP", "GROUP BY"),
    ("ORDER", "ORDER BY"),
    ("UNION", "UNION ALL"),
    ("LEFT", "LEFT JOIN"),
    ("RIGHT", "RIGHT JOIN"),
    ("INNER", "INNER JOIN"),
    ("CROSS", "CROSS JOIN"),
    ("FULL", "FULL JOIN"),
    ("LEFT OUTER", "LEFT OUTER JOIN"),
    ("RIGHT OUTER", "RIGHT OUTER JOIN"),
    ("FULL OUTER", "FULL OUTER JOIN"),
];

const KEYWORDS: &[&str] = &[
    "SELECT", "FROM", "WHERE", "AND", "OR", "NOT", "IN", "IS", "NULL", "LIKE",
    "BETWEEN", "EXISTS", "AS", "ON", "ASC", "DESC", "DISTINCT", "CASE", "WHEN",
    "THEN", "ELSE", "END", "INSERT", "INTO", "VALUES", "UPDATE", "SET",
    "DELETE", "CREATE", "TABLE", "DROP", "ALTER", "INDEX", "VIEW", "JOIN",
    "LEFT", "RIGHT", "INNER", "OUTER", "CROSS", "FULL", "UNION", "ALL",
    "GROUP", "ORDER", "BY", "HAVING", "LIMIT", "OFFSET", "PRIMARY", "KEY",
    "FOREIGN", "REFERENCES", "DEFAULT", "COUNT", "SUM", "AVG", "MIN", "MAX",
];


fn tokenize(sql: &str) -> Vec<Tok> {
    let chars: Vec<char> = sql.chars().collect();
    let n = chars.len();
    let mut i = 0;
    let mut toks = Vec::new();
    let get = |from: usize, to: usize| chars[from..to].iter().collect::<String>();
    while i < n {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if sql[i..].starts_with("--") {
            let mut j = i;
            while j < n && chars[j] != '\n' {
                j += 1;
            }
            toks.push(Tok::Comment(get(i, j).trim_end().to_string()));
            i = j;
        } else if sql[i..].starts_with("/*") {
            let mut j = i + 2;
            while j + 1 < n && !(chars[j] == '*' && chars[j + 1] == '/') {
                j += 1;
            }
            let end = (j + 2).min(n);
            toks.push(Tok::Comment(get(i, end)));
            i = end;
        } else if c == '\'' || c == '"' || c == '`' {
            let mut j = i + 1;
            while j < n {
                if chars[j] == c {
                    if j + 1 < n && chars[j + 1] == c {
                        j += 2;
                        continue;
                    }
                    break;
                }
                j += 1;
            }
            let end = (j + 1).min(n);
            toks.push(Tok::Str(get(i, end)));
            i = end;
        } else if c == '[' {
            let mut j = i + 1;
            while j < n && chars[j] != ']' {
                j += 1;
            }
            let end = (j + 1).min(n);
            toks.push(Tok::Str(get(i, end)));
            i = end;
        } else if "(),;".contains(c) {
            toks.push(Tok::Punc(c));
            i += 1;
        } else {
            let mut j = i;
            while j < n
                && !chars[j].is_whitespace()
                && !"\"'`[(),;".contains(chars[j])
            {
                j += 1;
            }
            toks.push(Tok::Word(get(i, j)));
            i = j;
        }
    }
    // 多词关键字合并（大小写不敏感，两遍：OUTER 先并 JOIN）
    merge_words(toks)
}

fn merge_words(toks: Vec<Tok>) -> Vec<Tok> {
    // 多词关键字合并：词序列与 MAJOR 中多词条目逐词匹配则并成一个词元
    const MULTI: &[&str] = &[
        "GROUP BY", "ORDER BY", "UNION ALL", "INNER JOIN", "LEFT JOIN",
        "RIGHT JOIN", "FULL JOIN", "CROSS JOIN", "LEFT OUTER JOIN",
        "RIGHT OUTER JOIN", "FULL OUTER JOIN",
    ];
    let up = |t: &Tok| match t {
        Tok::Word(w) => w.to_uppercase(),
        _ => String::new(),
    };
    let mut out: Vec<Tok> = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        if !is_word(&toks[i]) {
            out.push(toks[i].clone());
            i += 1;
            continue;
        }
        let mut hit: Option<&str> = None;
        for m in MULTI {
            let words: Vec<&str> = m.split(' ').collect();
            if up(&toks[i]) != words[0] {
                continue;
            }
            let mut ok = true;
            for (k, w) in words.iter().enumerate() {
                match toks.get(i + k) {
                    Some(t) if is_word(t) && up(t) == *w => {}
                    _ => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                hit = Some(m);
                break;
            }
        }
        match hit {
            Some(m) => {
                let nwords = m.split(' ').count();
                out.push(Tok::Word(m.to_string()));
                i += nwords;
            }
            None => {
                out.push(toks[i].clone());
                i += 1;
            }
        }
    }
    out
}

fn join_line(tokens: &[String]) -> String {
    let mut out = String::new();
    for t in tokens {
        if !out.is_empty() && t != "," && t != ")" && t != ";" && !out.ends_with('(') {
            out.push(' ');
        }
        out.push_str(t);
    }
    out
}

fn is_word(t: &Tok) -> bool {
    matches!(t, Tok::Word(_))
}

fn format_sql(sql: &str) -> String {
    let toks = tokenize(sql);
    let mut lines: Vec<String> = Vec::new();
    let mut cur: Vec<String> = Vec::new();
    let mut cur_indent = 0usize;
    let mut depth = 0usize;
    let mut stack: Vec<bool> = Vec::new(); // 每层 "(" 是否子查询

    let flush = |lines: &mut Vec<String>, cur: &mut Vec<String>, indent: usize| {
        if !cur.is_empty() {
            lines.push(format!("{}{}", "  ".repeat(indent), join_line(cur)));
            cur.clear();
        }
    };

    for idx in 0..toks.len() {
        let tok = toks[idx].clone();
        match &tok {
            Tok::Comment(c) => {
                flush(&mut lines, &mut cur, cur_indent);
                lines.push(format!("{}{}", "  ".repeat(depth), c));
            }
            Tok::Str(s) => cur.push(s.clone()),
            Tok::Punc(p) => match p {
                ',' => {
                    cur.push(",".into());
                    if stack.is_empty() || *stack.last().unwrap_or(&false) {
                        flush(&mut lines, &mut cur, cur_indent);
                        cur_indent = depth + 1;
                    }
                }
                '(' => {
                    let mut j = idx + 1;
                    while j < toks.len() && matches!(toks[j], Tok::Comment(_)) {
                        j += 1;
                    }
                    let nxt = toks.get(j).map(|t| match t {
                        Tok::Word(w) => w.to_uppercase(),
                        _ => String::new(),
                    });
                    let is_sub = nxt.as_deref() == Some("SELECT");
                    stack.push(is_sub);
                    cur.push("(".into());
                    if is_sub {
                        flush(&mut lines, &mut cur, cur_indent);
                        depth += 1;
                        cur_indent = depth;
                    }
                }
                ')' => {
                    if stack.pop() == Some(true) {
                        flush(&mut lines, &mut cur, cur_indent);
                        depth = depth.saturating_sub(1);
                        cur_indent = depth;
                        cur.push(")".into());
                    } else {
                        cur.push(")".into());
                    }
                }
                ';' => {
                    cur.push(";".into());
                    flush(&mut lines, &mut cur, cur_indent);
                }
                _ => {}
            },
            Tok::Word(w) => {
                let up = w.to_uppercase();
                if up == "SELECT" {
                    flush(&mut lines, &mut cur, cur_indent);
                    cur_indent = depth;
                    cur.push("SELECT".into());
                } else if MAJOR.contains(&up.as_str()) {
                    flush(&mut lines, &mut cur, cur_indent);
                    cur_indent = depth;
                    cur.push(up);
                } else if up == "AND" || up == "OR" {
                    flush(&mut lines, &mut cur, cur_indent);
                    cur_indent = depth + 1;
                    cur.push(up);
                } else if KEYWORDS.contains(&up.as_str()) {
                    cur.push(up);
                } else {
                    cur.push(w.clone());
                }
            }
        }
    }
    flush(&mut lines, &mut cur, cur_indent);
    lines.join("\n")
}

fn compact_sql(sql: &str) -> String {
    let mut out = String::new();
    for (_, tok) in tokenize(sql).iter().enumerate() {
        if let Tok::Comment(_) = tok {
            continue;
        }
        let t = match tok {
            Tok::Word(w) => w.clone(),
            Tok::Str(s) => s.clone(),
            Tok::Comment(_) => continue,
            Tok::Punc(p) => p.to_string(),
        };
        if !out.is_empty() && t != "," && t != ")" && t != ";" && !out.ends_with('(') {
            out.push(' ');
        }
        out.push_str(&t);
    }
    out
}

struct SqlfmtWasm;

impl QueryHandler for SqlfmtWasm {
    fn handle(text: &str, _now: i64) -> Vec<Row> {
        let (mode, sql) = text.split_once("\x1f").unwrap_or(("", ""));
        match mode {
            "fmt" => vec![Row::new("格式化 SQL", "点「复制」取全文", format_sql(sql))],
            "compact" => vec![Row::new("压缩 SQL", "点「复制」取全文", compact_sql(sql))],
            // 面板查询广播是用户原文：静默自过滤，错误行不进搜索列表（BUG-012）
            _ => vec![],
        }
    }
}

yihu_wasm_runtime::export_plugin!(SqlfmtWasm);
