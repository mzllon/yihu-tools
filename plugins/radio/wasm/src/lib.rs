//! 电台 mini 壳（面板出行入口）：native 插件 radio 的 panel_entry。
//!
//! 职责（docs/电台native插件设计.md §4.7 / §6.1）：
//! - 词头命中出「📻 电台控制台」入口行（回车 → 宿主 spawn `radio-player --open`）
//! - 播放状态行：读插件数据目录 status.json（native 端写），正在播时置顶
//!   「🔊 正在播放 · 回车停止」
//! - 面板直搜：`电台 <关键词>` 经 network.fetch 搜直播台，回车即播
//!   （宿主 spawn `--play <id> <标题>`，只出声不弹窗）
//!
//! 动作 payload（! 前缀由宿主 dispatch 剥掉）：open / stop / play:<id>|<标题>。
//! 宿主对 native 插件拦截止这些 payload 直接拉起窗体进程；mini 壳的
//! activate 不承担行为。

use std::sync::atomic::{AtomicI32, Ordering};

use yihu_wasm_runtime::cap;
use yihu_wasm_runtime::{export_plugin, PluginHandler, Row};

const KEYWORDS: &[&str] = &["radio", "电台", "广播", "fm", "收音机"];
/// status.json 契约路径（native 端 store::data_dir 写；两端同字面量）
const STATUS_PATH: &str = "~/.local/share/yihu/plugin-data/radio/status.json";
const SEARCH_API: &str = "https://search.qingting.fm/v3/search?k=";

/// 播放状态读取的在途请求 id（只认最近一次，迟到丢弃）
static STATUS_RID: AtomicI32 = AtomicI32::new(-1);
/// 搜索的在途请求 id（同上）
static SEARCH_RID: AtomicI32 = AtomicI32::new(-1);
/// 最近一次查询词（状态行/搜索行只服务当前查询）。
/// wasm 单线程执行模型，static mut 与 SDK 的 ROWS 同一安全口径。
static mut LAST_QUERY: Option<String> = None;

fn last_query_store(text: &str) {
    unsafe { LAST_QUERY = Some(text.to_string()) };
}

fn last_query_load() -> String {
    unsafe { LAST_QUERY.clone().unwrap_or_default() }
}

fn entry_row() -> Row {
    Row::new(
        "📻 电台控制台",
        "回车打开播放器（收藏 / 分类 / 地区 / 搜索）",
        "open",
    )
}

fn playing_row(title: &str) -> Row {
    Row::new(
        &format!("🔊 正在播放：{title} · 回车停止"),
        "后台播放中 · 关窗不中断",
        "stop",
    )
}

/// 拆出搜索词（"电台 xyz" → "xyz"；裸词头 → None）
fn search_term(text: &str) -> Option<String> {
    let rest = text.split_once(char::is_whitespace)?.1.trim();
    (!rest.is_empty()).then(|| rest.to_string())
}

pub struct RadioPanel;

impl PluginHandler for RadioPanel {
    fn handle(text: &str, _now_secs: i64) -> Vec<Row> {
        let first = text.trim().split_whitespace().next().unwrap_or("").to_lowercase();
        if first.is_empty() || !KEYWORDS.iter().any(|k| first.contains(k)) {
            return vec![];
        }
        last_query_store(text.trim());

        match search_term(text.trim()) {
            Some(term) => {
                // 面板直搜：占位行 + 异步回包出播放行（迟到回包按请求 id 丢弃）。
                // rid < 0 = 请求未入队（桩/队列满），不登记。
                let rid = cap::network_fetch(&format!("{SEARCH_API}{}", encode(&term)));
                if rid >= 0 {
                    SEARCH_RID.store(rid, Ordering::SeqCst);
                }
                vec![Row::new(
                    &format!("🔍 在电台中搜索「{term}」…"),
                    "正在搜索直播台",
                    "open",
                )]
            }
            None => {
                // 控制台入口 + 播放状态行（异步回包后重出行）
                let rid = cap::fs_read(STATUS_PATH);
                if rid >= 0 {
                    STATUS_RID.store(rid, Ordering::SeqCst);
                }
                vec![entry_row()]
            }
        }
    }

    fn on_response(id: i32, ok: bool, data: &[u8]) {
        let text = last_query_load();
        let first = text.split_whitespace().next().unwrap_or("").to_lowercase();
        if first.is_empty() || !KEYWORDS.iter().any(|k| first.contains(k)) {
            return;
        }

        // —— 搜索回包 ——（rid ≥ 1 才有效；-1 是「无在途」哨兵）
        let search_rid = SEARCH_RID.load(Ordering::SeqCst);
        if search_rid >= 0 && id == search_rid {
            SEARCH_RID.store(-1, Ordering::SeqCst);
            if !ok {
                let _ = yihu_wasm_runtime::__rows_store(vec![
                    Row::new("⚠ 电台搜索失败", "网络不可用或服务变更", "open"),
                    entry_row(),
                ]);
                return;
            }
            let mut rows = Vec::new();
            if let Some(list) = parse_search(data) {
                for ch in list.iter().take(8) {
                    rows.push(Row::new(
                        &ch.title,
                        &ch.sub,
                        &format!("play:{}|{}", ch.id, ch.title),
                    ));
                }
            }
            if rows.is_empty() {
                let term = search_term(&text).unwrap_or_default();
                rows.push(Row::new(
                    &format!("没有找到「{term}」的直播台"),
                    "到播放器里用分类/地区浏览",
                    "open",
                ));
            }
            yihu_wasm_runtime::__rows_store(rows);
            return;
        }

        // —— 播放状态回包 ——
        let status_rid = STATUS_RID.load(Ordering::SeqCst);
        if status_rid >= 0 && id == status_rid {
            STATUS_RID.store(-1, Ordering::SeqCst);
            let mut rows = Vec::new();
            if ok {
                if let Some(title) = playing_title(data) {
                    rows.push(playing_row(&title));
                }
            }
            rows.push(entry_row());
            yihu_wasm_runtime::__rows_store(rows);
        }
    }
}

struct SearchHit {
    id: i64,
    title: String,
    sub: String,
}

/// 搜索响应（JSON 字符串被宿主包了一层 JSON 引号）→ 直播台行
fn parse_search(data: &[u8]) -> Option<Vec<SearchHit>> {
    let raw: &str = core::str::from_utf8(data).ok()?;
    let body: String = serde_json::from_str(raw).ok()?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    let docs = v.pointer("/data/data/docs")?.as_array()?;
    let mut out = Vec::new();
    for d in docs {
        if d.get("type").and_then(|t| t.as_str()) != Some("channel_live") {
            continue;
        }
        let id = d.get("id").and_then(|i| i.as_i64())?;
        let title = d
            .get("title")
            .and_then(|t| t.as_str())
            .unwrap_or("未命名电台")
            .chars()
            .take(60)
            .collect::<String>();
        let aud = d
            .get("audience_count")
            .map(|a| match a {
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::String(s) => s.clone(),
                _ => String::new(),
            })
            .unwrap_or_default();
        let sub = if aud.is_empty() {
            "回车即播".to_string()
        } else {
            format!("{aud} 听众 · 回车即播")
        };
        out.push(SearchHit { id, title, sub });
    }
    Some(out)
}

/// status.json → 正在播的台名（不在播 = None）
fn playing_title(data: &[u8]) -> Option<String> {
    let raw: &str = core::str::from_utf8(data).ok()?;
    let body: String = serde_json::from_str(raw).ok()?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    if v.get("playing").and_then(|p| p.as_bool()) != Some(true) {
        return None;
    }
    v.pointer("/channel/title")
        .and_then(|t| t.as_str())
        .map(|s| s.chars().take(60).collect())
}

/// 搜索词 percent-encode（保守子集，与 radio-player api.rs 同实现）
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

export_plugin!(RadioPanel);



