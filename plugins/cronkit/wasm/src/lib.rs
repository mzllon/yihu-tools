//! cronkit WASM 版：完整 cron 解析/人话解读/预览逻辑。
//! 与宿主的交互 = Linker 导入函数（无 stdio）：
//! - 宿主调 query(ptr,len) 传文本
//! - 插件写结果到内部缓冲 → 宿主调 result_count/result_title 等取回

use core::cell::RefCell;

// ============ 宿主交互内存 ============

/// 宿主↔插件共享区：固定地址（静态数组不搬家，宿主持有的偏移永远有效）
const HEAP_CAP: usize = 1 << 20; // 1 MiB
static mut HEAP: [u8; HEAP_CAP] = [0u8; HEAP_CAP];
static mut HEAP_LEN: usize = 0;

/// 数组在线性内存中的基地址（宿主所有偏移 = 基地址 + 数组内偏移）
#[no_mangle]
pub extern "C" fn heap_base() -> i32 {
    unsafe { &raw mut HEAP as usize as i32 }
}

#[no_mangle]
pub extern "C" fn alloc(size: i32) -> i32 {
    unsafe {
        let start = HEAP_LEN as i32;
        let new_len = HEAP_LEN + size as usize;
        if new_len > HEAP_CAP {
            return -1;
        }
        HEAP_LEN = new_len;
        // 返回线性内存绝对地址（基地址 + 内部偏移）
        (&raw mut HEAP as usize + start as usize) as i32
    }
}

#[no_mangle]
pub extern "C" fn memory_size() -> i32 {
    unsafe { HEAP_LEN as i32 }
}


// ============ 结果缓冲 ============

struct Row {
    title: String,
    subtitle: String,
    payload: String,
}

static mut ROWS: Option<Vec<Row>> = None;

fn rows() -> &'static mut Vec<Row> {
    unsafe {
        let ptr = &raw mut ROWS;
        (*ptr).get_or_insert_with(Vec::new)
    }
}

#[no_mangle]
pub extern "C" fn result_count() -> i32 {
    rows().len() as i32
}

/// 取第 i 行的字段：field = 0 title / 1 subtitle / 2 payload
/// 返回值 = 该字符串在 HEAP 中的起始偏移；长度写入 out_len
#[no_mangle]
pub extern "C" fn result_field(i: i32, field: i32) -> i32 {
    let rows = rows();
    let Some(row) = rows.get(i as usize) else { return -1i32 };
    let s = match field {
        0 => &row.title,
        1 => &row.subtitle,
        _ => &row.payload,
    };
    let bytes = s.as_bytes();
    let start = unsafe { HEAP_LEN as i32 };
    unsafe {
        let end = HEAP_LEN + bytes.len();
        if end <= HEAP_CAP {
            HEAP[HEAP_LEN..end].copy_from_slice(bytes);
            HEAP_LEN = end;
        }
    }
    unsafe { (&raw mut HEAP as usize + start as usize) as i32 }
}

#[no_mangle]
pub extern "C" fn result_field_len(i: i32, field: i32) -> i32 {
    let rows = rows();
    let Some(row) = rows.get(i as usize) else { return 0i32 };
    match field {
        0 => row.title.len() as i32,
        1 => row.subtitle.len() as i32,
        _ => row.payload.len() as i32,
    }
}

// ============ cron 逻辑（与 Rust 版 cronkit 同源） ============

const SPECIAL: &[(&str, &str)] = &[
    ("@hourly", "0 * * * *"),
    ("@daily", "0 0 * * *"),
    ("@weekly", "0 0 * * 0"),
    ("@monthly", "0 0 1 * *"),
    ("@yearly", "0 0 1 1 *"),
];

const WEEK: [&str; 7] = ["周日", "周一", "周二", "周三", "周四", "周五", "周六"];

fn parse_field(f: &str, lo: i64, hi: i64) -> Result<Vec<i64>, ()> {
    let mut vals = Vec::new();
    for part in f.split(',') {
        if let Some((base, step)) = part.split_once('/') {
            let step: i64 = step.parse().map_err(|_| ())?;
            if step < 1 { return Err(()); }
            let start = if base == "*" || base.is_empty() { lo } else { base.parse().map_err(|_| ())? };
            let mut v = start;
            while v <= hi { vals.push(v); v += step; }
        } else if let Some((a, b)) = part.split_once('-') {
            let a: i64 = a.parse().map_err(|_| ())?;
            let b: i64 = b.parse().map_err(|_| ())?;
            if a > b { return Err(()); }
            vals.extend(a..=b);
        } else if part == "*" {
            vals.extend(lo..=hi);
        } else {
            vals.push(part.parse().map_err(|_| ())?);
        }
    }
    vals.retain(|v| (lo..=hi).contains(v));
    vals.sort_unstable();
    if vals.is_empty() { Err(()) } else { Ok(vals) }
}

/// 天数 → (y, m, d)， civil 算法（chrono 无 std 依赖版）
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe/1460 + doe/36524 - doe/146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365*yoe + yoe/4 - yoe/100);
    let mp = (5*doy + 2)/153;
    let d = (doy - (153*mp+2)/5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mm = m as i64;
    let doy = (153 * (if mm > 2 { mm - 3 } else { mm + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe/4 - yoe/100 + doy;
    era * 146_097 + doe - 719_468
}

/// Unix 秒 → 周几（0=周日）
fn weekday_from_days(days: i64) -> u32 {
    (days + 4).rem_euclid(7) as u32
}

struct Ctx {
    mins: Vec<i64>, hrs: Vec<i64>, doms: Vec<i64>, mons: Vec<i64>, dows: Vec<i64>,
    dom_star: bool, dow_star: bool,
}

fn next_runs(c: &Ctx, count: usize, now_secs: i64) -> Vec<(i64, u32, u32, u32, u32)> {
    let start_day = now_secs.div_euclid(86400);
    let mut out = Vec::new();
    'outer: for day_off in 0..1500i64 {
        let days = start_day + day_off;
        let (y, mo, d) = civil_from_days(days);
        if !(c.mons.contains(&(mo as i64))) { continue; }
        let dow = weekday_from_days(days);
        let dom_ok = c.doms.contains(&(d as i64));
        let dow_ok = c.dows.contains(&(dow as i64));
        let day_ok = match (c.dom_star, c.dow_star) {
            (true, true) => true,
            (true, false) => dow_ok,
            (false, true) => dom_ok,
            (false, false) => dom_ok || dow_ok,
        };
        if !day_ok { continue; }
        for &h in &c.hrs {
            for &mi in &c.mins {
                let secs = days * 86400 + h * 3600 + mi * 60;
                if secs <= now_secs { continue; }
                out.push((secs, y as u32, mo, d, (h * 60 + mi) as u32));
                if out.len() >= count { break 'outer; }
            }
        }
    }
    out
}

fn fmt_time(hm: u32) -> String {
    format!("{:02}:{:02}", hm / 60, hm % 60)
}

fn push_rows(expr: &str, count: usize, now_secs: i64) -> Result<(), ()> {
    let lowered = expr.to_lowercase();
    let fields = SPECIAL.iter().find(|(k, _)| *k == lowered)
        .map(|(_, v)| *v).unwrap_or(expr);
    let parts: Vec<&str> = fields.split_whitespace().collect();
    if parts.len() != 5 { return Err(()); }
    let ctx = Ctx {
        mins: parse_field(parts[0], 0, 59)?,
        hrs: parse_field(parts[1], 0, 23)?,
        doms: parse_field(parts[2], 1, 31)?,
        mons: parse_field(parts[3], 1, 12)?,
        dows: parse_field(parts[4], 0, 7)?.into_iter().map(|v| if v == 7 { 0 } else { v }).collect(),
        dom_star: parts[2] == "*",
        dow_star: parts[4] == "*",
    };
    // 人话解读
    let time_txt = if ctx.mins.len() == 60 {
        "每小时".to_string()
    } else if ctx.hrs.len() == 24 {
        format!("每小时第 {} 分", ctx.mins.iter().take(8).map(|m| m.to_string()).collect::<Vec<_>>().join("、"))
    } else if ctx.hrs.len() == 1 && ctx.mins.len() == 1 {
        format!("{:02}:{:02}", ctx.hrs[0], ctx.mins[0])
    } else {
        let times: Vec<String> = ctx.hrs.iter().take(4)
            .flat_map(|h| ctx.mins.iter().take(1).map(move |m| format!("{h:02}:{m:02}"))).collect();
        format!("{}{}", times.join("、"), if ctx.hrs.len() > 4 { "…" } else { "" })
    };
    let week_txt = ctx.dows.iter().map(|d| WEEK[*d as usize % 7]).collect::<Vec<_>>().join("、");
    let when = match (ctx.dom_star, ctx.dow_star) {
        (true, true) => "每天".to_string(),
        (true, false) => format!("每{week_txt}"),
        (false, true) => format!("每月{}", ctx.doms.iter().take(8).map(|d| format!("{d}日")).collect::<Vec<_>>().join("、")),
        (false, false) => format!("{}日 或 {}", ctx.doms.iter().take(8).map(|d| d.to_string()).collect::<Vec<_>>().join("、"), week_txt),
    };
    rows().push(Row {
        title: format!("📋 {when} {time_txt}"),
        subtitle: "表达式解读".into(),
        payload: String::new(),
    });
    for (secs, y, mo, d, hm) in next_runs(&ctx, count, now_secs) {
        let (hh, mm) = (hm / 60, hm % 60);
        rows().push(Row {
            title: format!("{y}-{mo:02}-{d:02} {hh:02}:{mm:02} ({})", WEEK[weekday_from_days(secs.div_euclid(86400)) as usize]),
            subtitle: "下次执行 · 点「复制」".into(),
            payload: format!("{y}-{mo:02}-{d:02} {hh:02}:{mm:02}:00"),
        });
    }
    Ok(())
}

// ============ 宿主入口 ============

/// 宿主调 query(text_ptr, text_len)：解析并填充结果缓冲。
/// now_secs 由宿主传入（WASM 无时钟）。
#[no_mangle]
pub extern "C" fn query(ptr_abs: i32, len: i32, now_secs: i64) -> i32 {
    unsafe {
        // 宿主持有的是线性内存绝对地址；转换为数组内偏移
        let base = &raw mut HEAP as usize;
        let ptr = (ptr_abs as usize - base) as i32;
        let text = String::from_utf8_lossy(
            &HEAP[ptr as usize..(ptr + len) as usize],
        )
        .to_string();
        rows().clear();
        let low = text.trim().to_lowercase();
        let prefixed = low.starts_with("cron ") || low.starts_with("crontab ");
        if !matches!(low.as_str(), "cron" | "crontab" | "定时") && !prefixed {
            return 0;
        }
        let expr = if prefixed {
            text.split_once(' ').map(|(_, r)| r.trim().to_string()).unwrap_or_default()
        } else {
            String::new()
        };
        if expr.is_empty() {
            rows().push(Row {
                title: "cron 表达式".into(),
                subtitle: "输入 cron 0 9 * * 1-5 查看解读与预览".into(),
                payload: String::new(),
            });
            return 0;
        }
        match push_rows(&expr, 5, now_secs) {
            Ok(()) => 0,
            Err(()) => {
                rows().push(Row {
                    title: "✗ 表达式非法".into(),
                    subtitle: "应为 5 段：分 时 日 月 周".into(),
                    payload: String::new(),
                });
                -1
            }
        }
    }
}
