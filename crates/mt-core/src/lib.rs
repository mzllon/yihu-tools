//! MiniTools 共享核心库。
//!
//! 封装 Linux `/proc`、`statvfs` 等系统信息的纯读取逻辑，
//! 以及自动深浅色主题切换的配置/调度/应用，
//! 供各小工具复用；不依赖任何窗口或事件框架。

pub mod autodark;
pub mod panel;
pub mod plugins;
pub mod sun;

use serde::Serialize;
use std::ffi::CString;
use std::fs;
use std::io;

fn read_to_string(path: &str) -> io::Result<String> {
    fs::read_to_string(path)
}

// ---- 进程查找 ----

/// 按进程名查找本用户的第一个进程，返回 (RSS kB, PSS kB)。
/// 数据来自 `/proc/<pid>/status` 与 `smaps_rollup`。
pub fn find_process_stats(name: &str) -> Option<(u64, u64)> {
    for entry in fs::read_dir("/proc").ok()? {
        let entry = entry.ok()?;
        let fname = entry.file_name();
        let fname = fname.to_string_lossy().into_owned();
        if !fname.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            continue;
        }
        let comm = fs::read_to_string(format!("/proc/{fname}/comm")).ok()?;
        if comm.trim() != name {
            continue;
        }
        let roll = fs::read_to_string(format!("/proc/{fname}/smaps_rollup")).ok()?;
        let (mut rss, mut pss) = (0u64, 0u64);
        for line in roll.lines() {
            if let Some(v) = line.strip_prefix("Rss:") {
                rss = v.trim().trim_end_matches(" kB").parse().unwrap_or(0);
            }
            if let Some(v) = line.strip_prefix("Pss:") {
                pss = v.trim().trim_end_matches(" kB").parse().unwrap_or(0);
            }
        }
        return Some((rss, pss));
    }
    None
}

// ---- 内存 ----

#[derive(Debug, Clone, Serialize)]
pub struct MemInfo {
    /// 物理内存总量（KiB）
    pub total_kb: u64,
    /// 可用内存（KiB），对应 `MemAvailable`（已扣除可回收缓存）
    pub available_kb: u64,
    pub swap_total_kb: u64,
    pub swap_used_kb: u64,
}

impl MemInfo {
    pub fn used_kb(&self) -> u64 {
        self.total_kb.saturating_sub(self.available_kb)
    }
}

/// 解析 `/proc/meminfo`。
pub fn read_mem_info() -> io::Result<MemInfo> {
    let text = read_to_string("/proc/meminfo")?;
    let mut total = 0;
    let mut available = 0;
    let mut swap_total = 0;
    let mut swap_free = 0;
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let (Some(key), Some(val)) = (it.next(), it.next()) else {
            continue;
        };
        let val: u64 = val.parse().unwrap_or(0);
        match key.strip_suffix(':') {
            Some("MemTotal") => total = val,
            Some("MemAvailable") => available = val,
            Some("SwapTotal") => swap_total = val,
            Some("SwapFree") => swap_free = val,
            _ => {}
        }
    }
    Ok(MemInfo {
        total_kb: total,
        available_kb: available,
        swap_total_kb: swap_total,
        swap_used_kb: swap_total.saturating_sub(swap_free),
    })
}

// ---- CPU ----

/// 单个逻辑核（或总体）在某一时刻的累计时间片。
#[derive(Debug, Clone, Default)]
pub struct CoreTimes {
    pub idle: u64,
    pub total: u64,
}

/// 一次 CPU 采样：总体 + 每个逻辑核。
#[derive(Debug, Clone, Default)]
pub struct CpuTimes {
    pub overall: CoreTimes,
    pub cores: Vec<CoreTimes>,
}

/// 读取 `/proc/stat` 中自开机以来的 CPU 累计时间片（单位：USER_HZ）。
pub fn read_cpu_times() -> io::Result<CpuTimes> {
    let text = read_to_string("/proc/stat")?;
    let mut out = CpuTimes::default();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("cpu") else {
            continue;
        };
        let fields: Vec<u64> = line
            .split_whitespace()
            .skip(1)
            .filter_map(|f| f.parse().ok())
            .collect();
        if fields.len() < 4 {
            continue;
        }
        // idle = 第 4 列 idle + 第 5 列 iowait
        let idle = fields[3] + fields.get(4).copied().unwrap_or(0);
        let total: u64 = fields.iter().sum();
        let t = CoreTimes { idle, total };
        if rest.starts_with(char::is_numeric) {
            out.cores.push(t);
        } else {
            out.overall = t;
        }
    }
    Ok(out)
}

/// 根据两次采样差分出使用率（0–100）：总体 + 每核。
/// 间隔为 0 或计数回退（如热插拔）时按 0 处理。
pub fn cpu_usage(prev: &CpuTimes, cur: &CpuTimes) -> (f32, Vec<f32>) {
    fn pct(a: &CoreTimes, b: &CoreTimes) -> f32 {
        let dt = b.total.saturating_sub(a.total);
        let di = b.idle.saturating_sub(a.idle);
        if dt == 0 {
            return 0.0;
        }
        (((dt - di) as f32 / dt as f32) * 100.0).clamp(0.0, 100.0)
    }
    let overall = pct(&prev.overall, &cur.overall);
    let cores = cur
        .cores
        .iter()
        .zip(prev.cores.iter())
        .map(|(c, p)| pct(p, c))
        .collect();
    (overall, cores)
}

// ---- 负载 / 运行时间 ----

/// 读取 `/proc/loadavg`：1 / 5 / 15 分钟平均负载。
pub fn read_loadavg() -> io::Result<[f32; 3]> {
    let text = read_to_string("/proc/loadavg")?;
    let mut it = text.split_whitespace();
    let mut out = [0f32; 3];
    for slot in &mut out {
        *slot = it.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
    }
    Ok(out)
}

/// 系统已运行秒数，来自 `/proc/uptime`。
pub fn read_uptime_secs() -> io::Result<f64> {
    let text = read_to_string("/proc/uptime")?;
    Ok(text
        .split_whitespace()
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0))
}

// ---- 主机信息 ----

#[derive(Debug, Clone, Serialize)]
pub struct HostInfo {
    pub hostname: String,
    pub kernel: String,
    pub cpu_model: String,
}

/// 读取运行期内基本不变的主机信息（主机名 / 内核版本 / CPU 型号）。
pub fn read_host_info() -> HostInfo {
    let hostname = read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let kernel = read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let cpu_model = read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|t| {
            t.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split_once(':'))
                .map(|(_, v)| v.trim().to_string())
        })
        .unwrap_or_default();
    HostInfo {
        hostname,
        kernel,
        cpu_model,
    }
}

// ---- 磁盘 ----

#[derive(Debug, Clone, Serialize)]
pub struct DiskUsage {
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub avail_bytes: u64,
}

impl DiskUsage {
    pub fn used_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.free_bytes)
    }
}

/// 查询某个挂载点的容量占用（如 `"/"`）。
pub fn read_disk_usage(mount: &str) -> io::Result<DiskUsage> {
    let c = CString::new(mount).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "挂载点路径包含 NUL 字节")
    })?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: c 是合法的以 NUL 结尾的字符串，st 是同类型可写缓冲。
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let frsize = st.f_frsize.max(1) as u64;
    Ok(DiskUsage {
        total_bytes: st.f_blocks as u64 * frsize,
        free_bytes: st.f_bfree as u64 * frsize,
        avail_bytes: st.f_bavail as u64 * frsize,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_real_proc_entries() {
        let mem = read_mem_info().unwrap();
        assert!(mem.total_kb > 0);
        assert!(mem.available_kb <= mem.total_kb);

        let t1 = read_cpu_times().unwrap();
        assert!(!t1.cores.is_empty());
        std::thread::sleep(std::time::Duration::from_millis(120));
        let t2 = read_cpu_times().unwrap();
        let (overall, cores) = cpu_usage(&t1, &t2);
        assert!((0.0..=100.0).contains(&overall));
        assert_eq!(cores.len(), t2.cores.len());

        let load = read_loadavg().unwrap();
        assert!(load[0] >= 0.0);
        assert!(read_uptime_secs().unwrap() > 0.0);

        let host = read_host_info();
        assert!(!host.hostname.is_empty());

        let disk = read_disk_usage("/").unwrap();
        assert!(disk.total_bytes > 0);
    }
}
