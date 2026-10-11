//! CSV ⇄ JSON 转换：首行表头开关、分隔符自动识别（, ; Tab 按首行计数）、
//! JSON 对象数组键并集、统计行 + 全文复制。

use std::cell::Cell;

use serde_json::{json, Map, Value};
use yihu_plugin_sdk::prelude::*;

struct Csvkit {
    ui_seq: Cell<u64>,
}

impl Csvkit {
    fn next_ui(&self) -> u64 {
        self.ui_seq.set(self.ui_seq.get() + 1);
        self.ui_seq.get()
    }
}

fn detect_delim(text: &str) -> char {
    let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let (comma, semi, tab) = (
        first.matches(',').count(),
        first.matches(';').count(),
        first.matches('\t').count(),
    );
    if semi > comma && semi > tab {
        ';'
    } else if tab > comma && tab > semi {
        '\t'
    } else {
        ','
    }
}

fn csv_to_json(text: &str, delim: char, header: bool) -> Result<(String, usize, usize), String> {
    let mut rows: Vec<Vec<String>> = csv::ReaderBuilder::new()
        .delimiter(delim as u8)
        .has_headers(false)
        .flexible(true)
        .from_reader(text.as_bytes())
        .records()
        .filter_map(|r| r.ok())
        .map(|r| r.iter().map(String::from).collect())
        .collect();
    rows.retain(|r| r.iter().any(|c| !c.trim().is_empty()));
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
        let objs: Vec<Map<String, Value>> = rows[1..]
            .iter()
            .map(|r| {
                let mut m = Map::new();
                for (i, k) in keys.iter().enumerate() {
                    m.insert(k.clone(), json!(r.get(i).cloned().unwrap_or_default()));
                }
                m
            })
            .collect();
        let cols = keys.len();
        let out = serde_json::to_string_pretty(&objs).map_err(|e| e.to_string())?;
        Ok((out, objs.len(), cols))
    } else {
        let cols = rows[0].len();
        let out = serde_json::to_string_pretty(&rows).map_err(|e| e.to_string())?;
        Ok((out, rows.len(), cols))
    }
}

fn cell_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Object(_) | Value::Array(_) => serde_json::to_string(v).unwrap_or_default(),
        other => other.to_string(),
    }
}

fn json_to_csv(text: &str, header: bool) -> Result<(String, usize, usize), String> {
    let obj: Value = serde_json::from_str(text).map_err(|e| format!("坏 JSON：{e}"))?;
    let mut w = csv::WriterBuilder::new().from_writer(vec![]);
    match &obj {
        Value::Array(arr) if arr.iter().all(|x| x.is_object()) => {
            let mut keys: Vec<String> = Vec::new();
            for x in arr {
                if let Some(m) = x.as_object() {
                    for k in m.keys() {
                        if !keys.contains(k) {
                            keys.push(k.clone());
                        }
                    }
                }
            }
            if header {
                w.write_record(&keys).map_err(|e| e.to_string())?;
            }
            for x in arr {
                let rec: Vec<String> = keys
                    .iter()
                    .map(|k| {
                        x.get(k)
                            .map(cell_text)
                            .unwrap_or_default()
                    })
                    .collect();
                w.write_record(&rec).map_err(|e| e.to_string())?;
            }
            let n = arr.len();
            let out = String::from_utf8(w.into_inner().map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            Ok((out, n, keys.len()))
        }
        Value::Array(arr) => {
            for x in arr {
                let rec: Vec<String> = x
                    .as_array()
                    .map(|r| r.iter().map(cell_text).collect())
                    .unwrap_or_default();
                w.write_record(&rec).map_err(|e| e.to_string())?;
            }
            let n = arr.len();
            let cols = arr.first().and_then(|a| a.as_array()).map(|a| a.len()).unwrap_or(0);
            let out = String::from_utf8(w.into_inner().map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            Ok((out, n, cols))
        }
        Value::Object(m) => {
            w.write_record(m.keys()).map_err(|e| e.to_string())?;
            let vals: Vec<String> = m.values().map(cell_text).collect();
            w.write_record(&vals).map_err(|e| e.to_string())?;
            let out = String::from_utf8(w.into_inner().map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            Ok((out, 1, m.len()))
        }
        _ => Err("JSON 应为对象数组 / 数组的数组 / 单个对象".into()),
    }
}

fn run_convert(
    mode: &str,
    text: &str,
    delim: &str,
    header: bool,
) -> Result<(String, usize, usize), String> {
    if mode == "c2j" {
        let d = if delim == "auto" {
            detect_delim(text)
        } else {
            delim.chars().next().unwrap_or(',')
        };
        csv_to_json(text, d, header)
    } else {
        json_to_csv(text, header)
    }
}

fn base_spec() -> Value {
    json!({
        "title": "CSV ⇄ JSON",
        "fields": [
            {"key": "mode", "kind": "select", "label": "方向",
             "choices": [{"label": "CSV → JSON", "value": "c2j"},
                          {"label": "JSON → CSV", "value": "j2c"}], "value": "c2j"},
            {"key": "input", "kind": "textarea", "label": "输入（Ctrl+V 粘贴）",
             "value": "", "lines": 9},
            {"key": "delim", "kind": "select", "label": "分隔符（仅 CSV 侧生效）",
             "choices": [{"label": "自动识别", "value": "auto"},
                          {"label": "逗号 ,", "value": ","},
                          {"label": "分号 ;", "value": ";"},
                          {"label": "Tab", "value": "\t"}], "value": "auto"},
            {"key": "header", "kind": "switch", "label": "首行是表头", "value": true}
        ],
        "actions": [{"key": "run", "label": "转换"}]
    })
}

fn spec_with(
    ui_id: u64,
    mode: &str,
    input: &str,
    delim: &str,
    header: bool,
    rows: Vec<Value>,
) -> Value {
    let mut spec = base_spec();
    spec["ui_id"] = json!(ui_id);
    if let Some(fields) = spec["fields"].as_array_mut() {
        for f in fields.iter_mut() {
            match f.get("key").and_then(|k| k.as_str()) {
                Some("mode") => f["value"] = json!(mode),
                Some("input") => f["value"] = json!(input),
                Some("delim") => f["value"] = json!(delim),
                Some("header") => f["value"] = json!(header),
                _ => {}
            }
        }
    }
    spec["output"] = json!({"rows": rows});
    spec
}

impl Handler for Csvkit {
    fn on_query(&self, _qid: u64, text: &str, _files: &[String]) {
        let low = text.trim().to_lowercase();
        if matches!(
            low.as_str(),
            "csv" | "csvjson" | "csv2json" | "json2csv" | "转换"
        ) {
            out_ui_show(self.next_ui(), &base_spec());
        }
    }

    fn on_ui_event(&self, ui_id: u64, event: &str, values: &Value) {
        if event != "action:run" {
            return;
        }
        let mode = values.get("mode").and_then(|v| v.as_str()).unwrap_or("c2j");
        let text = values.get("input").and_then(|v| v.as_str()).unwrap_or("");
        let delim = values.get("delim").and_then(|v| v.as_str()).unwrap_or("auto");
        let header = values.get("header").and_then(|v| v.as_bool()).unwrap_or(true);

        let rows = if text.trim().is_empty() {
            vec![item("✗ 输入为空", "粘贴 CSV 或 JSON 后重试", "")]
        } else {
            match run_convert(mode, text, delim, header) {
                Ok((out, n_rows, n_cols)) => {
                    let label = if mode == "c2j" { "JSON" } else { "CSV" };
                    vec![out_row(
                        &format!("✓ {label} · {n_rows} 行 × {n_cols} 列"),
                        "点「复制」取全文",
                        out,
                    )]
                }
                Err(e) => vec![item(&format!("✗ {e}"), "修正输入后重试", "")],
            }
        };
        out_ui_show(ui_id, &spec_with(ui_id, mode, text, delim, header, rows));
    }
}

fn main() {
    run(Csvkit { ui_seq: Cell::new(100) });
}
