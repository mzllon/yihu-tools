//! 随机密码生成器：随机字符 / 数字 PIN / 易读短语。
//! 随机源 = rand::thread_rng（OS CSPRNG）；每类选中字符集至少贡献一位
//! （Fisher–Yates 洗牌）；易读短语 = 音节词 + 数字尾缀，好念好口头转述。

use std::cell::Cell;

use rand::Rng;
use serde_json::{json, Value};
use yihu_plugin_sdk::prelude::*;

const SETS: &[(&str, &str, char)] = &[
    ("upper", "ABCDEFGHIJKLMNOPQRSTUVWXYZ", 'u'),
    ("lower", "abcdefghijklmnopqrstuvwxyz", 'l'),
    ("digit", "0123456789", 'd'),
    ("sym", "!@#$%^&*", 's'),
];
const SYMS: &str = "!@#$%^&*";
const CONS: &str = "bcdfghjklmnpqrstvwxz";
const VOW: &str = "aeiou";

struct Passgen {
    ui_seq: Cell<u64>,
}

impl Passgen {
    fn next_ui(&self) -> u64 {
        self.ui_seq.set(self.ui_seq.get() + 1);
        self.ui_seq.get()
    }
}

fn shuffle(chars: &mut Vec<char>) {
    let mut rng = rand::thread_rng();
    for i in (1..chars.len()).rev() {
        let j = rng.gen_range(0..=i);
        chars.swap(i, j);
    }
}

fn gen_random(length: usize, on: &[char]) -> Option<String> {
    let pools: Vec<&str> = SETS
        .iter()
        .filter(|(_, _, k)| on.contains(k))
        .map(|(_, s, _)| *s)
        .collect();
    if pools.is_empty() {
        return None;
    }
    let mut rng = rand::thread_rng();
    let length = length.max(pools.len()); // 每类至少一位
    let mut chars: Vec<char> = pools
        .iter()
        .map(|p| p.chars().nth(rng.gen_range(0..p.len())).unwrap())
        .collect();
    let pool: String = pools.concat();
    while chars.len() < length {
        chars.push(pool.chars().nth(rng.gen_range(0..pool.len())).unwrap());
    }
    shuffle(&mut chars);
    Some(chars.into_iter().collect())
}

fn gen_pin(length: usize) -> String {
    let mut rng = rand::thread_rng();
    (0..length.max(4))
        .map(|_| char::from(b'0' + rng.gen_range(0..10u8)))
        .collect()
}

fn gen_phrase(length: usize, sym: bool) -> String {
    let mut rng = rand::thread_rng();
    let mut syllables = Vec::new();
    let mut total = 0;
    while total < length.max(8) {
        let syl: String = [
            CONS.chars().nth(rng.gen_range(0..CONS.len())).unwrap(),
            VOW.chars().nth(rng.gen_range(0..VOW.len())).unwrap(),
            CONS.chars().nth(rng.gen_range(0..CONS.len())).unwrap(),
        ]
        .iter()
        .collect();
        let mut cs = syl.chars();
        let cap: String = cs.next().map(|c| c.to_uppercase().to_string()).unwrap_or_default()
            + cs.as_str();
        syllables.push(cap);
        total += 3;
    }
    let mut tail: String = (0..2)
        .map(|_| char::from(b'0' + rng.gen_range(0..10u8)))
        .collect();
    if sym {
        tail.push(SYMS.chars().nth(rng.gen_range(0..SYMS.len())).unwrap());
    }
    format!("{}-{}", syllables.join("-"), tail)
}

fn rows_for(style: &str, length: u64, count: u64, on: &serde_json::Map<String, Value>) -> Vec<Value> {
    let on_keys: Vec<char> = ["upper", "lower", "digit", "sym"]
        .iter()
        .filter(|k| on.get(**k).and_then(|v| v.as_bool()).unwrap_or(false))
        .map(|k| k.chars().next().unwrap())
        .collect();
    if style == "random" && on_keys.is_empty() {
        return vec![item("✗ 至少选择一类字符", "打开上方字符集开关后重试", "")];
    }
    let label = match style {
        "pin" => "数字 PIN",
        "phrase" => "易读短语",
        _ => "随机字符",
    };
    (1..=count.clamp(1, 10))
        .map(|i| {
            let (pwd, sub) = match style {
                "pin" => (gen_pin(length as usize), format!("{length} 位数字")),
                "phrase" => (
                    gen_phrase(length as usize, on.get("sym").and_then(|v| v.as_bool()).unwrap_or(true)),
                    "音节短语 · 长度近似".to_string(),
                ),
                _ => (
                    gen_random(length as usize, &on_keys).unwrap_or_default(),
                    format!("{length} 位 · 每类字符至少一位"),
                ),
            };
            item(&pwd, &format!("{label} · {sub} · 点「复制」"), &pwd)
        })
        .collect()
}

fn ui_spec(
    length: u64,
    count: u64,
    style: &str,
    on: &serde_json::Map<String, Value>,
    rows: Vec<Value>,
) -> Value {
    let b = |k: &str, d: bool| on.get(k).and_then(|v| v.as_bool()).unwrap_or(d);
    let mut spec = json!({
        "title": "随机密码生成器",
        "fields": [
            {"key": "length", "kind": "number", "label": "长度",
             "min": 6, "max": 64, "value": length},
            {"key": "count", "kind": "number", "label": "生成个数",
             "min": 1, "max": 10, "value": count},
            {"key": "style", "kind": "select", "label": "风格",
             "choices": [
                {"label": "随机字符", "value": "random"},
                {"label": "数字 PIN", "value": "pin"},
                {"label": "易读短语", "value": "phrase"}
             ], "value": style},
            {"key": "upper", "kind": "switch", "label": "大写字母 A-Z", "value": b("upper", true)},
            {"key": "lower", "kind": "switch", "label": "小写字母 a-z", "value": b("lower", true)},
            {"key": "digit", "kind": "switch", "label": "数字 0-9", "value": b("digit", true)},
            {"key": "sym", "kind": "switch", "label": "符号 !@#$%^&*", "value": b("sym", true)}
        ],
        "actions": [{"key": "gen", "label": "生成"}]
    });
    if !rows.is_empty() {
        spec["output"] = json!({"rows": rows});
    }
    spec
}

impl Handler for Passgen {
    fn on_query(&self, _qid: u64, text: &str, _files: &[String]) {
        let low = text.trim().to_lowercase();
        let prefixed = low.starts_with("pass ") || low.starts_with("密码 ");
        if !matches!(low.as_str(), "pass" | "密码" | "passgen") && !prefixed {
            return;
        }
        let length = if prefixed {
            text.split_once(' ')
                .and_then(|(_, r)| r.trim().parse::<u64>().ok())
                .map(|n| n.clamp(6, 64))
                .unwrap_or(16)
        } else {
            16
        };
        let ui = self.next_ui();
        out_ui_show(
            ui,
            &ui_spec(
                length,
                3,
                "random",
                &serde_json::Map::new(),
                Vec::new(),
            ),
        );
    }

    fn on_ui_event(&self, ui_id: u64, event: &str, values: &Value) {
        if event != "action:gen" {
            return;
        }
        let length = values.get("length").and_then(|v| v.as_u64()).unwrap_or(16);
        let count = values.get("count").and_then(|v| v.as_u64()).unwrap_or(3);
        let style = values
            .get("style")
            .and_then(|v| v.as_str())
            .unwrap_or("random")
            .to_string();
        let on = values.as_object().cloned().unwrap_or_default();
        out_ui_show(
            ui_id,
            &ui_spec(length, count, &style, &on, rows_for(&style, length, count, &on)),
        );
    }
}

fn main() {
    run(Passgen { ui_seq: Cell::new(100) });
}
