//! Nautilus 右键「复制绝对路径」的安装与状态管理。
//!
//! 唯一可靠路径：python3-nautilus 扩展（复制动作发生在 Nautilus
//! 进程内，剪贴板属主合法）。脚本内容与扩展源码均编译期内嵌。
//! 注：GNOME Wayland 的 mutter 未实现 wlr-data-control 协议，
//! wl-copy 等外部写入方案在本平台不可行（已实测验证）。

use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::Command;

const EXT_SRC: &str = include_str!("../../../packaging/nautilus/copy_absolute_path.py");

pub const EXT_NAME: &str = "copy_absolute_path.py";
/// 早期版本曾用过「右键 → 脚本」的外部脚本方案，启用扩展时顺带清理。
const LEGACY_SCRIPT_NAME: &str = "复制绝对路径";

#[derive(Debug, Clone, Copy)]
pub struct Status {
    pub ext_installed: bool,
    pub python_nautilus_available: bool,
    /// 旧方案残留脚本（启用扩展时自动清理）
    pub legacy_script: bool,
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
}

fn ext_path() -> PathBuf {
    home().join(".local/share/nautilus-python/extensions").join(EXT_NAME)
}

fn legacy_script_path() -> PathBuf {
    home().join(".local/share/nautilus/scripts").join(LEGACY_SCRIPT_NAME)
}

pub fn status() -> Status {
    Status {
        ext_installed: ext_path().exists(),
        python_nautilus_available: dpkg_has("python3-nautilus"),
        legacy_script: legacy_script_path().exists(),
    }
}

/// 启用：写入扩展并清理旧方案脚本，重载 Nautilus。
pub fn enable() -> io::Result<()> {
    if !dpkg_has("python3-nautilus") {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "缺少 python3-nautilus，请先安装依赖",
        ));
    }
    let p = ext_path();
    fs::create_dir_all(p.parent().expect("扩展路径必有父目录"))?;
    fs::write(&p, EXT_SRC)?;
    remove_legacy();
    reload_nautilus();
    Ok(())
}

/// 停用：移除扩展，重载 Nautilus。
pub fn disable() -> io::Result<()> {
    match fs::remove_file(ext_path()) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    reload_nautilus();
    Ok(())
}

fn remove_legacy() {
    fs::remove_file(legacy_script_path()).ok();
}

fn reload_nautilus() {
    Command::new("nautilus").arg("-q").status().ok();
}

pub fn dpkg_has(pkg: &str) -> bool {
    Command::new("dpkg")
        .args(["-s", pkg])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 通过 pkexec 安装系统包：弹出图形授权对话框，用户输入密码后
/// 自动完成。返回 false 表示授权被取消或安装未成功。
/// 阻塞式调用，请勿在 UI 主线程直接使用。
pub fn pkexec_install(pkg: &str) -> io::Result<bool> {
    let st = Command::new("pkexec")
        .args(["apt-get", "install", "-y", pkg])
        .status()?;
    if !st.success() {
        return Ok(false);
    }
    Ok(dpkg_has(pkg))
}
