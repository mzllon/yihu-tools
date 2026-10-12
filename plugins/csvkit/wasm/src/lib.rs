//! csvkit WASM 版：CSV⇄JSON 转换（表头开关/分隔符识别/键并集）。
//! 纯计算零依赖：手写 CSV 状态机 + 手写 JSON 序列化。

use yihu_wasm_runtime::Row;

fn detect_delim(text: &str) -> char {
    let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let (c, s, t) = (
        first.matches(',').count(),
        first.matches(';').count(),
        first.matches('\t').count(),
    );
    if s > c && s > t {
        ';'
    } else if t > c && t > s {
        '\t'
    } else {
        ','
    }
}

/// CSV 单行解析（引号感知状态机）
fn parse_csv_line(line: &str, delim: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    cur.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                cur.push(c);
            }
        } else if c == '"' {
            in_quotes = true;
        } else if c == delim {
            out.push(core::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    out.push(cur);
    out
}

fn csv_escape(s: &str, delim: char) -> String {
    if s.contains(delim) || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// 极简 JSON 字符串序列化
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn csv_to_json(text: &str, delim: char, header: bool) -> Result<String, String> {
    let rows: Vec<Vec<String>> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| parse_csv_line(l, delim))
        .collect();
    if rows.is_empty() {
        return Err("没有数据行".into());
    }
    if header {
        let keys: Vec<String> = rows[0]
            .iter()
            .enumerate()
            .map(|(i, k)| {
                let k = k.trim();
                if k.is_empty() { format!("col{i}") } else { k.to_string() }
            })
            .collect();
        let mut out = String::from("[");
        for (ri, r) in rows[1..].iter().enumerate() {
            if ri > 0 {
                out.push(',');
            }
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&json_str(k));
                out.push(':');
                out.push_str(&json_str(r.get(i).map(|s| s.as_str()).unwrap_or("")));
            }
            out.push('}');
        }
        out.push(']');
        Ok(out)
    } else {
        let mut out = String::from("[");
        for (ri, r) in rows.iter().enumerate() {
            if ri > 0 {
                out.push(',');
            }
            out.push('[');
            for (i, c) in r.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&json_str(c));
            }
            out.push(']');
        }
        out.push(']');
        Ok(out)
    }
}

/// 极简 JSON 解析（对象数组 / 数组的数组 / 对象）→ 统一为字符串二维表
fn json_parse(text: &str) -> Result<JsonTable, String> {
    let bytes: Vec<char> = text.chars().collect();
    let mut pos = 0;
    skip_ws(&bytes, &mut pos);
    let v = parse_value(&bytes, &mut pos)?;
    to_table(v)
}

enum JsonValue {
    Str(String),
    Num(f64, bool), // (值, 是整数)
    Bool(bool),
    Null,
    Arr(Vec<JsonValue>),
    Obj(Vec<(String, JsonValue)>),
}

fn skip_ws(b: &[char], pos: &mut usize) {
    while *pos < b.len() && b[*pos].is_whitespace() {
        *pos += 1;
    }
}

fn parse_value(b: &[char], pos: &mut usize) -> Result<JsonValue, String> {
    skip_ws(b, pos);
    match b.get(*pos) {
        Some('"') => parse_str(b, pos).map(JsonValue::Str),
        Some('[') => {
            *pos += 1;
            let mut arr = Vec::new();
            skip_ws(b, pos);
            if b.get(*pos) == Some(&']') {
                *pos += 1;
                return Ok(JsonValue::Arr(arr));
            }
            loop {
                arr.push(parse_value(b, pos)?);
                skip_ws(b, pos);
                match b.get(*pos) {
                    Some(',') => *pos += 1,
                    Some(']') => {
                        *pos += 1;
                        break;
                    }
                    _ => return Err("JSON 数组格式错误".into()),
                }
            }
            Ok(JsonValue::Arr(arr))
        }
        Some('{') => {
            *pos += 1;
            let mut obj = Vec::new();
            skip_ws(b, pos);
            if b.get(*pos) == Some(&'}') {
                *pos += 1;
                return Ok(JsonValue::Obj(obj));
            }
            loop {
                skip_ws(b, pos);
                let k = parse_str(b, pos)?;
                skip_ws(b, pos);
                if b.get(*pos) != Some(&':') {
                    return Err("JSON 对象缺冒号".into());
                }
                *pos += 1;
                let v = parse_value(b, pos)?;
                obj.push((k, v));
                skip_ws(b, pos);
                match b.get(*pos) {
                    Some(',') => *pos += 1,
                    Some('}') => {
                        *pos += 1;
                        break;
                    }
                    _ => return Err("JSON 对象格式错误".into()),
                }
            }
            Ok(JsonValue::Obj(obj))
        }
        Some('t') if b[*pos..].starts_with(&['t', 'r', 'u', 'e']) => {
            *pos += 4;
            Ok(JsonValue::Bool(true))
        }
        Some('f') if b[*pos..].starts_with(&['f', 'a', 'l', 's', 'e']) => {
            *pos += 5;
            Ok(JsonValue::Bool(false))
        }
        Some('n') if b[*pos..].starts_with(&['n', 'u', 'l', 'l']) => {
            *pos += 4;
            Ok(JsonValue::Null)
        }
        Some(_) => {
            let start = *pos;
            while *pos < b.len()
                && (b[*pos].is_ascii_digit()
                    || matches!(b[*pos], '-' | '+' | '.' | 'e' | 'E'))
            {
                *pos += 1;
            }
            let s: String = b[start..*pos].iter().collect();
            let is_int = !s.contains('.') && !s.contains('e') && !s.contains('E');
            s.parse::<f64>()
                .map(|n| JsonValue::Num(n, is_int))
                .map_err(|_| "数字解析失败".into())
        }
        None => Err("JSON 意外结束".into()),
    }
}

fn parse_str(b: &[char], pos: &mut usize) -> Result<String, String> {
    if b.get(*pos) != Some(&'"') {
        return Err("应为字符串".into());
    }
    *pos += 1;
    let mut out = String::new();
    while let Some(c) = b.get(*pos) {
        match c {
            '"' => {
                *pos += 1;
                return Ok(out);
            }
            '\\' => {
                *pos += 1;
                match b.get(*pos) {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('r') => out.push('\r'),
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some('/') => out.push('/'),
                    Some('u') => {
                        let hex: String = b[*pos + 1..(*pos + 5).min(b.len())].iter().collect();
                        if let Ok(n) = u32::from_str_radix(&hex, 16) {
                            if let Some(ch) = char::from_u32(n) {
                                out.push(ch);
                            }
                        }
                        *pos += 4;
                    }
                    _ => return Err("非法转义".into()),
                }
                *pos += 1;
            }
            c => {
                out.push(*c);
                *pos += 1;
            }
        }
    }
    Err("字符串未闭合".into())
}

struct JsonTable {
    header: Vec<String>,
    rows: Vec<Vec<String>>,
    has_header: bool,
}

fn to_table(v: JsonValue) -> Result<JsonTable, String> {
    match v {
        JsonValue::Arr(arr) if arr.iter().all(|x| matches!(x, JsonValue::Obj(_))) => {
            let mut header: Vec<String> = Vec::new();
            for x in &arr {
                if let JsonValue::Obj(m) = x {
                    for (k, _) in m {
                        if !header.contains(k) {
                            header.push(k.clone());
                        }
                    }
                }
            }
            let rows: Vec<Vec<String>> = arr
                .iter()
                .map(|x| {
                    header
                        .iter()
                        .map(|k| match x {
                            JsonValue::Obj(m) => m
                                .iter()
                                .find(|(mk, _)| mk == k)
                                .map(|(_, v)| value_text(v))
                                .unwrap_or_default(),
                            _ => String::new(),
                        })
                        .collect()
                })
                .collect();
            Ok(JsonTable { header, rows, has_header: true })
        }
        JsonValue::Arr(arr) => {
            let rows: Vec<Vec<String>> = arr
                .iter()
                .map(|x| match x {
                    JsonValue::Arr(r) => r.iter().map(value_text).collect(),
                    other => vec![value_text(other)],
                })
                .collect();
            Ok(JsonTable { header: Vec::new(), rows, has_header: false })
        }
        JsonValue::Obj(m) => Ok(JsonTable {
            header: m.iter().map(|(k, _)| k.clone()).collect(),
            rows: vec![m.iter().map(|(_, v)| value_text(v)).collect()],
            has_header: true,
        }),
        _ => Err("JSON 应为对象数组 / 数组的数组 / 单个对象".into()),
    }
}

fn value_text(v: &JsonValue) -> String {
    match v {
        JsonValue::Str(s) => s.clone(),
        JsonValue::Num(n, is_int) => {
            if *is_int {
                format!("{}", *n as i64)
            } else {
                format!("{n}")
            }
        }
        JsonValue::Bool(b) => b.to_string(),
        JsonValue::Null => String::new(),
        _ => String::new(),
    }
}

fn json_to_csv(table: &JsonTable, header: bool) -> String {
    let mut out = String::new();
    if header && table.has_header {
        out.push_str(
            &table
                .header
                .iter()
                .map(|h| csv_escape(h, ','))
                .collect::<Vec<_>>()
                .join(","),
        );
        out.push('\n');
    }
    for r in &table.rows {
        out.push_str(
            &r.iter()
                .map(|c| csv_escape(c, ','))
                .collect::<Vec<_>>()
                .join(","),
        );
        out.push('\n');
    }
    out
}

struct CsvkitWasm;

impl yihu_wasm_runtime::QueryHandler for CsvkitWasm {
    fn handle(text: &str, now: i64) -> Vec<Row> {
    let _ = now;
    // 输入格式：mode \x1f delim \x1f header \x1f text（宿主桥打包）
    let parts: Vec<&str> = text.split('\x1f').collect();
    if parts.len() < 4 {
        return vec![Row::new("✗ 输入格式错误", "宿主桥打包异常", "")];
    }
    let mode = parts[0];
    let delim_spec = parts[1];
    let header = parts[2] == "1";
    let input = parts[3..].join("\x1f");

    if input.trim().is_empty() {
        return vec![Row::new("✗ 输入为空", "粘贴 CSV 或 JSON 后重试", "")];
    }
    let delim = if delim_spec == "auto" {
        detect_delim(&input)
    } else {
        delim_spec.chars().next().unwrap_or(',')
    };
    match if mode == "c2j" {
        csv_to_json(&input, delim, header).map(|s| (s, "JSON"))
    } else {
        json_parse(&input)
            .map(|t| (json_to_csv(&t, header), "CSV"))
            .map_err(|e| e)
    } {
        Ok((out, label)) => {
            vec![Row::new(
                &format!("✓ {label} · 转换完成"),
                "点「复制」取全文",
                out,
            )]
        }
        Err(e) => vec![Row::new(&format!("✗ {e}"), "修正输入后重试", "")],
    }
    }
}

yihu_wasm_runtime::export_plugin!(CsvkitWasm);
