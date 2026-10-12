//! passgen WASM 版：随机字符 / 数字 PIN / 易读短语。
//! 随机源 = xorshift（宿主时间+地址熵）——密码级随机由宿主能力提供（W3）。

use yihu_wasm_runtime::{QueryHandler, Row};

const UPPER: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const LOWER: &str = "abcdefghijklmnopqrstuvwxyz";
const DIGIT: &str = "0123456789";
const SYMS: &str = "!@#$%^&*";
const CONS: &str = "bcdfghjklmnpqrstvwxz";
const VOW: &str = "aeiou";

struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Rng {
        let probe = [0u8; 8];
        Rng(
            seed.wrapping_mul(0x9E37_79B9_7F4A_7C15)
                ^ (&probe as *const _ as u64).rotate_left(32),
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
    fn pick(&mut self, s: &str) -> char {
        s.chars().nth((self.next() % s.len() as u64) as usize).unwrap()
    }
}

struct PassgenWasm;

impl QueryHandler for PassgenWasm {
    fn handle(text: &str, now: i64) -> Vec<Row> {
        let low = text.trim().to_lowercase();
        let prefixed = low.starts_with("pass ") || low.starts_with("密码 ");
        if !matches!(low.as_str(), "pass" | "密码" | "passgen") && !prefixed {
            return Vec::new();
        }
        let length = if prefixed {
            text.split_once(' ')
                .and_then(|(_, r)| r.trim().parse::<usize>().ok())
                .map(|n| n.clamp(6, 64))
                .unwrap_or(16)
        } else {
            16
        };
        let mut rng = Rng::new(now as u64);
        let mut rows = Vec::new();
        for i in 1..=3 {
            let mut chars: Vec<char> = Vec::with_capacity(length);
            // 每类至少一位
            chars.push(rng.pick(UPPER));
            chars.push(rng.pick(LOWER));
            chars.push(rng.pick(DIGIT));
            chars.push(rng.pick(SYMS));
            let pool = format!("{UPPER}{LOWER}{DIGIT}{SYMS}");
            while chars.len() < length {
                chars.push(rng.pick(&pool));
            }
            // Fisher–Yates
            for k in (1..chars.len()).rev() {
                let j = (rng.next() % (k as u64 + 1)) as usize;
                chars.swap(k, j);
            }
            let pwd: String = chars.into_iter().collect();
            rows.push(Row::new(
                &pwd,
                &format!("随机字符 · {length} 位 · 每类至少一位 · 第 {i} 个"),
                &pwd,
            ));
        }
        rows
    }
}

yihu_wasm_runtime::export_plugin!(PassgenWasm);
