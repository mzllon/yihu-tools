//! 一呼用户数据路径与旧历史配置命名空间兼容。
//!
//! 新代码统一写入 `~/.config/yihu`；读取时新路径优先，旧的
//! `~/.config/minitools` 只作为一次性迁移/回退来源。这里不删除任何用户数据。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const CURRENT_NAMESPACE: &str = "yihu";
pub const LEGACY_NAMESPACE: &str = "minitools";

fn config_home() -> PathBuf {
    std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|v| v.starts_with('/'))
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
                .join(".config")
        })
}

pub fn config_dir() -> PathBuf {
    config_home().join(CURRENT_NAMESPACE)
}

pub fn legacy_config_dir() -> PathBuf {
    config_home().join(LEGACY_NAMESPACE)
}

pub fn config_file(name: impl AsRef<Path>) -> PathBuf {
    config_dir().join(name)
}

pub fn legacy_config_file(name: impl AsRef<Path>) -> PathBuf {
    legacy_config_dir().join(name)
}

/// 新文件优先；新文件不存在时回退读取旧历史命名空间。
pub fn read_compatible(name: impl AsRef<Path>) -> io::Result<String> {
    let name = name.as_ref();
    match fs::read_to_string(config_file(name)) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            fs::read_to_string(legacy_config_file(name))
        }
        Err(e) => Err(e),
    }
}

pub fn write_current(name: impl AsRef<Path>, text: &str) -> io::Result<()> {
    let path = config_file(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, text)
}

/// 将旧文件复制到新路径，仅在新路径不存在时执行；不删除旧文件。
pub fn migrate_one(name: impl AsRef<Path>) -> io::Result<bool> {
    let name = name.as_ref();
    let current = config_file(name);
    let legacy = legacy_config_file(name);
    if current.exists() || !legacy.is_file() {
        return Ok(false);
    }
    if let Some(parent) = current.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(legacy, current)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespaces_are_distinct() {
        assert!(config_dir().ends_with("yihu"));
        assert!(legacy_config_dir().ends_with("minitools"));
        assert_ne!(config_file("panel.conf"), legacy_config_file("panel.conf"));
    }
}
