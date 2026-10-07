//! 能力代理的授权与参数校验（纯函数，无 GTK 依赖）。
//!
//! 决策链：capability_request → [`evaluate`]（词表白名单 + manifest
//! 已声明 + 参数形状/限额）→ 执行（app.rs，GTK/进程）→ capability_response。
//! 「管道即凭证」：请求只能来自宿主拉起的会话 stdio，直接启动的进程
//! 根本没有这条通道（安全模型文档 §4.3）。
//!
//! 用户确认策略（M4 冻结决策 #4）：安装时权限明示 = 授权时点，
//! 不做每请求弹窗；参数级限额在这里兜底。

use std::collections::HashSet;

use yihu_core::permissions as perms;

/// 单次剪贴板写入上限：1 MiB UTF-8
pub const MAX_CLIPBOARD_BYTES: usize = 1024 * 1024;
/// open_uri 长度上限（RFC 3986 常见实现的上限口径）
pub const MAX_URI_LEN: usize = 2048;
/// 通知摘要/正文长度上限
pub const MAX_NOTIFY_SUMMARY: usize = 128;
pub const MAX_NOTIFY_BODY: usize = 512;
/// 允许 open_uri 打开的 scheme 白名单
const URI_SCHEMES: &[&str] = &["http", "https", "mailto"];

/// 校验通过后要执行的动作（app.rs 按变体分发到 GTK/子进程）
#[derive(Debug, Clone, PartialEq)]
pub enum CapAction {
    ClipboardWrite(String),
    OpenUri(String),
    LaunchApp(String),
    Notify { summary: String, body: String },
}

/// 授权 + 参数校验。Err 文案直接回给插件（capability_response.error）。
pub fn evaluate(
    declared: &HashSet<String>,
    capability: &str,
    params: &serde_json::Value,
) -> Result<CapAction, String> {
    // 词表白名单（防未知能力名进来；合法名必然可声明，未声明在此拦截）
    if !perms::KNOWN.contains(&capability) {
        return Err(format!("未知能力 {capability:?}"));
    }
    if !declared.contains(capability) {
        return Err(format!("manifest 未声明能力 {capability}"));
    }
    let get = |key: &str| -> Result<&str, String> {
        params
            .get(key)
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("参数 {key} 缺失或非字符串"))
    };
    match capability {
        perms::CLIPBOARD_WRITE => {
            let text = get("text")?;
            if text.len() > MAX_CLIPBOARD_BYTES {
                return Err(format!("text 超限（>{} 字节）", MAX_CLIPBOARD_BYTES));
            }
            Ok(CapAction::ClipboardWrite(text.to_string()))
        }
        perms::OPEN_URI => {
            let uri = get("uri")?;
            check_uri(uri)?;
            Ok(CapAction::OpenUri(uri.to_string()))
        }
        perms::LAUNCH_APP => {
            let id = get("desktop_id")?;
            if id.is_empty() || id.contains('/') || id.starts_with('.') {
                return Err("desktop_id 非法".into());
            }
            Ok(CapAction::LaunchApp(id.to_string()))
        }
        perms::NOTIFY => {
            let summary = get("summary")?;
            let body = params.get("body").and_then(|v| v.as_str()).unwrap_or("");
            if summary.len() > MAX_NOTIFY_SUMMARY {
                return Err(format!("summary 超限（>{} 字节）", MAX_NOTIFY_SUMMARY));
            }
            if body.len() > MAX_NOTIFY_BODY {
                return Err(format!("body 超限（>{} 字节）", MAX_NOTIFY_BODY));
            }
            Ok(CapAction::Notify {
                summary: summary.to_string(),
                body: body.to_string(),
            })
        }
        // SELECTED_FILES_READ 不是主动请求的能力：query.context 由宿主
        // 决定是否携带，插件无从请求；出现即内部错误。
        _ => Err(format!("能力 {capability} 不支持主动请求")),
    }
}

fn check_uri(uri: &str) -> Result<(), String> {
    if uri.is_empty() || uri.len() > MAX_URI_LEN {
        return Err("uri 长度非法".into());
    }
    if uri.chars().any(|c| c.is_control() || c == ' ') {
        return Err("uri 含控制字符或空白".into());
    }
    let scheme = uri.split(':').next().unwrap_or("").to_ascii_lowercase();
    if !URI_SCHEMES.contains(&scheme.as_str()) {
        return Err(format!(
            "scheme {scheme:?} 不在白名单（允许：{}）",
            URI_SCHEMES.join("/")
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn declared(list: &[&str]) -> HashSet<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn rejects_undeclared_and_unknown() {
        let d = declared(&[perms::CLIPBOARD_WRITE]);
        let e = evaluate(&d, perms::OPEN_URI, &json!({"uri":"https://x"})).unwrap_err();
        assert!(e.contains("未声明"), "{e}");
        let e = evaluate(&d, "make.coffee", &json!({})).unwrap_err();
        assert!(e.contains("未知能力"), "{e}");
    }

    #[test]
    fn clipboard_write_happy_and_limits() {
        let d = declared(&[perms::CLIPBOARD_WRITE]);
        let a = evaluate(&d, perms::CLIPBOARD_WRITE, &json!({"text":"你好"})).unwrap();
        assert_eq!(a, CapAction::ClipboardWrite("你好".into()));
        let big = "x".repeat(MAX_CLIPBOARD_BYTES + 1);
        assert!(evaluate(&d, perms::CLIPBOARD_WRITE, &json!({"text":big})).is_err());
        assert!(evaluate(&d, perms::CLIPBOARD_WRITE, &json!({"text":42})).is_err());
        assert!(evaluate(&d, perms::CLIPBOARD_WRITE, &json!({})).is_err());
    }

    #[test]
    fn open_uri_scheme_whitelist() {
        let d = declared(&[perms::OPEN_URI]);
        for uri in ["https://a.b/c", "http://a.b", "mailto:x@y.z"] {
            evaluate(&d, perms::OPEN_URI, &json!({"uri":uri})).unwrap();
        }
        for uri in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "ftp://a",
            "HTTPS://ok-but-case-parses",
            "https://a b",
            "https://a\nb",
            "",
        ] {
            let r = evaluate(&d, perms::OPEN_URI, &json!({"uri":uri}));
            if uri == "HTTPS://ok-but-case-parses" {
                r.unwrap(); // scheme 大小写不敏感，放行
            } else {
                assert!(r.is_err(), "{uri:?} 应被拒");
            }
        }
        let long = format!("https://{}", "a".repeat(MAX_URI_LEN));
        assert!(evaluate(&d, perms::OPEN_URI, &json!({"uri":long})).is_err());
    }

    #[test]
    fn launch_app_shape_only() {
        let d = declared(&[perms::LAUNCH_APP]);
        evaluate(&d, perms::LAUNCH_APP, &json!({"desktop_id":"org.gnome.Files.desktop"})).unwrap();
        for bad in ["", "../x.desktop", "./x", "a/b"] {
            assert!(evaluate(&d, perms::LAUNCH_APP, &json!({"desktop_id":bad})).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn notify_limits_and_optional_body() {
        let d = declared(&[perms::NOTIFY]);
        let a = evaluate(&d, perms::NOTIFY, &json!({"summary":"标题"})).unwrap();
        assert_eq!(a, CapAction::Notify { summary: "标题".into(), body: String::new() });
        let long_sum = "s".repeat(MAX_NOTIFY_SUMMARY + 1);
        assert!(evaluate(&d, perms::NOTIFY, &json!({"summary":long_sum})).is_err());
        let long_body = "b".repeat(MAX_NOTIFY_BODY + 1);
        assert!(evaluate(&d, perms::NOTIFY, &json!({"summary":"s","body":long_body})).is_err());
    }

    #[test]
    fn selected_files_read_is_not_requestable() {
        let d = declared(&[perms::SELECTED_FILES_READ]);
        assert!(evaluate(&d, perms::SELECTED_FILES_READ, &json!({})).is_err());
    }
}
