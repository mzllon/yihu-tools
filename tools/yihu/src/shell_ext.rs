//! 一呼面板定位扩展（GNOME Shell）：安装 / 移除 / 状态。
//!
//! 与 Nautilus 扩展同惯例：源码内嵌进中心，一键安装到用户扩展目录。
//! 扩展作用：面板呼出时把它摆到「水平居中、垂直上 1/4 处」并置顶
//!（Wayland 客户端无自我定位接口，此能力只能由 Shell 一侧提供）。

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const UUID: &str = "yihu-panel-placer@tools.yihu";

const EXT_JS: &str =
    include_str!("../../../packaging/gnome-shell/yihu-panel-placer@tools.yihu/extension.js");
const METADATA: &str =
    include_str!("../../../packaging/gnome-shell/yihu-panel-placer@tools.yihu/metadata.json");

fn extensions_dir() -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|v| v.starts_with('/'))
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/share"));
    base.join("gnome-shell/extensions").join(UUID)
}

pub fn installed() -> bool {
    let dir = extensions_dir();
    dir.join("extension.js").exists() && dir.join("metadata.json").exists()
}

/// 写入扩展文件并尝试热启用（新装扩展在 Wayland 下可能需重新登录一次生效）。
pub fn install() -> io::Result<()> {
    let dir = extensions_dir();
    fs::create_dir_all(&dir)?;
    let (js, meta) = (dir.join("extension.js"), dir.join("metadata.json"));
    fs::write(&js, EXT_JS)?;
    fs::write(&meta, METADATA)?;
    // 显式 0644：避免 umask 002 系统上出现组可写文件
    fs::set_permissions(&js, fs::Permissions::from_mode(0o644))?;
    fs::set_permissions(&meta, fs::Permissions::from_mode(0o644))?;
    inject_shell_version(&meta);
    let _ = Command::new("gnome-extensions").args(["enable", UUID]).status();
    Ok(())
}

/// 把本机 gnome-shell 主版本号追加进 shell-version（已声明则不动）：
/// 避免发行版升级 GNOME 大版本后扩展被版本校验拦截。
fn inject_shell_version(meta_path: &Path) {
    let Ok(out) = Command::new("gnome-shell").arg("--version").output() else {
        return;
    };
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let Some(major) = text
        .rsplit(' ')
        .next()
        .and_then(|v| v.split('.').next())
        .map(|s| s.to_string())
    else {
        return;
    };
    let Ok(current) = fs::read_to_string(meta_path) else {
        return;
    };
    let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&current) else {
        return;
    };
    if let Some(arr) = v.get_mut("shell-version").and_then(|x| x.as_array_mut()) {
        if !arr.iter().any(|x| x.as_str() == Some(major.as_str())) {
            arr.push(serde_json::Value::String(major));
            if let Ok(text) = serde_json::to_string_pretty(&v) {
                let _ = fs::write(meta_path, text);
            }
        }
    }
}

/// 移除扩展文件（先禁用；失败不阻塞删除）。
pub fn remove() -> io::Result<()> {
    let _ = Command::new("gnome-extensions")
        .args(["disable", UUID])
        .status();
    let dir = extensions_dir();
    if dir.exists() {
        fs::remove_dir_all(dir)?;
    }
    Ok(())
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
}
