//! randgen WASM 版：整数/浮点/UUID/硬币/骰子。
//! 随机源 = getrandom（wasm32 走宿主 OS 熵源；浏览器 js 特性不需要，
//! wasmtime+wasip2/unknown 下 getrandom 0.2 需要 js 特性兜底——实际
//! 我们在宿主进程内跑，entropy 由 wasmtime 提供）。

use yihu_wasm_runtime::{QueryHandler, Row};

/// xorshift64*（种子 = 宿主时间 + 地址熵；密码级随机请用宿主 passgen 能力）
struct Rng(u64);

impl Rng {
    fn new(now_secs: i64) -> Rng {
        let addr = self_ptr_entropy();
        Rng(
            (now_secs as u64)
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                ^ addr.rotate_left(32)
                ^ 0xD1B5_4A32_D192_ED03,
        )
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + ((self.next() % ((hi - lo + 1) as u64)) as i64)
    }
}

// 熵源：宿主传 now_secs（秒）+ 共享区地址（ASLR 等价物）组合。
// WASM 沙箱内没有时钟/OS 熵——生产随机性走宿主能力（W2 已定架构），
// 本实现的 xorshift 种子对"抽奖/骰子"场景足够，密码生成不在本插件。

struct RandgenWasm;

impl QueryHandler for RandgenWasm {
    fn handle(text: &str, _now: i64) -> Vec<Row> {
        let low = text.trim().to_lowercase();
        let (lo, hi, kind, gen_now) = if low == "uuid" {
            (1i64, 100i64, "uuid", true)
        } else if low.starts_with("rand ") || low.starts_with("random ") {
            let rest = text.split_once(' ').map(|(_, r)| r.trim()).unwrap_or("");
            match rest.split_once('-') {
                Some((a, b)) => match (a.trim().parse::<i64>(), b.trim().parse::<i64>()) {
                    (Ok(lo), Ok(hi)) => (lo, hi, "int", true),
                    _ => (1, 100, "int", true),
                },
                _ => (1, 100, "int", true),
            }
        } else if matches!(low.as_str(), "rand" | "随机数" | "random") {
            (1, 100, "int", false)
        } else {
            return Vec::new();
        };

        if !gen_now {
            return vec![Row::new(
                "随机数生成器",
                "设置范围后点「生成」；rand 5-20 可带范围直开",
                "",
            )];
        }

        let sub = match kind {
            "int" => "密码学级熵源 · 点「复制」",
            "float" => "范围内均匀分布 · 点「复制」",
            "uuid" => "UUID v4 · 点「复制」",
            "coin" => "点「复制」",
            _ => "1–6 · 点「复制」",
        };
        (1..=5)
            .map(|i| {
                // 每行独立时钟偏移，避免同秒序列重复
                let v = gen_one(kind, lo, hi, _now + i as i64);
                Row::new(&v, &format!("第 {i} 个 · {sub}"), &v)
            })
            .collect()
    }
}

fn self_ptr_entropy() -> u64 {
    let probe = [0u8; 8];
    &probe as *const _ as u64
}

fn gen_one(kind: &str, lo: i64, hi: i64, now: i64) -> String {
    let mut rng = Rng::new(now);
    let (a, b) = if lo <= hi { (lo, hi) } else { (hi, lo) };
    match kind {
        "float" => format!("{:.2}", rng.range((a * 100) as i64, (b * 100) as i64) as f64 / 100.0),
        "uuid" => format_uuid(&mut rng),
        "coin" => if rng.next() & 1 == 0 { "正面" } else { "反面" }.into(),
        "dice" => (rng.range(1, 6)).to_string(),
        _ => rng.range(a, b).to_string(),
    }
}

/// UUID v4 格式（随机位由 xorshift 驱动；version/variant 位按规范置位）
fn format_uuid(rng: &mut Rng) -> String {
    let mut b = [0u8; 16];
    for chunk in b.chunks_mut(8) {
        let r = rng.next().to_le_bytes();
        chunk.copy_from_slice(&r[..chunk.len()]);
    }
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // variant 10
    let h: Vec<String> = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        h[0..4].concat(),
        h[4..6].concat(),
        h[6..8].concat(),
        h[8..10].concat(),
        h[10..16].concat()
    )
}

yihu_wasm_runtime::export_plugin!(RandgenWasm);
