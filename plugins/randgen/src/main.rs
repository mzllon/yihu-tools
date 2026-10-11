//! 随机数生成器：整数 / 浮点 / UUID / 硬币 / 骰子。
//! 整数与骰子走 OS 级 CSPRNG（rand::thread_rng 底层 getrandom），
//! 抽奖/抽样场景不可预测。

use std::cell::Cell;

use rand::Rng;
use serde_json::{json, Value};
use yihu_plugin_sdk::prelude::*;

const TYPES: &[(&str, &str)] = &[
    ("int", "整数"),
    ("float", "浮点（2 位小数）"),
    ("uuid", "UUID"),
    ("coin", "抛硬币"),
    ("dice", "掷骰子"),
];

struct Randgen {
    ui_seq: Cell<u64>,
}

impl Randgen {
    fn next_ui(&self) -> u64 {
        self.ui_seq.set(self.ui_seq.get() + 1);
        self.ui_seq.get()
    }
}

fn gen_one(kind: &str, lo: i64, hi: i64) -> String {
    let (a, b) = if lo <= hi { (lo, hi) } else { (hi, lo) };
    let mut rng = rand::thread_rng();
    match kind {
        "float" => format!("{:.2}", rng.gen_range(a as f64..=b as f64)),
        "uuid" => uuid::Uuid::new_v4().to_string(),
        "coin" => if rng.gen_bool(0.5) { "正面" } else { "反面" }.into(),
        "dice" => (rng.gen_range(0..6) + 1).to_string(),
        _ => rng.gen_range(a..=b).to_string(), // int
    }
}

fn rows_for(kind: &str, lo: i64, hi: i64, count: u64) -> Vec<Value> {
    let sub = match kind {
        "int" => "密码学安全随机 · 点「复制」",
        "float" => "范围内均匀分布 · 点「复制」",
        "uuid" => "UUID v4 · 点「复制」",
        "coin" => "点「复制」",
        _ => "1–6 · 点「复制」",
    };
    (1..=count.clamp(1, 20))
        .map(|i| {
            let v = gen_one(kind, lo, hi);
            item(&v, &format!("第 {i} 个 · {sub}"), &v)
        })
        .collect()
}

fn ui_spec(lo: i64, hi: i64, count: u64, kind: &str, rows: Vec<Value>) -> Value {
    let choices: Vec<Value> = TYPES
        .iter()
        .map(|(value, label)| json!({"label": label, "value": value}))
        .collect();
    let mut spec = json!({
        "title": "随机数生成器",
        "fields": [
            {"key": "lo", "kind": "number", "label": "最小值",
             "min": -1_000_000_000i64, "max": 1_000_000_000i64, "value": lo},
            {"key": "hi", "kind": "number", "label": "最大值",
             "min": -1_000_000_000i64, "max": 1_000_000_000i64, "value": hi},
            {"key": "count", "kind": "number", "label": "生成个数",
             "min": 1, "max": 20, "value": count},
            {"key": "kind", "kind": "select", "label": "类型",
             "choices": choices, "value": kind}
        ],
        "actions": [{"key": "gen", "label": "生成"}]
    });
    if !rows.is_empty() {
        spec["output"] = json!({"rows": rows});
    }
    spec
}

impl Handler for Randgen {
    fn on_query(&self, _qid: u64, text: &str, _files: &[String]) {
        let low = text.trim().to_lowercase();
        let prefixed = low.starts_with("rand ") || low.starts_with("random ");
        if !matches!(low.as_str(), "rand" | "随机数" | "random" | "uuid") && !prefixed {
            return;
        }
        let (lo, hi, kind) = if low == "uuid" {
            (1, 100, "uuid")
        } else if prefixed {
            let rest = text.split_once(' ').map(|(_, r)| r.trim()).unwrap_or("");
            match rest.split_once('-') {
                Some((a, b)) => match (a.trim().parse::<i64>(), b.trim().parse::<i64>()) {
                    (Ok(lo), Ok(hi)) => (lo, hi, "int"),
                    _ => (1, 100, "int"),
                },
                _ => (1, 100, "int"),
            }
        } else {
            (1, 100, "int")
        };
        let ui = self.next_ui();
        let rows = if low == "uuid" || prefixed {
            rows_for(kind, lo, hi, 5)
        } else {
            Vec::new()
        };
        out_ui_show(ui, &ui_spec(lo, hi, 5, kind, rows));
    }

    fn on_ui_event(&self, ui_id: u64, event: &str, values: &Value) {
        if event != "action:gen" {
            return;
        }
        let kind = values.get("kind").and_then(|v| v.as_str()).unwrap_or("int");
        let lo = values.get("lo").and_then(|v| v.as_i64()).unwrap_or(1);
        let hi = values.get("hi").and_then(|v| v.as_i64()).unwrap_or(100);
        let count = values.get("count").and_then(|v| v.as_u64()).unwrap_or(5);
        out_ui_show(
            ui_id,
            &ui_spec(lo, hi, count, kind, rows_for(kind, lo, hi, count)),
        );
    }
}

fn main() {
    run(Randgen { ui_seq: Cell::new(100) });
}
