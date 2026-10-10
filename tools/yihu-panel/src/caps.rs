//! 能力代理的授权与参数校验（纯函数，无 GTK 依赖）。
//!
//! 决策链：capability_request → [`evaluate`]（词表白名单 + manifest
//! 已声明 + 参数形状/限额）→ 执行（app.rs，GTK/进程/后台线程）→
//! capability_response。「管道即凭证」：请求只能来自宿主拉起的会话
//! stdio，直接启动的进程根本没有这条通道（安全模型文档 §4.3）。
//!
//! 用户确认策略（M4 冻结决策 #4）：安装时权限明示 = 授权时点，
//! 不做每请求弹窗；参数级限额在这里兜底。
//!
//! M5 开放 API v2（2026-10-10）：带参能力 `network.fetch` /
//! `settings.write` / `fs.write`——manifest 用「能力@参数」声明白名单
//! （host / gsettings schema / 路径 glob），逐请求校验参数 ∈ 声明集合。

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
/// network.fetch 响应体上限
pub const MAX_FETCH_BYTES: usize = 4 * 1024 * 1024;
/// fs.write 单次写入上限
pub const MAX_FSWRITE_BYTES: usize = 4 * 1024 * 1024;
/// settings.write 值长度上限
pub const MAX_SETTING_VALUE: usize = 4096;

/// 校验通过后要执行的动作（app.rs 按变体分发到 GTK/子进程/后台线程）
#[derive(Debug, Clone, PartialEq)]
pub enum CapAction {
    ClipboardWrite(String),
    OpenUri(String),
    LaunchApp(String),
    Notify { summary: String, body: String },
    /// 截屏（宿主经 xdg-desktop-portal 异步执行，pump 完成后回包）
    Screenshot { mode: String, clipboard: bool },
    /// 经宿主发 HTTP GET（url 的 host:port ∈ 声明集），响应文本回传
    NetworkFetch { url: String },
    /// 经宿主写 gsettings（schema ∈ 声明集，仅字符串值；op=set/reset）
    SettingsWrite { schema: String, key: String, value: String, op: String },
    /// 经宿主读 gsettings（schema_spec = schema[:reloc-path]，∈ 声明集）
    SettingsRead { schema_spec: String, key: String },
    /// 经宿主写文件（路径匹配声明 glob，UTF-8 原子写）
    FsWrite { path: String, text: String },
}

/// 从 URL 提取 host[:port]（port 缺省按 scheme 补 443/80）。
/// 非法/非 http(s) 返回 None。
pub fn url_host_port(url: &str) -> Option<(String, String)> {
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    // 丢掉 userinfo（如有）
    let authority = authority.rsplit('@').next()?;
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => {
            (h.to_string(), p.to_string())
        }
        _ => (
            authority.to_string(),
            if scheme == "https" { "443".into() } else { "80".into() },
        ),
    };
    if host.is_empty() || host.starts_with('.') || host.contains("..") {
        return None;
    }
    Some((host, port))
}

/// host[:port] 是否命中声明集：带端口的声明精确匹配；不带端口的声明
/// 匹配同 host 的默认端口（http:80 / https:443）。
pub fn host_allowed(declared_hosts: &[String], host: &str, port: &str) -> bool {
    declared_hosts.iter().any(|d| {
        match d.rsplit_once(':') {
            Some((dh, dp)) if dp.chars().all(|c| c.is_ascii_digit()) => {
                dh == host && dp == port
            }
            _ => {
                // 无端口声明：默认端口匹配（host 段相同且端口为 80/443）
                d == &host && (port == "80" || port == "443")
            }
        }
    })
}

/// 展开 `~` 前缀并规范化（消除 `.`/`..`），供 glob 匹配。
pub fn normalize_path(path: &str, home: &str) -> Option<String> {
    let expanded = match path.strip_prefix('~') {
        Some(rest) => {
            // 只接受 ~/xxx 或恰为 ~（~user 不支持）
            if rest.is_empty() {
                home.to_string()
            } else if let Some(stripped) = rest.strip_prefix('/') {
                format!("{home}/{stripped}")
            } else {
                return None;
            }
        }
        None => path.to_string(),
    };
    if !expanded.starts_with('/') {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    for seg in expanded.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                // 根级 .. 无处可弹：按 POSIX 忽略（/../x ≡ /x）
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    Some(format!("/{}", parts.join("/")))
}

/// 授权 + 参数校验。`declared_raw` 是 manifest permissions 原始串
///（含「能力@参数」）。Err 文案直接回给插件（capability_response.error）。
pub fn evaluate(
    declared_raw: &[String],
    capability: &str,
    params: &serde_json::Value,
) -> Result<CapAction, String> {
    // 词表白名单（防未知能力名进来）
    if !perms::KNOWN.contains(&capability) {
        return Err(format!("未知能力 {capability:?}"));
    }
    let get = |key: &str| -> Result<&str, String> {
        params
            .get(key)
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("参数 {key} 缺失或非字符串"))
    };
    match capability {
        // —— 无参能力：裸声明即授权 ——
        perms::CLIPBOARD_WRITE => {
            if !perms::has_plain(declared_raw, capability) {
                return Err(format!("manifest 未声明能力 {capability}"));
            }
            let text = get("text")?;
            if text.len() > MAX_CLIPBOARD_BYTES {
                return Err(format!("text 超限（>{} 字节）", MAX_CLIPBOARD_BYTES));
            }
            Ok(CapAction::ClipboardWrite(text.to_string()))
        }
        perms::OPEN_URI => {
            if !perms::has_plain(declared_raw, capability) {
                return Err(format!("manifest 未声明能力 {capability}"));
            }
            let uri = get("uri")?;
            check_uri(uri)?;
            Ok(CapAction::OpenUri(uri.to_string()))
        }
        perms::LAUNCH_APP => {
            if !perms::has_plain(declared_raw, capability) {
                return Err(format!("manifest 未声明能力 {capability}"));
            }
            let id = get("desktop_id")?;
            if id.is_empty() || id.contains('/') || id.starts_with('.') {
                return Err("desktop_id 非法".into());
            }
            Ok(CapAction::LaunchApp(id.to_string()))
        }
        perms::NOTIFY => {
            if !perms::has_plain(declared_raw, capability) {
                return Err(format!("manifest 未声明能力 {capability}"));
            }
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
        perms::SCREENSHOT_TAKE => {
            if !perms::has_plain(declared_raw, capability) {
                return Err(format!("manifest 未声明能力 {capability}"));
            }
            let mode = get("mode")?;
            if mode != "full" && mode != "area" {
                return Err(format!("mode {mode:?} 非法（full/area）"));
            }
            let clipboard = params.get("clipboard").and_then(|v| v.as_bool()).unwrap_or(false);
            Ok(CapAction::Screenshot {
                mode: mode.to_string(),
                clipboard,
            })
        }
        // —— 带参能力：请求参数 ∈ 声明白名单 ——
        perms::NETWORK_FETCH => {
            let url = get("url")?;
            if url.len() > MAX_URI_LEN {
                return Err("url 超限".into());
            }
            if url.chars().any(|c| c.is_control()) {
                return Err("url 含控制字符".into());
            }
            let (host, port) =
                url_host_port(url).ok_or_else(|| "url 非法（仅支持 http/https）".to_string())?;
            let hosts = perms::params_of(declared_raw, capability);
            if !host_allowed(&hosts, &host, &port) {
                return Err(format!("host {host}:{port} 不在声明白名单"));
            }
            Ok(CapAction::NetworkFetch { url: url.to_string() })
        }
        perms::SETTINGS_WRITE => {
            let schema = get("schema")?;
            let key = get("key")?;
            let op = params.get("op").and_then(|v| v.as_str()).unwrap_or("set");
            if op != "set" && op != "reset" {
                return Err(format!("op {op:?} 非法（set/reset）"));
            }
            let value = params.get("value").and_then(|v| v.as_str()).unwrap_or("");
            if op == "set" && value.is_empty() {
                return Err("set 操作 value 不能为空".into());
            }
            let schemas = perms::params_of(declared_raw, capability);
            if !schemas.iter().any(|s| s == schema) {
                return Err(format!("schema {schema} 不在声明白名单"));
            }
            if key.is_empty() || key.len() > 128 || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                return Err(format!("key 非法：{key:?}"));
            }
            if value.len() > MAX_SETTING_VALUE {
                return Err(format!("value 超限（>{} 字节）", MAX_SETTING_VALUE));
            }
            Ok(CapAction::SettingsWrite {
                schema: schema.to_string(),
                key: key.to_string(),
                value: value.to_string(),
                op: op.to_string(),
            })
        }
        perms::SETTINGS_READ => {
            // schema_spec = <schema>[:<reloc-path>]；base 必须在声明集内，
            // reloc-path 仅字母数字/连字符/斜杠（条目路径）
            let spec = get("schema")?;
            let key = get("key")?;
            let (base, reloc) = match spec.split_once(':') {
                Some((b, p)) => (b, Some(p)),
                None => (spec, None),
            };
            let schemas = perms::params_of(declared_raw, capability);
            if !schemas.iter().any(|s| s == base) {
                return Err(format!("schema {base} 不在声明白名单"));
            }
            if let Some(p) = reloc {
                if !p.starts_with('/')
                    || !p.ends_with('/')
                    || !p
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '/' || c == '-' || c == '_')
                {
                    return Err(format!("relocatable 路径非法：{p:?}"));
                }
            }
            if key.is_empty() || key.len() > 128 || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                return Err(format!("key 非法：{key:?}"));
            }
            Ok(CapAction::SettingsRead {
                schema_spec: spec.to_string(),
                key: key.to_string(),
            })
        }
        perms::FS_WRITE => {
            let path = get("path")?;
            let text = get("text")?;
            if text.len() > MAX_FSWRITE_BYTES {
                return Err(format!("text 超限（>{} 字节）", MAX_FSWRITE_BYTES));
            }
            let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
            let norm = normalize_path(path, &home)
                .ok_or_else(|| format!("路径非法：{path:?}"))?;
            let globs = perms::params_of(declared_raw, capability);
            let hit = globs.iter().any(|g| {
                let expanded = normalize_path(g, &home).unwrap_or_default();
                glob::Pattern::new(&expanded)
                    .map(|p| {
                        p.matches_with(
                            &norm,
                            glob::MatchOptions {
                                // `*` 不跨目录、`**` 才跨层：防 Code* 越权匹配子目录
                                require_literal_separator: true,
                                ..glob::MatchOptions::new()
                            },
                        )
                    })
                    .unwrap_or(false)
            });
            if !hit {
                return Err(format!("路径 {norm} 不在声明白名单"));
            }
            Ok(CapAction::FsWrite { path: norm, text: text.to_string() })
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

    fn declared(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn plain(list: &[&str]) -> Vec<String> {
        declared(list)
    }

    #[test]
    fn rejects_undeclared_and_unknown() {
        let d = plain(&[perms::CLIPBOARD_WRITE]);
        let e = evaluate(&d, perms::OPEN_URI, &json!({"uri":"https://x"})).unwrap_err();
        assert!(e.contains("未声明"), "{e}");
        let e = evaluate(&d, "make.coffee", &json!({})).unwrap_err();
        assert!(e.contains("未知能力"), "{e}");
    }

    #[test]
    fn clipboard_write_happy_and_limits() {
        let d = plain(&[perms::CLIPBOARD_WRITE]);
        let a = evaluate(&d, perms::CLIPBOARD_WRITE, &json!({"text":"你好"})).unwrap();
        assert_eq!(a, CapAction::ClipboardWrite("你好".into()));
        let big = "x".repeat(MAX_CLIPBOARD_BYTES + 1);
        assert!(evaluate(&d, perms::CLIPBOARD_WRITE, &json!({"text":big})).is_err());
        assert!(evaluate(&d, perms::CLIPBOARD_WRITE, &json!({})).is_err());
    }

    #[test]
    fn open_uri_scheme_whitelist() {
        let d = plain(&[perms::OPEN_URI]);
        for uri in ["https://a.b/c", "http://a.b", "mailto:x@y.z"] {
            evaluate(&d, perms::OPEN_URI, &json!({"uri":uri})).unwrap();
        }
        for uri in ["file:///etc/passwd", "javascript:alert(1)", "ftp://a", ""] {
            assert!(evaluate(&d, perms::OPEN_URI, &json!({"uri":uri})).is_err(), "{uri:?}");
        }
    }

    #[test]
    fn url_host_port_extracts_and_defaults() {
        assert_eq!(
            url_host_port("https://api.example.com/v1?q=1"),
            Some(("api.example.com".into(), "443".into()))
        );
        assert_eq!(
            url_host_port("http://10.0.0.8:8080/x"),
            Some(("10.0.0.8".into(), "8080".into()))
        );
        // userinfo 不参与匹配
        assert_eq!(
            url_host_port("https://user:pass@h.com/"),
            Some(("h.com".into(), "443".into()))
        );
        assert_eq!(url_host_port("ftp://x"), None);
        assert_eq!(url_host_port("https://"), None);
    }

    #[test]
    fn host_allows_declared_only() {
        let d = declared(&["api.example.com", "10.0.0.8:8080"]);
        assert!(host_allowed(&d, "api.example.com", "443"));
        assert!(host_allowed(&d, "api.example.com", "80"));
        assert!(host_allowed(&d, "10.0.0.8", "8080"));
        // 无端口声明不匹配任意端口
        assert!(!host_allowed(&d, "api.example.com", "8443"));
        // 子域不连带
        assert!(!host_allowed(&d, "evil.example.com", "443"));
        // host 相同端口不同的精确声明不匹配
        assert!(!host_allowed(&d, "10.0.0.8", "9090"));
    }

    #[test]
    fn network_fetch_enforces_declaration() {
        let d = declared(&["network.fetch@api.example.com"]);
        let a = evaluate(
            &d,
            perms::NETWORK_FETCH,
            &json!({"url":"https://api.example.com/v1/data"}),
        )
        .unwrap();
        assert!(matches!(a, CapAction::NetworkFetch { .. }));
        // 未声明 host 拒绝
        let e = evaluate(&d, perms::NETWORK_FETCH, &json!({"url":"https://evil.com/x"}))
            .unwrap_err();
        assert!(e.contains("不在声明白名单"), "{e}");
        // 子域不连带
        assert!(evaluate(&d, perms::NETWORK_FETCH, &json!({"url":"https://api.example.com.evil.com/"})).is_err());
        // 非法 URL
        assert!(evaluate(&d, perms::NETWORK_FETCH, &json!({"url":"ftp://api.example.com/"})).is_err());
        // 完全未声明能力
        let e = evaluate(&plain(&[]), perms::NETWORK_FETCH, &json!({"url":"https://api.example.com/"}))
            .unwrap_err();
        assert!(e.contains("不在声明白名单"), "{e}");
    }

    #[test]
    fn settings_write_enforces_schema() {
        let d = declared(&["settings.write@org.gnome.desktop.interface"]);
        let a = evaluate(
            &d,
            perms::SETTINGS_WRITE,
            &json!({"schema":"org.gnome.desktop.interface","key":"color-scheme","value":"prefer-dark"}),
        )
        .unwrap();
        assert_eq!(
            a,
            CapAction::SettingsWrite {
                schema: "org.gnome.desktop.interface".into(),
                key: "color-scheme".into(),
                value: "prefer-dark".into(),
                op: "set".into(),
            }
        );
        // reset：无需 value
        let a = evaluate(
            &d,
            perms::SETTINGS_WRITE,
            &json!({"schema":"org.gnome.desktop.interface","key":"color-scheme","op":"reset"}),
        )
        .unwrap();
        assert_eq!(a, CapAction::SettingsWrite {
            schema: "org.gnome.desktop.interface".into(),
            key: "color-scheme".into(),
            value: String::new(),
            op: "reset".into(),
        });
        // op 非法
        assert!(evaluate(
            &d,
            perms::SETTINGS_WRITE,
            &json!({"schema":"org.gnome.desktop.interface","key":"x","op":"delete"})
        )
        .is_err());
        // 其他 schema 拒绝
        assert!(evaluate(
            &d,
            perms::SETTINGS_WRITE,
            &json!({"schema":"org.gnome.shell","key":"x","value":"y"})
        )
        .is_err());
        // key 形状校验
        assert!(evaluate(
            &d,
            perms::SETTINGS_WRITE,
            &json!({"schema":"org.gnome.desktop.interface","key":"a b","value":"y"})
        )
        .is_err());
    }

    #[test]
    fn settings_read_enforces_schema_and_reloc() {
        // relocatable 子 schema 与父 schema 是不同名字，声明制要求精确声明
        let d = declared(&[
            "settings.read@org.gnome.settings-daemon.plugins.media-keys",
            "settings.read@org.gnome.settings-daemon.plugins.media-keys.custom-keybinding",
        ]);
        // base schema
        let a = evaluate(
            &d,
            perms::SETTINGS_READ,
            &json!({"schema":"org.gnome.settings-daemon.plugins.media-keys","key":"custom-keybindings"}),
        )
        .unwrap();
        assert!(matches!(a, CapAction::SettingsRead { .. }));
        // relocatable 子条目（base = 子 schema 名 ∈ 声明集）
        evaluate(
            &d,
            perms::SETTINGS_READ,
            &json!({"schema":"org.gnome.settings-daemon.plugins.media-keys.custom-keybinding:/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/custom0/","key":"name"}),
        )
        .unwrap();
        // 子 schema 未声明 → 拒（即使父 schema 已声明）
        let parent_only = declared(&[
            "settings.read@org.gnome.settings-daemon.plugins.media-keys",
        ]);
        assert!(evaluate(
            &parent_only,
            perms::SETTINGS_READ,
            &json!({"schema":"org.gnome.settings-daemon.plugins.media-keys.custom-keybinding:/org/x/","key":"name"})
        )
        .is_err(), "未声明的子 schema 应被拒");
        // base 不在声明 → 拒
        assert!(evaluate(
            &d,
            perms::SETTINGS_READ,
            &json!({"schema":"org.gnome.shell","key":"x"})
        )
        .is_err());
        // reloc path 形状非法
        for bad in [
            "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding:relative/",
            "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding:/abs/no-end",
            "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding:/a/../b/",
        ] {
            assert!(evaluate(
                &d,
                perms::SETTINGS_READ,
                &json!({"schema":bad,"key":"name"})
            )
            .is_err(), "{bad}");
        }
        // key 形状
        assert!(evaluate(
            &d,
            perms::SETTINGS_READ,
            &json!({"schema":"org.gnome.settings-daemon.plugins.media-keys","key":"has dot"})
        )
        .is_err());
    }

    #[test]
    fn fs_write_enforces_glob_and_normalizes() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let d = declared(&["fs.write@~/.config/Code*/settings.json"]);
        let a = evaluate(
            &d,
            perms::FS_WRITE,
            &json!({"path":"~/.config/Code - OSS/User/settings.json","text":"{}"}),
        )
        .unwrap_err(); // ** 未用：单层 * 不跨层，应失败
        assert!(a.contains("不在声明白名单"), "{a}");

        let d2 = declared(&["fs.write@~/.config/Code*/**/settings.json"]);
        let a2 = evaluate(
            &d2,
            perms::FS_WRITE,
            &json!({"path":"~/.config/Code - OSS/User/settings.json","text":"{}"}),
        )
        .unwrap();
        assert_eq!(
            a2,
            CapAction::FsWrite {
                path: format!("{home}/.config/Code - OSS/User/settings.json"),
                text: "{}".into(),
            }
        );
        // 穿越攻击：声明内路径借 .. 逃逸
        let e = evaluate(
            &d2,
            perms::FS_WRITE,
            &json!({"path":"~/.config/Code/../.ssh/authorized_keys","text":"x"}),
        )
        .unwrap_err();
        assert!(e.contains("不在声明白名单"), "{e}");
        // ~user 形态拒绝
        assert!(evaluate(
            &d2,
            perms::FS_WRITE,
            &json!({"path":"~other/x","text":"x"})
        )
        .is_err());
    }

    #[test]
    fn normalize_path_resolves_dots() {
        let home = "/home/u";
        assert_eq!(normalize_path("~/a/../b", home).unwrap(), "/home/u/b");
        assert_eq!(normalize_path("/a/./b//c", home).unwrap(), "/a/b/c");
        // 根级 .. 按 POSIX 忽略
        assert_eq!(normalize_path("/../x", home).unwrap(), "/x");
        assert_eq!(normalize_path("rel/path", home), None);
        assert_eq!(normalize_path("~root/x", home), None);
    }
}

