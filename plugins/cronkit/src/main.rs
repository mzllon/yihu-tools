//! cron 表达式工具：解析 → 人话解读 + 下 N 次执行预览（纯本地）。
//! 5 段标准语法（* , - /）与 @hourly 等特殊串；星期 0/7 = 周日；
//! 「日」与「周」同时受限按 cron 语义取并集。

use std::cell::Cell;

use chrono::{Datelike, Duration as ChronoDuration, NaiveDateTime, Timelike};
use serde_json::{json, Value};
use yihu_plugin_sdk::prelude::*;

const SPECIAL: &[(&str, &str)] = &[
    ("@hourly", "0 * * * *"),
    ("@daily", "0 0 * * *"),
    ("@midnight", "0 0 * * *"),
    ("@weekly", "0 0 * * 0"),
    ("@monthly", "0 0 1 * *"),
    ("@yearly", "0 0 1 1 *"),
    ("@annually", "0 0 1 1 *"),
];

const WEEK: [&str; 7] = ["周日", "周一", "周二", "周三", "周四", "周五", "周六"];

struct Expr {
    parts: [String; 5],
    mins: Vec<u32>,
    hrs: Vec<u32>,
    doms: Vec<u32>,
    mons: Vec<u32>,
    dows: Vec<u32>,
}

fn parse_field(f: &str, lo: u32, hi: u32) -> Result<Vec<u32>, String> {
    let mut vals = Vec::new();
    for part in f.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err("存在空字段".into());
        }
        if let Some((base, step)) = part.split_once('/') {
            let step: u32 = step.parse().map_err(|_| "步长非法")?;
            if step < 1 {
                return Err("步长须 ≥ 1".into());
            }
            let start = if base == "*" || base.is_empty() {
                lo
            } else {
                base.parse().map_err(|_| "数字非法")?
            };
            let mut v = start;
            while v <= hi {
                vals.push(v);
                v += step;
            }
        } else if let Some((a, b)) = part.split_once('-') {
            let a: u32 = a.parse().map_err(|_| "数字非法")?;
            let b: u32 = b.parse().map_err(|_| "数字非法")?;
            if a > b {
                return Err(format!("区间起大于止：{a}-{b}"));
            }
            vals.extend(a..=b);
        } else if part == "*" {
            vals.extend(lo..=hi);
        } else {
            vals.push(part.parse().map_err(|_| "数字非法")?);
        }
    }
    vals.retain(|v| (lo..=hi).contains(v));
    vals.sort_unstable();
    if vals.is_empty() {
        return Err("取值全部越界".into());
    }
    Ok(vals)
}

fn parse_expr(expr: &str) -> Result<Expr, String> {
    let lowered = expr.trim().to_lowercase();
    let fields = SPECIAL
        .iter()
        .find(|(k, _)| *k == lowered)
        .map(|(_, v)| v.to_string())
        .unwrap_or_else(|| expr.trim().to_string());
    let parts: Vec<String> = fields.split_whitespace().map(String::from).collect();
    if parts.len() != 5 {
        return Err("cron 应为 5 段：分 时 日 月 周".into());
    }
    Ok(Expr {
        mins: parse_field(&parts[0], 0, 59)?,
        hrs: parse_field(&parts[1], 0, 23)?,
        doms: parse_field(&parts[2], 1, 31)?,
        mons: parse_field(&parts[3], 1, 12)?,
        dows: parse_field(&parts[4], 0, 7)?
            .into_iter()
            .map(|v| if v == 7 { 0 } else { v })
            .collect(),
        parts: [
            parts[0].clone(),
            parts[1].clone(),
            parts[2].clone(),
            parts[3].clone(),
            parts[4].clone(),
        ],
    })
}

fn list_text(vals: &[u32], unit: &str, max: usize) -> String {
    let shown: Vec<String> = vals.iter().take(max).map(|v| format!("{unit}{v}")).collect();
    let more = if vals.len() > max { "…" } else { "" };
    format!("{}{}", shown.join("、"), more)
}

fn describe(e: &Expr) -> String {
    let dom_star = e.parts[2] == "*";
    let dow_star = e.parts[4] == "*";
    let mon_txt = if e.parts[3] == "*" {
        None
    } else {
        Some(list_text(&e.mons, "月", 12))
    };
    let day_txt = if dom_star {
        None
    } else {
        Some(list_text(&e.doms, "日", 8))
    };
    let week_txt = if dow_star {
        None
    } else {
        Some(
            e.dows
                .iter()
                .map(|d| WEEK[*d as usize % 7])
                .collect::<Vec<_>>()
                .join("、"),
        )
    };

    let time_txt = if e.mins.len() == 60 {
        "每小时".to_string()
    } else if e.hrs.len() == 24 {
        format!("每小时第 {} 分", list_text(&e.mins, "", 8))
    } else if e.hrs.len() == 1 && e.mins.len() == 1 {
        format!("{:02}:{:02}", e.hrs[0], e.mins[0])
    } else {
        let times: Vec<String> = e
            .hrs
            .iter()
            .take(4)
            .flat_map(|h| e.mins.iter().take(1).map(move |m| format!("{h:02}:{m:02}")))
            .collect();
        let more = if e.hrs.len() > 4 { "…" } else { "" };
        format!("{}{}", times.join("、"), more)
    };

    let when = match (dom_star, dow_star) {
        (true, true) => "每天".to_string(),
        (true, false) => format!("每{}", week_txt.unwrap_or_default()),
        (false, true) => format!("每月{}", day_txt.unwrap_or_default()),
        (false, false) => format!(
            "{} {} 或 {}",
            mon_txt.unwrap_or_else(|| "每月".into()),
            day_txt.unwrap_or_default(),
            week_txt.unwrap_or_default()
        ),
    };
    format!("{when} {time_txt}")
}

fn next_runs(e: &Expr, count: usize) -> Result<Vec<NaiveDateTime>, String> {
    let now = chrono::Local::now().naive_local();
    let mut t = now
        .with_second(0)
        .and_then(|t| t.with_nanosecond(0))
        .ok_or("时间构造失败")?
        + ChronoDuration::minutes(1);
    let limit = t + ChronoDuration::days(366 * 4);
    let mut out = Vec::new();
    while t < limit && out.len() < count {
        if !e.mons.contains(&t.month()) {
            let day1 = t.with_day(1).ok_or("日越界")?;
            t = (day1 + ChronoDuration::days(32))
                .with_day(1)
                .and_then(|d| d.with_hour(0))
                .and_then(|d| d.with_minute(0))
                .ok_or("时间构造失败")?;
            continue;
        }
        let dom_ok = e.doms.contains(&t.day());
        let dow_ok = e.dows.contains(&(t.weekday().num_days_from_sunday()));
        let day_ok = match (e.parts[2] == "*", e.parts[4] == "*") {
            (true, true) => true,
            (true, false) => dow_ok,
            (false, true) => dom_ok,
            (false, false) => dom_ok || dow_ok,
        };
        if !day_ok {
            t = (t + ChronoDuration::days(1))
                .with_hour(0)
                .and_then(|d| d.with_minute(0))
                .ok_or("时间构造失败")?;
            continue;
        }
        if e.hrs.contains(&t.hour()) && e.mins.contains(&t.minute()) {
            out.push(t);
        }
        t += ChronoDuration::minutes(1);
    }
    if out.len() < count {
        return Err("4 年内没有匹配的执行时间（如 2 月 30 日）".into());
    }
    Ok(out)
}

fn rows_for(expr: &str, count: usize) -> Result<Vec<Value>, String> {
    let e = parse_expr(expr)?;
    let mut rows = vec![item(&format!("📋 {}", describe(&e)), "表达式解读", "")];
    for t in next_runs(&e, count)? {
        rows.push(item(
            &t.format("%Y-%m-%d %H:%M (%a)").to_string(),
            "下次执行 · 点「复制」",
            t.format("%Y-%m-%d %H:%M:%S").to_string(),
        ));
    }
    Ok(rows)
}

fn ui_spec(expr: &str, count: u64, rows: Vec<Value>) -> Value {
    let mut spec = json!({
        "title": "cron 表达式",
        "fields": [
            {"key": "expr", "kind": "text",
             "label": "表达式（5 段：分 时 日 月 周；支持 @daily 等）",
             "value": expr, "placeholder": "如 0 9 * * 1-5"},
            {"key": "count", "kind": "number", "label": "预览条数",
             "min": 1, "max": 10, "value": count}
        ],
        "actions": [{"key": "run", "label": "解析"}]
    });
    if !rows.is_empty() {
        spec["output"] = json!({"rows": rows});
    }
    spec
}

struct Cronkit {
    ui_seq: Cell<u64>,
}

impl Cronkit {
    fn next_ui(&self) -> u64 {
        self.ui_seq.set(self.ui_seq.get() + 1);
        self.ui_seq.get()
    }
}

impl Handler for Cronkit {
    fn on_query(&self, _qid: u64, text: &str, _files: &[String]) {
        let low = text.trim().to_lowercase();
        let prefixed = low.starts_with("cron ") || low.starts_with("crontab ");
        if !matches!(low.as_str(), "cron" | "crontab" | "定时") && !prefixed {
            return;
        }
        let expr = if prefixed {
            text.split_once(' ')
                .map(|(_, r)| r.trim().to_string())
                .unwrap_or_default()
        } else {
            String::new()
        };
        let ui = self.next_ui();
        let rows = if expr.is_empty() {
            Vec::new()
        } else {
            rows_for(&expr, 5).unwrap_or_else(|e| vec![item(&format!("✗ {e}"), "修正表达式后重试", "")])
        };
        out_ui_show(ui, &ui_spec(&expr, 5, rows));
    }

    fn on_ui_event(&self, ui_id: u64, event: &str, values: &Value) {
        if event != "action:run" {
            return;
        }
        let expr = values
            .get("expr")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let count = values.get("count").and_then(|v| v.as_u64()).unwrap_or(5);
        let rows = if expr.is_empty() {
            Vec::new()
        } else {
            rows_for(&expr, count.clamp(1, 10) as usize)
                .unwrap_or_else(|e| vec![item(&format!("✗ {e}"), "修正表达式后重试", "")])
        };
        out_ui_show(ui_id, &ui_spec(&expr, count, rows));
    }
}

fn main() {
    run(Cronkit { ui_seq: Cell::new(100) });
}
