//! 自动深浅色主题切换：配置解析、切换判定与 gsettings 应用。
//!
//! 切换动作由无 UI 的 `autodark-agent`（systemd 用户定时器驱动）
//! 通过 `gsettings` 完成；GUI 工具只负责读写配置与查看状态。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::paths;
use crate::sun;

/// 切换模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// 自定义时间
    Custom,
    /// 日出至日落（按地理坐标计算）
    Sun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    Light,
    Dark,
}

impl Theme {
    pub fn name(self) -> &'static str {
        match self {
            Theme::Light => "浅色",
            Theme::Dark => "深色",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub enabled: bool,
    pub mode: Mode,
    /// 深色时段开始时刻（时, 分）
    pub dark_start: (u32, u32),
    /// 深色时段结束时刻；start > end 表示时段跨零点，时段之外一律浅色
    pub dark_end: (u32, u32),
    pub latitude: f64,
    pub longitude: f64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: false,
            mode: Mode::Custom,
            dark_start: (20, 0),
            dark_end: (7, 0),
            latitude: 30.66,
            longitude: 104.06,
        }
    }
}

impl Config {
    pub fn config_path() -> PathBuf {
        paths::config_file("autodark.conf")
    }

    pub fn load() -> Config {
        // 新路径优先；迁移期间从旧命名空间回退，并复制一份到新路径。
        match paths::read_compatible("autodark.conf") {
            Ok(text) => {
                let cfg = Self::read_text(&text).unwrap_or_default();
                let _ = paths::migrate_one("autodark.conf");
                cfg
            }
            Err(_) => Config::default(),
        }
    }

    fn read_text(text: &str) -> io::Result<Config> {
        let mut c = Config::default();
        let mut seen_start = false;
        let mut seen_end = false;
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "enabled" => c.enabled = v == "true",
                "mode" => c.mode = if v == "sun" { Mode::Sun } else { Mode::Custom },
                "dark_start" => if let Some(t) = parse_hm(v) { c.dark_start = t; seen_start = true },
                "dark_end" => if let Some(t) = parse_hm(v) { c.dark_end = t; seen_end = true },
                // 旧版键（light_time/dark_time 两个独立切换点）映射为深色时段
                "dark_time" => if !seen_start { if let Some(t) = parse_hm(v) { c.dark_start = t } },
                "light_time" => if !seen_end { if let Some(t) = parse_hm(v) { c.dark_end = t } },
                "latitude" => if let Ok(f) = v.parse::<f64>() { c.latitude = f.clamp(-89.0, 89.0) },
                "longitude" => if let Ok(f) = v.parse::<f64>() { c.longitude = f.clamp(-180.0, 180.0) },
                _ => {}
            }
        }
        Ok(c)
    }

    pub fn read_from(path: &Path) -> io::Result<Config> {
        Self::read_text(&fs::read_to_string(path)?)
    }

    pub fn to_text(&self) -> String {
        format!(
            "# 一呼 AutoDark 配置（深色时段之外自动使用浅色；结束 < 开始表示跨零点）\n\
             enabled = {}\n\
             mode = {}\n\
             dark_start = {:02}:{:02}\n\
             dark_end = {:02}:{:02}\n\
             latitude = {:.4}\n\
             longitude = {:.4}\n",
            self.enabled,
            match self.mode {
                Mode::Custom => "custom",
                Mode::Sun => "sun",
            },
            self.dark_start.0,
            self.dark_start.1,
            self.dark_end.0,
            self.dark_end.1,
            self.latitude,
            self.longitude
        )
    }

    pub fn save(&self) -> io::Result<()> {
        paths::write_current("autodark.conf", &self.to_text())
    }
}

fn parse_hm(v: &str) -> Option<(u32, u32)> {
    let (h, m) = v.split_once(':')?;
    Some((h.trim().parse().ok()?, m.trim().parse().ok()?))
}

fn hm_f(t: (u32, u32)) -> f64 {
    t.0 as f64 + t.1 as f64 / 60.0
}

/// 本地墙钟时间（仅主题调度所需字段）。
#[derive(Debug, Clone, Copy)]
pub struct LocalTime {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub utc_offset_hours: f64,
}

impl LocalTime {
    pub fn now() -> io::Result<LocalTime> {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?
            .as_secs() as i64;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        // SAFETY: libc::localtime_r 将解析结果写入调用方提供的缓冲。
        if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(LocalTime {
            year: tm.tm_year + 1900,
            month: (tm.tm_mon + 1) as u32,
            day: tm.tm_mday as u32,
            hour: tm.tm_hour as u32,
            minute: tm.tm_min as u32,
            second: tm.tm_sec as u32,
            utc_offset_hours: tm.tm_gmtoff as f64 / 3600.0,
        })
    }

    /// 当天小时数（含小数）。
    pub fn hours(&self) -> f64 {
        self.hour as f64 + self.minute as f64 / 60.0 + self.second as f64 / 3600.0
    }
}

/// 该配置下当前时刻应当使用的主题。
pub fn desired_theme(cfg: &Config, now: &LocalTime) -> Theme {
    match cfg.mode {
        Mode::Custom => custom_theme(cfg, now.hours()),
        Mode::Sun => {
            match sun::sun_times(now.year, now.month, now.day, cfg.latitude, cfg.longitude, now.utc_offset_hours)
            {
                Some((rise, set)) => {
                    let h = now.hours();
                    if h < rise || h >= set {
                        Theme::Dark
                    } else {
                        Theme::Light
                    }
                }
                // 极昼/极夜兜底：按浅色处理
                None => Theme::Light,
            }
        }
    }
}

/// 深色时段 [start, end) 内为深色，其余一律浅色；start == end 视为无深色时段。
fn custom_theme(cfg: &Config, now_h: f64) -> Theme {
    let (s, e) = (hm_f(cfg.dark_start), hm_f(cfg.dark_end));
    if s == e {
        return Theme::Light;
    }
    let in_dark = if s < e {
        (s..e).contains(&now_h)
    } else {
        // 跨零点：如 17:00 → 次日 07:00
        now_h >= s || now_h < e
    };
    if in_dark { Theme::Dark } else { Theme::Light }
}

/// 下一次主题切换：(当天小时数, 切换后的主题)。
/// 今天已无切换时返回明天首次切换（小时数 > 24）。
pub fn next_transition(cfg: &Config, now: &LocalTime) -> Option<(f64, Theme)> {
    let events = |y: i32, m: u32, d: u32| -> Vec<(f64, Theme)> {
        match cfg.mode {
            Mode::Custom => vec![(hm_f(cfg.dark_start), Theme::Dark), (hm_f(cfg.dark_end), Theme::Light)],
            Mode::Sun => sun::sun_times(y, m, d, cfg.latitude, cfg.longitude, now.utc_offset_hours)
                .map(|(r, s)| vec![(r, Theme::Light), (s, Theme::Dark)])
                .unwrap_or_default(),
        }
    };

    let h = now.hours();
    if let Some((t, th)) = events(now.year, now.month, now.day)
        .into_iter()
        .filter(|(t, _)| *t > h)
        .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap())
    {
        return Some((t, th));
    }
    let (y2, m2, d2) = {
        let days = sun::days_from_civil(now.year as i64, now.month, now.day) + 1;
        let (y, m, d) = sun::civil_from_days(days);
        (y as i32, m, d)
    };
    let mut tomorrow = events(y2, m2, d2);
    tomorrow.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    tomorrow.first().map(|&(t, th)| (t + 24.0, th))
}

// ---- gsettings 应用 ----

/// 读取当前 GNOME 深浅色模式。
pub fn current_scheme() -> io::Result<Theme> {
    let out = Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "color-scheme"])
        .output()?;
    let s = String::from_utf8_lossy(&out.stdout);
    Ok(if s.contains("dark") { Theme::Dark } else { Theme::Light })
}

/// 设置 GNOME 深浅色模式（同时切换 Yaru GTK3 主题，兼顾旧式应用）。
pub fn set_scheme(t: Theme) -> io::Result<()> {
    let (scheme, gtk3) = match t {
        Theme::Dark => ("prefer-dark", "Yaru-dark"),
        Theme::Light => ("default", "Yaru"),
    };
    run_gsettings(&["set", "org.gnome.desktop.interface", "color-scheme", scheme])?;
    run_gsettings(&["set", "org.gnome.desktop.interface", "gtk-theme", gtk3])
}

/// 立即按配置应用主题，返回最终主题。
pub fn apply(cfg: &Config) -> io::Result<Theme> {
    let now = LocalTime::now()?;
    let desired = desired_theme(cfg, &now);
    if current_scheme()? != desired {
        set_scheme(desired)?;
    }
    Ok(desired)
}

fn run_gsettings(args: &[&str]) -> io::Result<()> {
    let st = Command::new("gsettings").args(args).status()?;
    if st.success() {
        Ok(())
    } else {
        Err(io::Error::new(io::ErrorKind::Other, format!("gsettings {args:?} 执行失败")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tm(hour: f64) -> LocalTime {
        let h = hour.floor() as u32;
        let m = ((hour - h as f64) * 60.0).round() as u32;
        LocalTime { year: 2026, month: 9, day: 14, hour: h, minute: m, second: 0, utc_offset_hours: 8.0 }
    }

    #[test]
    fn custom_mode_basic_and_wrap() {
        // 深色时段 20:00 → 次日 07:00（跨零点）
        let c = Config { dark_start: (20, 0), dark_end: (7, 0), ..Config::default() };
        assert_eq!(custom_theme(&c, 6.99), Theme::Dark);
        assert_eq!(custom_theme(&c, 7.0), Theme::Light);
        assert_eq!(custom_theme(&c, 12.0), Theme::Light);
        assert_eq!(custom_theme(&c, 20.0), Theme::Dark);
        // 深色时段 06:00 → 22:00（当天内）
        let c2 = Config { dark_start: (6, 0), dark_end: (22, 0), ..Config::default() };
        assert_eq!(custom_theme(&c2, 1.0), Theme::Light);
        assert_eq!(custom_theme(&c2, 12.0), Theme::Dark);
        assert_eq!(custom_theme(&c2, 23.0), Theme::Light);
        // 起止相同：视为无深色时段，恒为浅色
        let c3 = Config { dark_start: (8, 0), dark_end: (8, 0), ..Config::default() };
        assert_eq!(custom_theme(&c3, 8.0), Theme::Light);
        assert_eq!(custom_theme(&c3, 23.0), Theme::Light);
    }

    #[test]
    fn legacy_config_maps_to_dark_window() {
        // 旧版两个切换点（dark 20:05 进入深色 / light 07:30 进入浅色）
        // ⇒ 深色时段 20:05 → 07:30
        let legacy = "enabled = true\nmode = custom\nlight_time = 07:30\ndark_time = 20:05\n";
        let c = Config::read_text(legacy).unwrap();
        assert_eq!(c.dark_start, (20, 5));
        assert_eq!(c.dark_end, (7, 30));
        // 新旧键同时出现时以新键为准
        let mixed = "dark_start = 21:00\ndark_time = 20:05\nlight_time = 06:00\n";
        let c2 = Config::read_text(mixed).unwrap();
        assert_eq!(c2.dark_start, (21, 0));
        assert_eq!(c2.dark_end, (6, 0));
    }

    #[test]
    fn config_roundtrip() {
        let c = Config {
            enabled: true,
            mode: Mode::Sun,
            dark_start: (20, 5),
            dark_end: (7, 30),
            latitude: 39.9042,
            longitude: 116.4074,
        };
        let text = c.to_text();
        let dir = std::env::temp_dir().join(format!("mt-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("autodark.conf");
        fs::write(&p, &text).unwrap();
        let c2 = Config::read_from(&p).unwrap();
        assert_eq!(c2.enabled, c.enabled);
        assert_eq!(c2.mode, Mode::Sun);
        assert_eq!(c2.dark_start, (20, 5));
        assert_eq!(c2.dark_end, (7, 30));
        assert!((c2.latitude - c.latitude).abs() < 1e-6);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn next_transition_orders_events() {
        // 深色时段 20:00 → 次日 07:00
        let c = Config { dark_start: (20, 0), dark_end: (7, 0), ..Config::default() };
        // 12:00 → 下一次是 20:00 切深色
        let (t, th) = next_transition(&c, &tm(12.0)).unwrap();
        assert!((t - 20.0).abs() < 1e-6 && th == Theme::Dark);
        // 23:00 → 明天 07:00 切浅色（>24 表示跨天）
        let (t, th) = next_transition(&c, &tm(23.0)).unwrap();
        assert!((t - 31.0).abs() < 1e-6 && th == Theme::Light);
    }
}
