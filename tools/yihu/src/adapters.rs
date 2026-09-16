//! 「应用跟随系统主题」适配器。
//!
//! 对内置了深浅主题但不随系统切换的应用，改写其自身配置实现
//! 自动跟随。VS Code 系编辑器（含各分支）的 settings.json 支持
//! 热重载：改写后运行中的实例立即生效。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// 一个可适配的应用（VS Code 系：settings.json + User 目录约定）。
pub struct AppAdapter {
    pub name: &'static str,
    /// ~/.config 下的目录名
    pub dir: &'static str,
}

pub const VSCODE_FAMILY: &[AppAdapter] = &[
    AppAdapter { name: "CodeBuddy", dir: "CodeBuddy CN" },
    AppAdapter { name: "ZCode", dir: "ZCode" },
];

#[derive(Debug, Clone, Copy)]
pub struct Status {
    /// 应用已安装（配置目录存在）
    pub app_present: bool,
    /// 已开启自动跟随
    pub adapted: bool,
}

fn config_home() -> PathBuf {
    std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into())).join(".config"))
}

fn settings_path(dir: &str) -> PathBuf {
    config_home().join(dir).join("User/settings.json")
}

pub fn status(adapter: &AppAdapter) -> Status {
    let app_present = config_home().join(adapter.dir).exists();
    let adapted = fs::read_to_string(settings_path(adapter.dir))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("window.autoDetectColorScheme").cloned())
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    Status { app_present, adapted }
}

/// 应用适配：开启「自动跟随系统深浅色」。
/// 已有配置完整保留；用户自定义过的主题偏好不被覆盖。
pub fn apply(adapter: &AppAdapter) -> io::Result<()> {
    vscode_merge(&settings_path(adapter.dir))
}

/// 通用的 settings.json 合并（独立出来便于单元测试）。
fn vscode_merge(path: &Path) -> io::Result<()> {
    let mut v: Value = if path.exists() {
        let text = fs::read_to_string(path)?;
        serde_json::from_str(&text).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} 解析失败：{e}", path.display()),
            )
        })?
    } else {
        json!({})
    };
    if !v.is_object() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} 不是 JSON 对象", path.display()),
        ));
    }
    let obj = v.as_object_mut().expect("已确认是对象");
    obj.insert("window.autoDetectColorScheme".into(), json!(true));
    // 仅在用户未自定义主题偏好时写入默认主题对，尊重个性化选择
    obj.entry("workbench.preferredDarkColorTheme")
        .or_insert(json!("Default Dark Modern"));
    obj.entry("workbench.preferredLightColorTheme")
        .or_insert(json!("Default Light Modern"));

    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let out = serde_json::to_string_pretty(&v)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    fs::write(path, out + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_preserves_existing_and_sets_keys() {
        let dir = std::env::temp_dir().join(format!("mt-adapter-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("settings.json");
        fs::write(&p, r#"{ "workbench.colorTheme": "My Theme", "editor.fontSize": 14 }"#).unwrap();

        vscode_merge(&p).unwrap();

        let v: Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["window.autoDetectColorScheme"], json!(true));
        // 用户已有 colorTheme 保留；未设置的偏好主题补默认值
        assert_eq!(v["workbench.colorTheme"], json!("My Theme"));
        assert_eq!(v["editor.fontSize"], json!(14));
        assert_eq!(v["workbench.preferredDarkColorTheme"], json!("Default Dark Modern"));
        assert_eq!(v["workbench.preferredLightColorTheme"], json!("Default Light Modern"));

        // 重复应用幂等，且不覆盖用户改过的偏好主题
        let mut v2 = v.clone();
        v2["workbench.preferredDarkColorTheme"] = json!("Night Owl");
        fs::write(&p, serde_json::to_string_pretty(&v2).unwrap()).unwrap();
        vscode_merge(&p).unwrap();
        let v3: Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v3["workbench.preferredDarkColorTheme"], json!("Night Owl"));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn merge_creates_new_file() {
        let dir = std::env::temp_dir().join(format!("mt-adapter-new-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("User").join("settings.json");
        vscode_merge(&p).unwrap();
        let v: Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["window.autoDetectColorScheme"], json!(true));
        fs::remove_dir_all(&dir).ok();
    }
}
