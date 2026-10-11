//! 一呼官方 Rust 插件 SDK（M5 追加）：stdio 行式 JSON 协议 v1 的运行时。
//!
//! 插件 = 实现 [`Handler`] 四个回调的可执行程序，`main` 里调 [`run`]：
//!
//! ```no_run
//! use yihu_plugin_sdk::prelude::*;
//!
//! struct Demo;
//! impl Handler for Demo {
//!     fn on_query(&self, qid: u64, text: &str, _files: &[String]) {
//!         if text == "hi" {
//!             out_results(qid, &[item("你好", "回车复制", "hi!")]);
//!         }
//!     }
//! }
//! fn main() { run(Demo); }
//! ```
//!
//! 协议细节由 SDK 托管：init→ready 自动应答、shutdown 退出、
//! capability_request 发射（id 自增分配）、结果/UI/通知的 JSON 转义。
//! 能力回包经 [`Handler::on_capability_response`] 回调，data 已从 JSON
//! 字符串解码为原始文本（与 Python 插件看到的一致）。

use std::io::{BufRead, Write};

use serde_json::{json, Value};

/// 输入消息（已解析）
#[derive(Debug)]
pub enum Incoming {
    Query { id: u64, text: String, files: Vec<String> },
    Activate { id: u64, payload: String },
    UiEvent { ui_id: u64, event: String, values: Value },
    CapabilityResponse { id: u64, ok: bool, error: String, data: Option<String> },
    /// init：run() 自动应答 ready，插件无需关心
    Init,
    Shutdown,
}

/// 结果行构造器：title 必填，subtitle/payload/icon 可选
pub fn item(title: &str, subtitle: &str, payload: impl Into<String>) -> Value {
    let payload = payload.into();
    json!({"title": title, "subtitle": subtitle, "payload": payload})
}

/// 带图标的结果行
pub fn item_icon(title: &str, subtitle: &str, payload: impl Into<String>, icon: &str) -> Value {
    let mut v = item(title, subtitle, payload);
    v["icon"] = json!(icon);
    v
}

/// 动作型结果行（payload 以 ! 开头，宿主不复制、转发 activate）
pub fn item_action(title: &str, subtitle: &str, action_payload: impl Into<String>) -> Value {
    let action = action_payload.into();
    let mut v = item(title, subtitle, format!("!{action}"));
    v["payload"] = json!(format!("!{action}"));
    v
}

/// 插件回调：协议事件 → 业务逻辑。所有发射经 [`emit`] 系列。
pub trait Handler {
    fn on_query(&self, qid: u64, text: &str, files: &[String]);
    fn on_activate(&self, _qid: u64, _payload: &str) {}
    fn on_ui_event(&self, _ui_id: u64, _event: &str, _values: &Value) {}
    /// data 已解码：host 的 JSON 字符串 → 原始文本
    fn on_capability_response(&self, _rid: u64, _ok: bool, _error: &str, _data: Option<&str>) {}
    fn on_shutdown(&self) {}
}

/// 发射结果集
pub fn out_results(query_id: u64, items: &[Value]) {
    emit(&json!({"type": "results", "query_id": query_id, "items": items}));
}

/// 发射 UI 声明（ui.show）
pub fn out_ui_show(ui_id: u64, spec: &Value) {
    emit(&json!({"type": "ui.show", "ui_id": ui_id, "spec": spec}));
}

/// 发射能力请求（id 由调用方管理；SDK 不做分配，回包按 id 原样回传）
pub fn out_capability_request(id: u64, capability: &str, params: &Value) {
    emit(&json!({
        "type": "capability_request",
        "id": id,
        "capability": capability,
        "params": params
    }));
}

pub fn emit(v: &Value) {
    let line = serde_json::to_string(v).unwrap_or_default();
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

fn parse_line(line: &str) -> Option<Incoming> {
    let v: Value = serde_json::from_str(line).ok()?;
    match v.get("type")?.as_str()? {
        "query" => Some(Incoming::Query {
            id: v.get("id").and_then(|x| x.as_u64()).unwrap_or(0),
            text: v.get("text").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            files: v
                .get("context")
                .and_then(|c| c.get("files"))
                .and_then(|f| f.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default(),
        }),
        "activate" => Some(Incoming::Activate {
            id: v.get("id").and_then(|x| x.as_u64()).unwrap_or(0),
            payload: v.get("payload").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        }),
        "ui.event" => Some(Incoming::UiEvent {
            ui_id: v.get("ui_id").and_then(|x| x.as_u64()).unwrap_or(0),
            event: v.get("event").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            values: v.get("values").cloned().unwrap_or_else(|| json!({})),
        }),
        "capability_response" => Some(Incoming::CapabilityResponse {
            id: v.get("id").and_then(|x| x.as_u64()).unwrap_or(0),
            ok: v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false),
            error: v.get("error").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            // data = 宿主 JSON 编码的字符串 → 解码为原始文本
            data: v.get("data").and_then(|x| x.as_str()).map(|s| s.to_string()),
        }),
        "init" => Some(Incoming::Init),
        "shutdown" => Some(Incoming::Shutdown),
        _ => None,
    }
}

/// 协议主循环：读 stdin → 分发 → （init 自动 ready / shutdown 退出）。
/// 阻塞直到 shutdown 或 stdin 关闭。
pub fn run<H: Handler>(handler: H) {
    let handler = &handler;
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Some(msg) = parse_line(line.trim()) else { continue };
        match msg {
            Incoming::Query { ref id, ref text, ref files } => {
                handler.on_query(*id, text, files)
            }
            Incoming::Activate { ref id, ref payload } => handler.on_activate(*id, payload),
            Incoming::UiEvent { ref ui_id, ref event, ref values } => {
                handler.on_ui_event(*ui_id, event, values)
            }
            Incoming::CapabilityResponse { ref id, ok, ref error, ref data } => {
                handler.on_capability_response(*id, ok, error, data.as_deref())
            }
            Incoming::Init => out_ready(),
            Incoming::Shutdown => {
                handler.on_shutdown();
                return;
            }
        }
    }
}

/// init 消息（协议要求 ready 应答）在 parse 层不可见（宿主只认 ready 回包），
/// run() 在启动时先自动应答 ready——插件无需关心。
pub fn out_ready() {
    emit(&json!({"type": "ready"}));
}

/// UI 输出区结果行（带行内复制按钮）
pub fn out_row(title: &str, subtitle: &str, copy: impl Into<String>) -> Value {
    json!({"title": title, "subtitle": subtitle, "copy": copy.into()})
}

/// UI 输出区结果行 + 多行预览
pub fn out_row_preview(title: &str, subtitle: &str, copy: impl Into<String>, preview: impl Into<String>) -> Value {
    json!({"title": title, "subtitle": subtitle, "copy": copy.into(), "preview": preview.into()})
}

pub mod prelude {
    pub use super::{emit, item, item_action, item_icon, out_capability_request, out_ready, out_row, out_row_preview,
                    out_results, out_ui_show, run, Handler, Incoming};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_query_with_context_files() {
        let line = r#"{"type":"query","id":7,"text":"abc","context":{"files":["/a","/b"]}}"#;
        match parse_line(line) {
            Some(Incoming::Query { id, text, files }) => {
                assert_eq!((id, text.as_str(), files.len()), (7, "abc", 2));
            }
            _ => panic!("应解析为 Query"),
        }
    }

    #[test]
    fn decodes_capability_data_string() {
        let line = r#"{"type":"capability_response","id":9,"ok":true,"data":"{\"a\":1}"}"#;
        match parse_line(line) {
            Some(Incoming::CapabilityResponse { id, ok, data, .. }) => {
                assert_eq!((id, ok, data.as_deref()), (9, true, Some("{\"a\":1}")));
            }
            _ => panic!("应解析为 CapabilityResponse"),
        }
    }

    #[test]
    fn item_action_prefixes_payload() {
        let v = item_action("标题", "副标题", "open:1");
        assert_eq!(v["payload"], json!("!open:1"));
    }

    #[test]
    fn unknown_type_ignored() {
        assert!(parse_line(r#"{"type":"mystery"}"#).is_none());
        assert!(parse_line("not json").is_none());
    }
}
