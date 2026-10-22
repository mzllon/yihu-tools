//! Markdown 工作台 mini 壳（面板出行入口）：native 插件 mdworkbench 的
//! panel_entry。词头命中出「📝 Markdown 工作台」入口行（回车 → 宿主
//! spawn `mdworkbench --open`）。本地工具，无直搜/无状态行。

use yihu_wasm_runtime::{export_plugin, PluginHandler, Row};

const KEYWORDS: &[&str] = &["md", "markdown", "工作台", "笔记", "mdworkbench"];

fn entry_row() -> Row {
    Row::new(
        "📝 Markdown 工作台",
        "回车打开所见即所得编辑器（md / markdown）",
        "open",
    )
}

pub struct MdworkbenchPanel;

impl PluginHandler for MdworkbenchPanel {
    fn handle(text: &str, _now_secs: i64) -> Vec<Row> {
        let first = text
            .trim()
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_lowercase();
        if first.is_empty() || !KEYWORDS.iter().any(|k| first.contains(k)) {
            return vec![];
        }
        vec![entry_row()]
    }

    fn on_response(_id: i32, _ok: bool, _data: &[u8]) {}
}

export_plugin!(MdworkbenchPanel);
