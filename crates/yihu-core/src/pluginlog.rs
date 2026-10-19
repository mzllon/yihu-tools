//! 插件统一日志（基座能力）：插件入口调一次 [`init`]，之后用 `pinfo!`/
//! `pwarn!`/`perror!`（yihu-core 根导出）写日志，插件自身不碰文件与轮转。
//!
//! - 落盘 `~/.local/share/yihu/logs/<id>.log`，超过 512KiB 轮转为
//!   `<id>.log.1`（只留一代：够回溯排查，不无限占盘）
//! - 每行同步镜像 stderr——nohup/journald 等既有通道照旧可收
//! - [`init`] 同时挂 panic 钩子：panic 消息、位置、backtrace 落盘。
//!   GTK 回调里的 panic 不可 unwind、进程直接 abort（BUG-008 闪退），
//!   不挂钩子事故现场就什么都没了——native 插件必须 init
//!
//! 设计取舍：每次写入单独 open(O_APPEND)（插件日志量级下开销可忽略），
//! 换取无句柄状态、钩子路径零并发陷阱；轮转竞态无害（最坏一次空轮转）。

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::panic;
use std::path::PathBuf;
use std::sync::OnceLock;

/// 单文件上限，超过轮转一代
const MAX_BYTES: u64 = 512 * 1024;
/// backtrace 落盘截断（帧多为 unknown 地址，整段很长）
const MAX_BT_BYTES: usize = 8 * 1024;

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// 插件入口调用一次：注册日志文件 + 安装 panic 钩子。重复调用以首次为准
/// （OnceLock 语义），钩子重复挂也无害（链式回退到前一个）。
pub fn init(id: &str) {
    let dir = crate::plugins::data_home().join("yihu/logs");
    let _ = fs::create_dir_all(&dir);
    let _ = LOG_PATH.set(dir.join(format!("{id}.log")));
    install_panic_hook();
}

/// 供 `pinfo!`/`pwarn!`/`perror!` 使用；也可直接调用写自定义级别。
pub fn write(level: &str, args: fmt::Arguments<'_>) {
    let line = format!("{} {level:<5} {args}\n", timestamp());
    let _ = std::io::stderr().write_all(line.as_bytes());
    let Some(path) = LOG_PATH.get() else {
        return; // init 未调用：只镜像 stderr，绝不因日志拖垮宿主
    };
    rotate_if_needed(path, MAX_BYTES);
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
    }
}

fn install_panic_hook() {
    let prev = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let bt = std::backtrace::Backtrace::force_capture();
        let mut bt = bt.to_string();
        bt.truncate(MAX_BT_BYTES);
        write("PANIC", format_args!("{info}\nbacktrace:\n{bt}"));
        prev(info);
    }));
}

/// 超限轮转：现文件改名 .log.1（覆盖上一代）。竞态无害（最坏一次空轮转）。
fn rotate_if_needed(path: &std::path::Path, max: u64) {
    let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if size > max {
        let mut old = path.as_os_str().to_os_string();
        old.push(".1");
        let _ = fs::rename(path, PathBuf::from(old));
    }
}

/// 本地时间戳（不引时间库，libc localtime_r）。
fn timestamp() -> String {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return "????-??-?? ??:??:??".into();
        }
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_kicks_in_over_limit() {
        let dir = std::env::temp_dir().join(format!("yihu-plog-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("t.log");
        fs::write(&path, "x".repeat(100)).unwrap();
        rotate_if_needed(&path, 50);
        assert!(!path.exists(), "超限后现文件应被轮转走");
        assert_eq!(fs::read(dir.join("t.log.1")).unwrap(), b"x".repeat(100));
        rotate_if_needed(&path, 50); // 文件不存在时是空操作
        assert!(!dir.join("t.log.1.1").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn timestamp_shape() {
        let ts = timestamp();
        assert_eq!(ts.len(), 19, "{ts}");
        assert_eq!(ts.as_bytes()[4], b'-');
        assert_eq!(ts.as_bytes()[10], b' ');
        assert_eq!(ts.as_bytes()[13], b':');
    }

    /// 端到端：init 后 panic 必须把 PANIC 行落盘（BUG-008：GTK 回调 panic
    /// 直接 abort，钩子是唯一现场）。独占 env，勿与其他改 env 的测试并行假设。
    #[test]
    fn panic_hook_writes_to_file() {
        let dir = std::env::temp_dir().join(format!("yihu-plog-hook-{}", std::process::id()));
        let prev = std::env::var("XDG_DATA_HOME").ok();
        std::env::set_var("XDG_DATA_HOME", &dir);
        init("hooktest");
        let _ = std::panic::catch_unwind(|| panic!("boom-{}", 42));
        if let Some(p) = prev {
            std::env::set_var("XDG_DATA_HOME", p);
        } else {
            std::env::remove_var("XDG_DATA_HOME");
        }
        let log = fs::read_to_string(dir.join("yihu/logs/hooktest.log")).unwrap();
        assert!(log.contains("PANIC"), "{log}");
        assert!(log.contains("boom-42"), "{log}");
        assert!(log.contains("backtrace"), "{log}");
        let _ = fs::remove_dir_all(&dir);
    }
}
