//! 时间戳转换插件（协议 v0：stdio 行式 JSON）。
//!
//! 查询：10/13 位 Unix 时间戳 → UTC 日期时间；YYYY-MM-DD → Unix 秒。
//! 激活：宿主把 payload 复制到剪贴板。

use serde_json::{json, Value};
use std::io::{BufRead, Write};

fn main() {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match msg["type"].as_str() {
            Some("init") => {
                let _ = writeln!(out, r#"{{"type":"ready"}}"#);
            }
            Some("query") => {
                let text = msg["text"].as_str().unwrap_or("").trim().to_string();
                for item in handle_query(&text) {
                    let _ = writeln!(out, "{item}");
                }
            }
            Some("activate") => {} // v0 激活由宿主完成（复制 payload）
            _ => {}
            }
        out.flush().ok();
    }
}

fn handle_query(text: &str) -> Vec<Value> {
    let mut out = Vec::new();

    // 纯数字：10 位按秒、13 位按毫秒
    if let Ok(n) = text.parse::<i64>() {
        if (10..=13).contains(&text.len()) {
            let secs = if text.len() == 13 { n / 1000 } else { n };
            out.push(json!({
                "title": format!("{}（UTC）", fmt_secs(secs)),
                "subtitle": format!("Unix 时间戳 {secs}s · 回车复制"),
                "icon": "",
                "payload": fmt_secs(secs),
            }));
        }
    }

    // YYYY-MM-DD → Unix 秒
    let parts: Vec<&str> = text.split('-').collect();
    if parts.len() == 3 {
        if let (Ok(y), Ok(m), Ok(d)) = (
            parts[0].parse::<i64>(),
            parts[1].parse::<i64>(),
            parts[2].parse::<i64>(),
        ) {
            if (1..=12).contains(&m) && (1..=31).contains(&d) {
                let secs = days_from_civil(y, m, d) * 86400;
                out.push(json!({
                    "title": format!("{secs}"),
                    "subtitle": format!("{text} 的 Unix 秒 · 回车复制"),
                    "icon": "",
                    "payload": format!("{secs}"),
                }));
            }
        }
    }
    out
}

fn fmt_secs(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

// ---- 民用历换算（Howard Hinnant 算法，公有领域）----

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (y + if m <= 2 { 1 } else { 0 }, m, d)
}
