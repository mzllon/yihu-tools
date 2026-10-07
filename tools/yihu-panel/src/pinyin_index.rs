//! 拼音/首字母索引（M4 穿插小项落地，对标 uTools 拼音搜索）。
//!
//! 数据来源：内嵌 rust-pinyin 字表（离线、纯 Rust、无运行时 IO），
//! 应用枚举/热刷新时一次性生成匹配列（非按键路径），进 nucleo 第二列。
//!
//! 匹配列构造：`match_column("文件管理")` = `"wenjianguanli wwjgl"`——
//! 全拼 + 首字母两段，用户输入 "wenjian"、"wjgl" 均可命中（nucleo
//! 模糊匹配，最佳列得分）。非中文字符小写透传，中英混排（"WPS表格"）
//! 两侧都可搜。

/// 生成 nucleo 匹配列：全拼 + 首字母（非中文小写透传，混在两段中）。
pub fn match_column(s: &str) -> String {
    use pinyin::ToPinyin;
    let mut full = String::with_capacity(s.len() * 3);
    let mut initials = String::with_capacity(s.len());
    for (ch, py) in s.chars().zip(s.to_pinyin()) {
        match py {
            Some(p) => {
                let plain = p.plain();
                full.push_str(plain);
                // plain 无声调，首字节必是 ASCII 小写字母
                if let Some(c) = plain.bytes().next() {
                    initials.push(c as char);
                }
            }
            None => {
                let lower = ch.to_lowercase().to_string();
                full.push_str(&lower);
                // 只把字母数字收进首字母段（空格/标点作分隔，防止跨词误拼）
                if lower.chars().next().is_some_and(|c| c.is_ascii_alphanumeric()) {
                    initials.push_str(&lower);
                }
            }
        }
    }
    full.push(' ');
    full.push_str(&initials);
    full
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chinese_full_and_initials() {
        assert_eq!(match_column("文件管理"), "wenjianguanli wjgl");
        assert_eq!(match_column("设置"), "shezhi sz");
        assert_eq!(match_column("一呼"), "yihu yh");
    }

    #[test]
    fn mixed_script_passes_through() {
        // 英文小写透传；大写归一为小写
        assert_eq!(match_column("WPS Office"), "wps office wpsoffice");
        // 中文 + 英文：全拼段含拼音与英文，首字母段中文取首字母、英文整词
        assert_eq!(match_column("WPS表格"), "wpsbiaoge wpsbg");
    }

    #[test]
    fn punctuation_separates_initials() {
        // 标点不进首字母段：不会把 "-network" 拼成 "wn"
        let col = match_column("Network-设置");
        assert_eq!(col, "network-shezhi networksz");
    }

    #[test]
    fn empty_and_ascii_only() {
        assert_eq!(match_column(""), " ");
        assert_eq!(match_column("abc"), "abc abc");
    }

    #[test]
    fn digits_included() {
        assert_eq!(match_column("7zip压缩"), "7zipyasuo 7zipys");
    }
}
