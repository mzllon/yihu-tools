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

    /// 搜索匹配：文本的每个空白分隔词都须出现在「标题+关键字」中（大小写无关）。
    pub fn query(text: &str) -> Vec<PanelEntry> {
        let t = text.trim().to_lowercase();
        if t.is_empty() {
            return Self::capabilities();
        }
        Self::capabilities()
            .into_iter()
            .filter(|e| {
                let hay = format!(
                    "{} {}",
                    e.title,
                    match e.payload.as_str() {
                        "theme:dark" => "深色 dark",
                        "theme:light" => "浅色 light",
                        "center" => "中心 center",
                        "page:radio" => "广播 radio",
                        "page:autodark" => "主题 theme",
                        _ => "",
                    }
                )
                .to_lowercase();
                t.split_whitespace().all(|tok| hay.contains(tok))
            })
            .collect()
    }
}

/// 执行内置能力（深浅色 / 打开中心 / 深链）。
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
        _ => {}
    }
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
