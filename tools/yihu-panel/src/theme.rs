//! 面板主题：系统强调色 + 深浅色 → 动态生成样式表。
//!
//! 面板是裸 GTK4（不挂 libadwaita），拿不到 @accent_color 等 CSS
//! 变量，所以样式表由本模块在运行时生成：accent 从 gsettings 读
//! （GNOME 47+ 的 accent-color 枚举，custom 时读 accent-bg-color），
//! 卡片/文字底色按 libadwaita Yaru 口径的调色板写死两套。
//! 主题/强调色变化由 app.rs 监听 gsettings 后重建注入。

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
    /// 自定义：直接给 RGB
    Custom(u8, u8, u8),
}

/// GNOME accent-color 枚举 → RGB（dark 语境用亮阶，light 语境用暗阶）。
/// 取值口径：libadwaita 命名调色板（HIG Accent Colors），Yaru 实测近似。
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

/// 解析 `gsettings get org.gnome.desktop.interface accent-color` 的输出。
/// 枚举输出形如 `'orange'`；custom 时的实际颜色在 accent-bg-color 键
/// （`rgb(230,97,0)`），由调用方读出后经 [`parse_rgb`] 传入。
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

/// 解析 `rgb(230,97,0)`（gsettings color 输出，分量间空格可有可无）。
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

/// 读取当前系统强调色（无该键/读取失败回退默认蓝）
pub fn detect_accent() -> Accent {
    let get = |key: &str| -> Option<String> {
        std::process::Command::new("gsettings")
            .args(["get", "org.gnome.desktop.interface", key])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    let raw = get("accent-color").unwrap_or_default();
    let custom = get("accent-bg-color").and_then(|s| parse_rgb(&s));
    parse_accent(&raw, custom).unwrap_or(Accent::Blue)
}

/// hex 辅助（生成 CSS 用）
fn hex(rgb: (u8, u8, u8)) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb.0, rgb.1, rgb.2)
}

/// 生成面板完整样式表。dark + accent 决定全部配色；
/// 视觉语言：扁平浮层、系统强调色贯穿（搜索焦点/选中条/宫格 hover）、
/// 行尾徽章、底部键提示。cairo renderer 无阴影可用，层级靠底色差表达。
pub fn build_css(dark: bool, accent: Accent) -> String {
    let a = hex(rgb(accent, dark));
    // libadwaita Yaru 口径的窗口/浮层调色板
    let (bg, fg, dim, hover, surface) = if dark {
        (
            "#242428",   // 浮层底（比窗口底亮半档）
            "#ffffff",
            "rgba(255,255,255,0.62)",
            "rgba(255,255,255,0.08)",
            "rgba(255,255,255,0.10)",
        )
    } else {
        (
            "#fafafa",
            "#323232",
            "rgba(0,0,0,0.55)",
            "rgba(0,0,0,0.06)",
            "rgba(0,0,0,0.09)",
        )
    };
    format!(
        r#"
window.panel-root {{ background-color: transparent; }}

.panel-card {{
  border-radius: 16px;
  padding: 12px;
  background-color: {bg};
  color: {fg};
}}

/* —— 搜索框：主导航件，放大 —— */
.panel-search {{
  padding: 10px 14px;
  border-radius: 12px;
  font-size: 16px;
  background-color: {surface};
  color: inherit;
  box-shadow: none;
}}
.panel-search:focus-within {{
  background-color: alpha({a}, 0.12);
}}

/* —— 列表行 —— */
.panel-list {{ background-color: transparent; color: inherit; }}
.panel-list > row {{
  border-radius: 10px;
  margin: 1px 2px;
  padding: 7px 10px 7px 6px;
  background-color: transparent;
  color: inherit;
}}
.panel-list > row:hover {{ background-color: {hover}; }}
.panel-list > row:selected {{
  background-color: alpha({a}, 0.14);
}}
.panel-list > row .sel-bar {{
  min-width: 3px;
  border-radius: 2px;
  background-color: transparent;
}}
.panel-list > row:selected .sel-bar {{ background-color: {a}; }}
.panel-list > row .row-title {{ font-weight: 600; font-size: 14px; }}
.panel-list > row .caption-sm {{ font-size: 12px; opacity: 0.72; }}

/* —— 行尾类型徽章 —— */
.badge {{
  font-size: 10px;
  font-weight: 700;
  padding: 1px 7px;
  border-radius: 8px;
  background-color: {surface};
  color: {dim};
}}
.badge.plugin {{ background-color: alpha({a}, 0.16); color: {a}; }}
.badge.calc {{ background-color: alpha({a}, 0.16); color: {a}; }}

/* —— 底部键提示栏 —— */
.keybar {{
  padding: 6px 8px 2px;
  border-top: 1px solid {hover};
}}
.keybar .kbd {{
  font-size: 10px;
  font-weight: 700;
  padding: 1px 6px;
  border-radius: 5px;
  background-color: {surface};
  color: {dim};
}}
.keybar .kbd-hint {{
  font-size: 11px;
  color: {dim};
}}

/* —— 默认页：图标墙 + 能力宫格 —— */
.appwall {{ padding: 4px 2px 2px; }}
.appwall .tile {{
  border-radius: 12px;
  padding: 10px 6px 8px;
  background-color: transparent;
}}
.appwall .tile:hover {{ background-color: {hover}; }}
.appwall .tile .tile-name {{
  font-size: 11px;
  margin-top: 4px;
  color: {dim};
}}
.capgrid .tile {{
  border-radius: 12px;
  padding: 10px 14px;
  background-color: {surface};
}}
.capgrid .tile:hover {{ background-color: alpha({a}, 0.14); }}
.capgrid .tile .tile-name {{ font-size: 12px; }}

.section-label {{
  font-size: 12px;
  font-weight: 700;
  color: {dim};
  padding: 6px 4px 2px;
}}
"#,
        bg = bg,
        fg = fg,
        dim = dim,
        hover = hover,
        surface = surface,
        a = a,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accent_palette_matches_expected() {
        assert_eq!(rgb(Accent::Orange, true), (0xe6, 0x61, 0x00));
        assert_eq!(rgb(Accent::Orange, false), (0xc6, 0x46, 0x00));
        assert_eq!(rgb(Accent::Blue, true), (0x35, 0x84, 0xe4));
        // 深浅两套必须不同（可辨识的强调差异）
        assert_ne!(rgb(Accent::Blue, true), rgb(Accent::Blue, false));
    }

    #[test]
    fn parses_enum_and_fallbacks() {
        assert_eq!(parse_accent("'orange'", None), Some(Accent::Orange));
        assert_eq!(parse_accent("teal", None), Some(Accent::Teal));
        // custom 且读到颜色 → Custom；读不到 → None（调用方回退默认）
        assert_eq!(
            parse_accent("'custom'", Some((1, 2, 3))),
            Some(Accent::Custom(1, 2, 3))
        );
        assert_eq!(parse_accent("'custom'", None), None);
        assert_eq!(parse_accent("garbage", None), None);
    }

    #[test]
    fn parses_rgb_forms() {
        assert_eq!(parse_rgb("rgb(230,97,0)"), Some((230, 97, 0)));
        assert_eq!(parse_rgb("rgb(0, 255, 16)"), Some((0, 255, 16)));
        assert_eq!(parse_rgb("rgb(300,0,0)"), None);
        assert_eq!(parse_rgb("nope"), None);
    }

    #[test]
    fn css_covers_key_selectors_and_accent() {
        let css = build_css(true, Accent::Orange);
        for sel in [
            ".panel-card",
            ".panel-search:focus-within",
            ".panel-list > row:selected .sel-bar",
            ".badge.plugin",
            ".keybar .kbd",
            ".appwall .tile .tile-name",
            ".capgrid .tile",
            "#e66100", // accent 注入
        ] {
            assert!(css.contains(sel), "缺选择器/色值 {sel}");
        }
        // 深浅两套底色不同
        assert!(!build_css(true, Accent::Blue).contains("#fafafa"));
        assert!(build_css(false, Accent::Blue).contains("#fafafa"));
    }
}
