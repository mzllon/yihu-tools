//! 主题：系统深浅色 + 强调色 → 运行时生成样式表。
//!
//! 与 yihu-panel/src/theme.rs 同源口径（accent 从 gsettings 读，GNOME 47+
//! accent-color 枚举，custom 时读 accent-bg-color；底色按 libadwaita Yaru
//! 口径两套）。读取在启动与 3s 轮询里做（实时 dconf 通知不可靠就轮询闭环，
//! BUG-004 教训）；变化时由 main 重建注入。

/// 系统强调色（GNOME accent-color 枚举）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accent {
    Blue,
    Teal,
    Green,
    Yellow,
    Orange,
    Red,
    Purple,
    Pink,
    Slate,
    Custom(u8, u8, u8),
}

pub fn rgb(accent: Accent, dark: bool) -> (u8, u8, u8) {
    match (accent, dark) {
        (Accent::Blue, false) => (0x1c, 0x71, 0xd8),
        (Accent::Blue, true) => (0x35, 0x84, 0xe4),
        (Accent::Teal, false) => (0x0f, 0x94, 0x94),
        (Accent::Teal, true) => (0x21, 0x93, 0x93),
        (Accent::Green, false) => (0x26, 0xa2, 0x69),
        (Accent::Green, true) => (0x2e, 0xc2, 0x7e),
        (Accent::Yellow, false) => (0xb5, 0x8b, 0x00),
        (Accent::Yellow, true) => (0xd7, 0xa4, 0x1c),
        (Accent::Orange, false) => (0xc6, 0x46, 0x00),
        (Accent::Orange, true) => (0xe6, 0x61, 0x00),
        (Accent::Red, false) => (0xc0, 0x1c, 0x28),
        (Accent::Red, true) => (0xe0, 0x1b, 0x24),
        (Accent::Purple, false) => (0x61, 0x44, 0xc0),
        (Accent::Purple, true) => (0x81, 0x48, 0xe6),
        (Accent::Pink, false) => (0xa9, 0x2b, 0x70),
        (Accent::Pink, true) => (0xd2, 0x39, 0x94),
        (Accent::Slate, false) => (0x5d, 0x67, 0x70),
        (Accent::Slate, true) => (0x77, 0x82, 0x8c),
        (Accent::Custom(r, g, b), _) => (r, g, b),
    }
}

pub fn parse_accent(text: &str, custom_rgb: Option<(u8, u8, u8)>) -> Option<Accent> {
    let t = text.trim().trim_matches('\'');
    match t {
        "blue" => Some(Accent::Blue),
        "teal" => Some(Accent::Teal),
        "green" => Some(Accent::Green),
        "yellow" => Some(Accent::Yellow),
        "orange" => Some(Accent::Orange),
        "red" => Some(Accent::Red),
        "purple" => Some(Accent::Purple),
        "pink" => Some(Accent::Pink),
        "slate" => Some(Accent::Slate),
        _ => custom_rgb.map(|(r, g, b)| Accent::Custom(r, g, b)),
    }
}

pub fn parse_rgb(text: &str) -> Option<(u8, u8, u8)> {
    let inner = text.trim().strip_prefix("rgb(")?.strip_suffix(')')?;
    let it = inner.split(',');
    let get = |i: usize| -> Option<u8> {
        it.clone()
            .nth(i)?
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|v| *v <= 255)
            .map(|v| v as u8)
    };
    Some((get(0)?, get(1)?, get(2)?))
}

fn gsettings(key: &str) -> Option<String> {
    std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", key])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// 当前系统深浅色（color-scheme prefer-dark = 深；读不到按浅）
pub fn detect_dark() -> bool {
    gsettings("color-scheme").is_some_and(|s| s.contains("prefer-dark"))
}

/// 当前强调色（无该键/读取失败回退默认蓝）
pub fn detect_accent() -> Accent {
    let raw = gsettings("accent-color").unwrap_or_default();
    let custom = gsettings("accent-bg-color").and_then(|s| parse_rgb(&s));
    parse_accent(&raw, custom).unwrap_or(Accent::Blue)
}

fn hex(rgb: (u8, u8, u8)) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb.0, rgb.1, rgb.2)
}

/// 生成播放器完整样式表。视觉口径 = docs/电台native插件-草图.html 方向 C：
/// 168 圆角封面卡 + 绿点正在播 + 40/58 圆控制键 + 入口胶囊 + 列表卡片行。
/// 只用 GTK4 支持的属性（无 filter/transition；border 用长手写法）。
pub fn build_css(dark: bool, accent: Accent) -> String {
    let a = hex(rgb(accent, dark));
    let (bg, fg, dim, hover, surface, border, green) = if dark {
        (
            "#1e1e20",
            "#ffffff",
            "rgba(255,255,255,0.60)",
            "rgba(255,255,255,0.08)",
            "rgba(255,255,255,0.10)",
            "rgba(255,255,255,0.14)",
            "#35c26e",
        )
    } else {
        (
            "#fafafa",
            "#323232",
            "rgba(0,0,0,0.55)",
            "rgba(0,0,0,0.06)",
            "rgba(0,0,0,0.09)",
            "rgba(20,20,30,0.14)",
            "#1f9d55",
        )
    };
    format!(
        r#"
.rp-root {{ background-color: {bg}; color: {fg}; }}

/* —— 播放页 —— */
.rp-cover {{ border-radius: 20px; background-color: {surface}; }}
.rp-cover-fallback {{
  border-radius: 20px;
  font-size: 34px;
  font-weight: 600;
  color: rgba(255,255,255,0.94);
  background-image: linear-gradient(to bottom right, {a}, alpha({a}, 0.45));
}}
.rp-dot {{
  min-width: 7px;
  min-height: 7px;
  border-radius: 4px;
  background-color: {green};
}}
.rp-prog {{ font-size: 13px; font-weight: 600; color: {a}; }}
.rp-title {{ font-size: 19px; font-weight: 600; }}
.rp-meta {{ font-size: 12px; color: {dim}; }}

.rp-ctl {{
  min-width: 40px;
  min-height: 40px;
  padding: 0;
  border-radius: 20px;
  border-width: 1px;
  border-style: solid;
  border-color: {border};
  background-color: transparent;
  background-image: none;
  color: {fg};
}}
.rp-ctl:hover {{ background-color: {hover}; }}
/* MenuButton 的可见壳是内部 button 节点，类加在外层管不到（≡ 键返工） */
.rp-ctl > button {{
  padding: 0;
  border-radius: 20px;
  border-width: 1px;
  border-style: solid;
  border-color: {border};
  background-color: transparent;
  background-image: none;
  color: {fg};
}}
.rp-ctl > button:hover {{ background-color: {hover}; }}
.rp-play {{
  min-width: 58px;
  min-height: 58px;
  padding: 0;
  border-radius: 29px;
  border-width: 1px;
  border-style: solid;
  border-color: {a};
  background-color: {a};
  background-image: none;
  color: #ffffff;
}}
.rp-play:hover {{ background-color: alpha({a}, 0.85); }}
.rp-star-fav {{ color: {a}; }}
.rp-chip {{
  padding: 5px 12px;
  border-radius: 16px;
  border-width: 1px;
  border-style: solid;
  border-color: {border};
  background-color: transparent;
  background-image: none;
  color: {dim};
}}
.rp-chip:hover {{ background-color: {hover}; color: {fg}; }}
.rp-chip-t {{ font-size: 12px; }}
.rp-vol-ic {{ color: {dim}; }}
.rp-vol > trough > highlight {{ background-color: {a}; }}
.rp-vol > trough > slider {{
  min-width: 13px;
  min-height: 13px;
  border-radius: 7px;
}}

/* —— 列表页 —— */
.rp-headbar {{ background-color: {bg}; }}
.rp-entry {{
  padding: 8px 12px;
  border-radius: 10px;
  background-color: {surface};
  background-image: none;
  color: inherit;
}}
.rp-list {{ background-color: transparent; }}
.rp-list > row {{
  border-radius: 12px;
  margin: 2px 4px;
  padding: 8px 10px;
  background-color: transparent;
}}
.rp-list > row:hover {{ background-color: {hover}; }}
.rp-list > row:selected {{ background-color: alpha({a}, 0.14); }}
.rp-row-title {{ font-size: 14px; font-weight: 600; }}
.rp-row-sub {{ font-size: 12px; color: {dim}; }}
.rp-row-cover {{
  border-radius: 10px;
  background-color: {surface};
}}
.rp-row-fb {{
  border-radius: 10px;
  font-size: 18px;
  font-weight: 800;
  color: rgba(255,255,255,0.9);
  background-image: linear-gradient(to bottom right, {a}, alpha({a}, 0.45));
}}
.rp-star {{ padding: 4px 8px; border-radius: 14px; }}
.rp-star-fav-on {{ color: {a}; }}

/* —— 底部迷你条 —— */
.rp-mini {{
  padding: 8px 12px;
  border-top: 1px solid {hover};
}}
.rp-mini .rp-mini-title {{ font-size: 13px; font-weight: 600; }}

/* —— 顶栏按钮/下拉 —— */
.rp-back {{
  min-width: 36px;
  min-height: 36px;
  padding: 0;
  border-radius: 18px;
  background-image: none;
}}
.rp-more {{ min-width: 36px; min-height: 36px; padding: 0; border-radius: 18px; }}
.rp-cat {{ background-color: {surface}; border-radius: 10px; }}
"#,
        bg = bg,
        fg = fg,
        dim = dim,
        hover = hover,
        surface = surface,
        border = border,
        green = green,
        a = a,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_and_parsing() {
        assert_eq!(rgb(Accent::Orange, true), (0xe6, 0x61, 0x00));
        assert_ne!(rgb(Accent::Blue, true), rgb(Accent::Blue, false));
        assert_eq!(parse_accent("'orange'", None), Some(Accent::Orange));
        assert_eq!(parse_accent("garbage", None), None);
        assert_eq!(parse_rgb("rgb(230,97,0)"), Some((230, 97, 0)));
        assert_eq!(parse_rgb("rgb(300,0,0)"), None);
    }

    #[test]
    fn css_covers_key_selectors() {
        let css = build_css(true, Accent::Orange);
        for sel in [".rp-cover", ".rp-play", ".rp-list > row:selected", ".rp-mini", "#e66100"] {
            assert!(css.contains(sel), "缺选择器 {sel}");
        }
        assert!(!build_css(true, Accent::Blue).contains("#fafafa"));
        assert!(build_css(false, Accent::Blue).contains("#fafafa"));
    }
}
