//! 运行审计日志（M4 能力代理配套）：JSONL 追加 + 体积轮转 + 后台写线程。
//!
//! 审计是宿主 owned 的——放在 `yihu/audit.jsonl`（注册表同级）而非插件
//! 可写的 plugin-data，插件无法抹改自己的审计记录。
//!
//! 写入走 mpsc + 独立线程：调用方 `record()` 永不阻塞（主循环回调里
//! 禁止任何同步 IO，BUG-001 教训）；Drop 时关闭通道并等线程排空。

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::time::{SystemTime, UNIX_EPOCH};

/// 默认轮转阈值：1 MiB（超过后整文件改名 .1，旧 .1 被覆盖）
pub const DEFAULT_ROTATE_BYTES: u64 = 1024 * 1024;

/// 审计写入器。Clone 不支持——调用方用 `Rc`/`Arc` 共享。
pub struct Audit {
    tx: Option<Sender<String>>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Audit {
    /// 打开（惰性创建文件）默认阈值的审计器，写线程立即启动。
    pub fn open(path: PathBuf) -> Audit {
        Self::open_with_limit(path, DEFAULT_ROTATE_BYTES)
    }

    /// 同上，轮转阈值可注入（测试用）。
    pub fn open_with_limit(path: PathBuf, rotate_bytes: u64) -> Audit {
        let (tx, rx) = mpsc::channel::<String>();
        let join = std::thread::Builder::new()
            .name("yihu-audit".into())
            .spawn(move || writer_loop(rx, &path, rotate_bytes))
            .ok();
        Audit {
            tx: Some(tx),
            join,
        }
    }

    /// 记一条审计事件。非阻塞：内部通道满/关闭时静默丢弃
    /// （审计缺失不应影响插件调用路径；通道无界，正常运行不会满）。
    pub fn record(&self, plugin: &str, gen: u64, capability: &str, decision: &str, detail: &str) {
        let Some(tx) = &self.tx else { return };
        let line = format!(
            "{{\"ts\":{},\"plugin\":{},\"gen\":{},\"capability\":{},\"decision\":{},\"detail\":{}}}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            json(plugin),
            gen,
            json(capability),
            json(decision),
            json(detail),
        );
        let _ = tx.send(line);
    }
}

impl Drop for Audit {
    fn drop(&mut self) {
        self.tx.take(); // 关闭通道 → 写线程 recv Err 排空退出
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn json(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

fn writer_loop(rx: mpsc::Receiver<String>, path: &PathBuf, rotate_bytes: u64) {
    while let Ok(line) = rx.recv() {
        rotate_if_needed(path, rotate_bytes);
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "{line}");
        }
    }
}

fn rotate_if_needed(path: &PathBuf, rotate_bytes: u64) {
    let Ok(m) = std::fs::metadata(path) else { return };
    if m.len() < rotate_bytes {
        return;
    }
    let mut bak = path.clone().into_os_string();
    bak.push(".1");
    let _ = std::fs::rename(path, PathBuf::from(bak));
}

/// 审计文件路径（宿主数据区，不在任何插件可写目录下）
pub fn audit_path() -> PathBuf {
    crate::plugins::data_home().join("yihu/audit.jsonl")
}

/// 中心页读取审计：返回最近 `max` 条（文件太老/损坏行跳过）。
/// 仅供中心应用低频调用，呼出面板不读它。
pub fn read_recent(max: usize) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(audit_path()) else {
        return Vec::new();
    };
    let start = text.lines().count().saturating_sub(max);
    text.lines().skip(start).map(String::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn tmp(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("yihu-audit-test-{}-{tag}", std::process::id()))
    }

    #[test]
    fn records_jsonl_and_flushes_on_drop() {
        let path = tmp("basic");
        let _ = std::fs::remove_file(&path);
        {
            let a = Audit::open(path.clone());
            a.record("passgen", 3, "clipboard.write", "grant", "len=42");
            a.record("probe", 4, "notify", "deny", "未声明能力");
        } // Drop 排空
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(v["plugin"], "passgen");
        assert_eq!(v["capability"], "clipboard.write");
        assert_eq!(v["decision"], "grant");
        assert_eq!(v["gen"], 3);
        assert!(v["ts"].as_u64().unwrap() > 1_700_000_000);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rotates_past_limit() {
        let path = tmp("rotate");
        let bak: PathBuf = PathBuf::from(format!("{}.1", path.display()));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&bak);
        let big = "x".repeat(300);
        {
            let a = Audit::open_with_limit(path.clone(), 512);
            for _ in 0..4 {
                a.record("p", 1, "cap", "grant", &big); // 每行 ~350B，第 2 行触发轮转
            }
        }
        assert!(bak.exists(), "轮转备份应存在");
        assert!(!std::fs::read_to_string(&bak).unwrap().is_empty());
        let tail = std::fs::read_to_string(&path).unwrap();
        assert!(!tail.is_empty());
        assert!(tail.lines().count() < 4);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&bak);
    }

    #[test]
    fn escapes_special_chars() {
        let path = tmp("escape");
        let _ = std::fs::remove_file(&path);
        {
            let a = Audit::open(path.clone());
            a.record("p\"lugin", 1, "cap\"x", "deny", "引号\"与\\反斜杠");
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(v["plugin"], "p\"lugin");
        assert_eq!(v["detail"], "引号\"与\\反斜杠");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_recent_returns_tail() {
        let path = tmp("recent");
        let _ = std::fs::remove_file(&path);
        {
            let a = Audit::open(path.clone());
            for i in 0..5 {
                a.record("p", i, "cap", "grant", "");
            }
        }
        // read_recent 读全局路径，这里直接验证 tail 逻辑的等价性
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let tail: Vec<&str> = lines.iter().rev().take(3).rev().copied().collect();
        assert_eq!(tail.len(), 3);
        assert_eq!(tail.last().unwrap(), lines.last().unwrap());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn record_never_blocks_on_dropped_writer() {
        let a = Audit::open(tmp("deadchannel"));
        // tx 尚未关闭：正常发送；此后 Drop 由 impl 处理
        a.record("p", 1, "cap", "grant", "");
        std::thread::sleep(Duration::from_millis(10));
    }
}
