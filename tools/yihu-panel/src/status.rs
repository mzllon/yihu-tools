//! 插件运行时状态（中心插件页配套）：/proc 实扫聚合，不碰面板内部状态。
//!
//! 为什么走 /proc 而不是会话注册表：状态查询来自中心应用的 D-Bus 调用，
//! 在 zbus 执行线程上（不能触碰主线程的 Rc<PluginMgr>）；且 /proc 是
//! 「进程真实存在」的地面真相——会话表可能滞后于进程死亡。
//!
//! 插件识别：插件沙箱统一经 bwrap 启动，cmdline 含
//! `--ro-bind <注册表>/<插件id> /plugin`——取挂载点前一参的尾段即插件 id；
//! 进程组 = bwrap 的 pgid（sandbox.rs process_group(0) 约定），组内所有
//! 成员的 RSS/CPU 求和。
//!
//! CPU% 为两次查询间的差分（间隔 <0.7s 时复用上次值防抖动）。

use std::collections::HashMap;
use std::time::Instant;

use serde_json::json;

/// /proc/<pid>/stat 解析 → (pgrp, utime+stime ticks, rss 页数)。
/// comm 可能含空格与括号：取最后一个 ')' 之后切分。
/// 字段序（剥掉 pid/comm 后）：state(3) ppid(4) pgrp(5) … utime(14)
/// stime(15) … rss(24)——切片下标 = 原字段号 - 3。
pub fn parse_stat(text: &str) -> Option<(i32, u64, u64)> {
    let after = text.rsplit_once(')')?.1.trim_start();
    let f: Vec<&str> = after.split_whitespace().collect();
    let pgrp = f.get(2)?.parse().ok()?;
    let utime: u64 = f.get(11)?.parse().ok()?;
    let stime: u64 = f.get(12)?.parse().ok()?;
    let rss_pages: u64 = f.get(21)?.parse().ok()?;
    Some((pgrp, utime + stime, rss_pages))
}

fn read_cmdline(pid: i32) -> Option<Vec<String>> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    Some(
        raw.split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).to_string())
            .collect(),
    )
}

/// bwrap 进程 → 插件 id（挂载点 "/plugin" 的前一参尾段）
fn plugin_id_of_bwrap(args: &[String]) -> Option<String> {
    for (i, a) in args.iter().enumerate() {
        if a == "/plugin" && i > 0 {
            let id = args[i - 1].rsplit_once('/')?.1.to_string();
            if !id.is_empty() {
                return Some(id);
            }
        }
    }
    None
}

struct ProcInfo {
    pgrp: i32,
    ticks: u64,
    rss_kb: u64,
}

/// 扫描全部进程的 stat（读失败的跳过）
fn scan_all() -> Vec<ProcInfo> {
    let page_kb = 4; // stat 的 rss 单位是页；x86_64 页 4KiB
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return out;
    };
    for e in entries.flatten() {
        let Some(pid) = e.file_name().to_string_lossy().parse::<i32>().ok() else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        if let Some((pgrp, ticks, rss_pages)) = parse_stat(&text) {
            out.push(ProcInfo {
                pgrp,
                ticks,
                rss_kb: rss_pages * page_kb,
            });
        }
    }
    out
}

/// 插件 id → bwrap 主进程号（cmdline 识别）
fn plugin_leaders() -> HashMap<String, Vec<i32>> {
    let mut out: HashMap<String, Vec<i32>> = HashMap::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return out;
    };
    for e in entries.flatten() {
        let Some(pid) = e.file_name().to_string_lossy().parse::<i32>().ok() else {
            continue;
        };
        let Some(args) = read_cmdline(pid) else {
            continue;
        };
        if args.first().map(|s| s == "bwrap").unwrap_or(false) {
            if let Some(id) = plugin_id_of_bwrap(&args) {
                out.entry(id).or_default().push(pid);
            }
        }
    }
    out
}

thread_local! {
    /// CPU% 差分采样缓存（状态查询只在中心页触发，thread_local 够用：
    /// zbus 方法在同一执行线程上回调）
    static LAST: RefCell<HashMap<String, (Instant, u64, f64)>> = RefCell::new(HashMap::new());
}
use std::cell::RefCell;

fn clk_tck() -> f64 {
    unsafe { libc::sysconf(libc::_SC_CLK_TCK) as f64 }
}

/// 两次查询差分 CPU%；间隔过短复用上次值
fn cpu_percent(id: &str, ticks_now: u64) -> f64 {
    LAST.with(|m| {
        let mut m = m.borrow_mut();
        let hz = clk_tck().max(1.0);
        if let Some((t0, ticks0, pct0)) = m.get(id) {
            let dt = t0.elapsed().as_secs_f64();
            if dt < 0.7 {
                return *pct0;
            }
            let pct = (((ticks_now - ticks0) as f64 / hz) / dt * 100.0).clamp(0.0, 100.0);
            m.insert(id.to_string(), (Instant::now(), ticks_now, pct));
            pct
        } else {
            m.insert(id.to_string(), (Instant::now(), ticks_now, 0.0));
            0.0
        }
    })
}

/// 汇总全部已装插件的运行态（/proc 实扫 + 注册表清单）
pub fn plugin_status_json() -> String {
    let (installed, _) = yihu_core::plugins::list_installed();
    let state = yihu_core::plugins::PluginsState::load();
    let leaders = plugin_leaders();
    let procs = scan_all();

    let mut items = Vec::new();
    for inst in &installed {
        let id = &inst.manifest.id;
        let leaders = leaders.get(id);
        let running = leaders.is_some();
        let (mut procs_n, mut rss_kb, mut ticks) = (0u32, 0u64, 0u64);
        if let Some(ls) = leaders {
            for p in &procs {
                if ls.contains(&p.pgrp) {
                    procs_n += 1;
                    rss_kb += p.rss_kb;
                    ticks += p.ticks;
                }
            }
        }
        items.push(json!({
            "id": id,
            "running": running,
            "resident": inst.manifest.resident,
            "enabled": !state.is_disabled(id),
            "procs": procs_n,
            "rss_kb": rss_kb,
            "cpu_percent": if running { cpu_percent(id, ticks) } else { 0.0 },
            "cpu_time_s": ticks / clk_tck().max(1.0) as u64,
        }));
    }
    serde_json::to_string(&items).unwrap_or_else(|_| "[]".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stat_with_spaces_in_comm() {
        // comm 含空格与括号：rsplit 兜底
        // 字段序剥壳后：state(0) ppid(1) pgrp(2) … utime(11) stime(12) … rss(21)
        let text = "1234 (bwrap: :x y) S 1 1234 100 0 0 0 0 0 0 0 5 6 0 0 0 0 0 0 0 0 100";
        let (pgrp, ticks, rss) = parse_stat(text).unwrap();
        assert_eq!(pgrp, 1234);
        assert_eq!(ticks, 11); // utime 5 + stime 6
        assert_eq!(rss, 100);
    }

    #[test]
    fn parse_stat_rejects_garbage() {
        assert!(parse_stat("not a stat").is_none());
        assert!(parse_stat("").is_none());
    }

    #[test]
    fn plugin_id_extracted_from_mount_pair() {
        let args: Vec<String> = [
            "bwrap",
            "--ro-bind",
            "/home/u/.local/share/yihu/plugins/devcodec",
            "/plugin",
            "/plugin/devcodec.py",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(plugin_id_of_bwrap(&args).as_deref(), Some("devcodec"));
        // 无挂载点
        assert!(plugin_id_of_bwrap(&["bwrap".into(), "--proc".into(), "/proc".into()]).is_none());
    }
}
