//! 内置能力提供者：搜索结果路径的进程内实现（系统插件形态）。
//!
//! 外部插件走 `sessions.rs` 的独立进程协议；内置能力留在宿主进程内
//!（零开销），但结果/激活的数据形状与外部插件完全一致（PanelEntry）。

use std::path::PathBuf;

use crate::app::PanelEntry;

pub struct BuiltinProvider;

impl BuiltinProvider {
    pub fn capabilities() -> Vec<PanelEntry> {
        vec![
            PanelEntry {
                title: "切换到深色模式".into(),
                subtitle: "一呼 · 能力".into(),
                icon_spec: "weather-clear-night-symbolic".into(),
                kind: "cap",
                payload: "theme:dark".into(),
            },
            PanelEntry {
                title: "切换到浅色模式".into(),
                subtitle: "一呼 · 能力".into(),
                icon_spec: "weather-clear-symbolic".into(),
                kind: "cap",
                payload: "theme:light".into(),
            },
            PanelEntry {
                title: "打开一呼中心".into(),
                subtitle: "一呼 · 能力".into(),
                icon_spec: "tools.yihu.desktop".into(),
                kind: "cap",
                payload: "center".into(),
            },
            PanelEntry {
                title: "打开广播页".into(),
                subtitle: "一呼 · 能力".into(),
                icon_spec: "applications-multimedia-symbolic".into(),
                kind: "cap",
                payload: "page:radio".into(),
            },
            PanelEntry {
                title: "打开主题切换页".into(),
                subtitle: "一呼 · 能力".into(),
                icon_spec: "night-light-symbolic".into(),
                kind: "cap",
                payload: "page:autodark".into(),
            },
        ]
    }

    /// 系统命令包（对标 uTools 系统命令，2026-10-07）：进程内条目 +
    /// 激活时一次性子进程，不新增常驻进程；拼音列自动生效
    ///（"suoping" 命中锁屏）。默认胶囊集不含它们（CHIPS 硬编码），
    /// 只出现在搜索结果里。
    pub fn system_commands() -> Vec<PanelEntry> {
        const SYS: &[(&str, &str, &str, &str)] = &[
            ("sys:lock", "锁定屏幕", "system-lock-screen-symbolic", "lock"),
            ("sys:suspend", "挂起", "weather-clear-night-symbolic", "suspend"),
            ("sys:poweroff", "关机", "system-shutdown-symbolic", "poweroff shutdown"),
            ("sys:reboot", "重启", "view-refresh-symbolic", "reboot restart"),
            ("sys:empty-trash", "清空回收站", "user-trash-symbolic", "trash 回收站"),
            ("sys:screenshot", "截图", "camera-photo-symbolic", "screenshot"),
            ("sys:files", "打开文件管理器", "system-file-manager-symbolic", "files nautilus"),
            ("sys:night-light", "夜灯开关", "night-light-symbolic", "nightlight 夜灯"),
        ];
        SYS.iter()
            .map(|(payload, title, icon, _)| PanelEntry {
                title: title.to_string(),
                subtitle: "一呼 · 系统命令".into(),
                icon_spec: icon.to_string(),
                kind: "cap",
                payload: payload.to_string(),
            })
            .collect()
    }

    /// 网页搜索直达（对标 uTools 网页快开的快捷前缀）：
    /// `g 词`=Google、`b 词`=百度、`bing 词`=Bing、`ddg 词`=DuckDuckGo；
    /// 裸域名（含点、无空白）直达打开。返回 None = 走正常搜索。
    pub fn web_direct(text: &str) -> Option<Vec<PanelEntry>> {
        const ENGINES: &[(&str, &str)] = &[
            ("g ", "https://www.google.com/search?q="),
            ("b ", "https://www.baidu.com/s?wd="),
            ("bing ", "https://www.bing.com/search?q="),
            ("ddg ", "https://duckduckgo.com/?q="),
        ];
        let t = text.trim();
        let lower = t.to_lowercase();
        for (prefix, base) in ENGINES {
            if let Some(term) = lower.strip_prefix(prefix) {
                let term = term.trim();
                if term.is_empty() {
                    continue;
                }
                // 取原始文本中对应位置的词（保留大小写），转义后拼 URL
                let raw = t[t.len() - term.len()..].trim();
                return Some(vec![PanelEntry {
                    title: format!("搜索「{raw}」"),
                    subtitle: "一呼 · 网页搜索（回车在默认浏览器打开）".into(),
                    icon_spec: "web-browser-symbolic".into(),
                    kind: "cap",
                    payload: format!("web:{}{}", base, percent_encode_query(raw)),
                }]);
            }
        }
        // 裸域名直达（尾段限常见 TLD：settings.ini 之类文件名不误触；
        // 拦错的代价只是回落正常搜索，无损失）
        if t.contains('.') && !t.contains(' ') && looks_like_domain(t) {
            let url = format!("https://{t}");
            return Some(vec![PanelEntry {
                title: format!("打开网址 {t}"),
                subtitle: "一呼 · 网页直达".into(),
                icon_spec: "web-browser-symbolic".into(),
                kind: "cap",
                payload: format!("web:{url}"),
            }]);
        }
        None
    }

    /// 搜索匹配：网页直达前缀优先，其余文本的每个空白分隔词都须出现在
    /// 「标题+关键字+拼音列」中（大小写无关；拼音列含全拼与首字母，
    /// "shense"/"ds" 均可命中深色）。
    pub fn query(text: &str) -> Vec<PanelEntry> {
        let t = text.trim().to_lowercase();
        if t.is_empty() {
            return Self::capabilities();
        }
        if let Some(rows) = Self::web_direct(text) {
            return rows;
        }
        Self::capabilities()
            .into_iter()
            .chain(Self::system_commands())
            .filter(|e| {
                let hay = format!(
                    "{} {} {}",
                    e.title,
                    match e.payload.as_str() {
                        "theme:dark" => "深色 dark",
                        "theme:light" => "浅色 light",
                        "center" => "中心 center",
                        "page:radio" => "广播 radio",
                        "page:autodark" => "主题 theme",
                        "sys:lock" => "lock",
                        "sys:suspend" => "suspend",
                        "sys:poweroff" => "poweroff shutdown",
                        "sys:reboot" => "reboot restart",
                        "sys:empty-trash" => "trash",
                        "sys:screenshot" => "screenshot",
                        "sys:files" => "files nautilus",
                        "sys:night-light" => "nightlight",
                        _ => "",
                    },
                    crate::pinyin_index::match_column(&e.title),
                )
                .to_lowercase();
                t.split_whitespace().all(|tok| hay.contains(tok))
            })
            .collect()
    }
}

/// query 组件百分号转义（RFC 3986 保留字符外的全部转义，含空格与 CJK）
fn percent_encode_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'*' | b'!' | b'\'' | b'(' | b')' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 粗筛「像域名」：以字母数字开头结尾、点分标签合法（无连续点、
/// 标签不以连字符开头结尾），且尾段在常见 TLD 白名单内。拦错的代价
/// 只是回落正常搜索；放错（如 settings.ini）会打开错误网址。
fn looks_like_domain(s: &str) -> bool {
    const TLDS: &[&str] = &[
        "com", "net", "org", "io", "dev", "app", "xyz", "cn", "uk", "de", "ru", "me", "info",
        "cc", "top", "online", "site", "tech", "ai", "co", "jp", "kr", "fr", "us", "so", "tv",
    ];
    let s = s.trim();
    if s.len() < 4 || !s.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        return false;
    }
    s.split('.').all(|label| {
        !label.is_empty()
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    })
    && s.rsplit('.')
        .next()
        .is_some_and(|tld| TLDS.contains(&tld.to_ascii_lowercase().as_str()))
}

/// 执行内置能力（深浅色 / 打开中心 / 深链 / 系统命令 / 网页直达）。
pub fn activate_capability(payload: &str, center: &PathBuf) {
    match payload {
        "theme:dark" => {
            std::thread::spawn(|| {
                let _ = yihu_core::autodark::set_scheme(yihu_core::autodark::Theme::Dark);
            });
        }
        "theme:light" => {
            std::thread::spawn(|| {
                let _ = yihu_core::autodark::set_scheme(yihu_core::autodark::Theme::Light);
            });
        }
        "center" => {
            spawn_detached(center, &[]);
        }
        p if p.starts_with("page:") => {
            let page = &p["page:".len()..];
            spawn_detached(center, &["--page", page]);
        }
        // —— 系统命令包：激活时一次性子进程，无常驻（挂起/关机/重启为
        // 显式激活，等同 GNOME 电源菜单的操作路径）——
        "sys:lock" => spawn_cmd("loginctl", &["lock-session"]),
        "sys:suspend" => spawn_cmd("systemctl", &["suspend"]),
        "sys:poweroff" => spawn_cmd("systemctl", &["poweroff"]),
        "sys:reboot" => spawn_cmd("systemctl", &["reboot"]),
        "sys:empty-trash" => spawn_cmd("gio", &["trash", "--empty"]),
        "sys:screenshot" => spawn_cmd("gnome-screenshot", &["--interactive"]),
        "sys:files" => {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
            spawn_cmd("gio", &["open", &home]);
        }
        "sys:night-light" => toggle_night_light(),
        // —— 网页搜索/网址直达：默认浏览器打开 ——
        p if p.starts_with("web:") => {
            let url = &p["web:".len()..];
            spawn_cmd("xdg-open", &[url]);
        }
        _ => {}
    }
}

fn spawn_cmd(program: &str, args: &[&str]) {
    if let Err(e) = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        eprintln!("yihu-panel: 系统命令 {program} 启动失败：{e}");
    }
}

/// 夜灯开关：读当前状态取反（gsettings 输出有界，后台线程执行）
fn toggle_night_light() {
    std::thread::spawn(|| {
        let on = std::process::Command::new("gsettings")
            .args([
                "get",
                "org.gnome.settings-daemon.plugins.color",
                "night-light-enabled",
            ])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "true")
            .unwrap_or(false);
        let _ = std::process::Command::new("gsettings")
            .args([
                "set",
                "org.gnome.settings-daemon.plugins.color",
                "night-light-enabled",
                if on { "false" } else { "true" },
            ])
            .spawn();
    });
}

fn spawn_detached(program: &PathBuf, args: &[&str]) {
    if let Err(e) = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        eprintln!("yihu-panel: 启动 {} 失败：{e}", program.display());
    }
}

#[cfg(test)]
mod pinyin_tests {
    use super::*;

    #[test]
    fn pinyin_hits_capabilities() {
        assert!(BuiltinProvider::query("shense")
            .iter()
            .any(|e| e.payload == "theme:dark"), "全拼命中深色");
        assert!(BuiltinProvider::query("qianse")
            .iter()
            .any(|e| e.payload == "theme:light"), "全拼命中浅色");
        assert!(BuiltinProvider::query("zhuti")
            .iter()
            .any(|e| e.payload == "page:autodark"), "全拼命中主题");
    }
}

#[cfg(test)]
mod direct_tests {
    use super::*;

    #[test]
    fn web_search_prefixes() {
        let rows = BuiltinProvider::web_direct("g rust lang").unwrap();
        assert_eq!(rows[0].payload, "web:https://www.google.com/search?q=rust%20lang");
        let rows = BuiltinProvider::web_direct("b 中文搜索").unwrap();
        assert!(
            rows[0].payload.contains("baidu.com/s?wd=%E4%B8%AD%E6%96%87%E6%90%9C%E7%B4%A2"),
            "{}",
            rows[0].payload
        );
        assert!(BuiltinProvider::web_direct("bing news").is_some());
        // 空词不触发（还想搜应用）
        assert!(BuiltinProvider::web_direct("g ").is_none());
        assert!(BuiltinProvider::web_direct("b").is_none());
    }

    #[test]
    fn bare_domain_direct() {
        let rows = BuiltinProvider::web_direct("github.com").unwrap();
        assert_eq!(rows[0].payload, "web:https://github.com");
        let rows = BuiltinProvider::web_direct("docs.rust-lang.org").unwrap();
        assert!(rows[0].payload.starts_with("web:https://docs.rust-lang.org"));
        // 非域名形态 / 非常见 TLD 不触发（回落正常搜索）
        assert!(BuiltinProvider::web_direct("settings.ini").is_none());
        assert!(BuiltinProvider::web_direct("a..b").is_none());
        assert!(BuiltinProvider::web_direct("make.coffee").is_none());
    }

    #[test]
    fn system_commands_query_and_pinyin() {
        let all = BuiltinProvider::system_commands();
        assert_eq!(all.len(), 8);
        assert!(BuiltinProvider::query("suoding")
            .iter()
            .any(|e| e.payload == "sys:lock"), "拼音命中锁屏");
        assert!(BuiltinProvider::query("sdpm")
            .iter()
            .any(|e| e.payload == "sys:lock"), "首字母命中锁屏");
        assert!(BuiltinProvider::query("trash")
            .iter()
            .any(|e| e.payload == "sys:empty-trash"), "英文命中回收站");
        // 正常应用搜索不被系统命令挤掉：纯中文词不误触
        assert!(BuiltinProvider::query("shense").iter().any(|e| e.payload == "theme:dark"));
    }

    #[test]
    fn percent_encode_covers_reserved() {
        assert_eq!(percent_encode_query("a b&c=d/中"), "a%20b%26c%3Dd%2F%E4%B8%AD");
        assert_eq!(percent_encode_query("ok!~*'()"), "ok!~*'()");
    }

    #[test]
    fn domain_shape_check() {
        assert!(looks_like_domain("github.com"));
        assert!(looks_like_domain("docs.rust-lang.org"));
        assert!(!looks_like_domain("-a.com"));
        assert!(!looks_like_domain("a-.com"));
        assert!(!looks_like_domain("a.b1")); // 尾段含数字
        assert!(!looks_like_domain("a.b")); // 尾段过短
    }
}
