//! 呼出面板：配置持久化与 GNOME 自定义快捷键注册。
//!
//! Wayland 下第三方应用拿不到全局热键（协议层无 grab），走 GNOME
//! 「自定义快捷键」路径：把 `<安装目录>/yihu-panel toggle` **合并**写入
//! gsettings 的 custom-keybindings（绝不覆盖用户已有键）。
//! 列表解析失败时报错返回、不写入，避免破坏用户手工配置。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use crate::paths;
use serde::{Deserialize, Serialize};

pub const DEFAULT_HOTKEY: &str = "<Alt>space";
/// 本项目快捷键在 relocatable schema 下的固定路径
pub const KEYBINDING_PATH: &str =
    "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/customYihuPanel/";
pub const PANEL_NAME: &str = "一呼面板";

const SCHEMA: &str = "org.gnome.settings-daemon.plugins.media-keys";
const SCHEMA_KEY: &str = "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding";
const LIST_KEY: &str = "custom-keybindings";

#[derive(Debug, Clone)]
pub struct Config {
    pub hotkey: String,
    /// 摆放位置在「上部居中」基准上再上移的像素数
    pub place_offset_up: i32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            hotkey: DEFAULT_HOTKEY.into(),
            place_offset_up: 100,
        }
    }
}

impl Config {
    pub fn config_path() -> PathBuf {
        paths::config_file("panel.conf")
    }

    pub fn load() -> Config {
        let result = paths::read_compatible("panel.conf")
            .ok()
            .and_then(|text| Self::read_from_text(&text).ok())
            .unwrap_or_default();
        let _ = paths::migrate_one("panel.conf");
        result
    }

    fn read_from_text(text: &str) -> io::Result<Config> {
        let mut c = Config::default();
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            let Some((k, v)) = line.split_once('=') else { continue };
            match (k.trim(), v.trim()) {
                ("hotkey", v) if !v.is_empty() => c.hotkey = v.to_string(),
                ("place_offset_up", v) => if let Ok(n) = v.parse::<i32>() { c.place_offset_up = n },
                _ => {}
            }
        }
        Ok(c)
    }

    pub fn read_from(path: &Path) -> io::Result<Config> {
        let text = fs::read_to_string(path)?;
        let mut c = Config::default();
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            match (k.trim(), v.trim()) {
                ("hotkey", v) if !v.is_empty() => c.hotkey = v.to_string(),
                ("place_offset_up", v) => {
                    if let Ok(n) = v.parse::<i32>() {
                        c.place_offset_up = n;
                    }
                }
                _ => {}
            }
        }
        Ok(c)
    }

    pub fn to_text(&self) -> String {
        format!(
            "# 一呼面板配置\nhotkey = {}\nplace_offset_up = {}\n",
            self.hotkey, self.place_offset_up
        )
    }

    pub fn save(&self) -> io::Result<()> {
        paths::write_current("panel.conf", &self.to_text())
    }
}

/// 解析 `gsettings get … custom-keybindings` 的输出（GVariant 字符串数组文本，
/// 如 `['/a/', '/b/']`；空列表为 `[]` 或 `@as []`）。
/// 只接受带单引号的元素（路径场景足够），其他一律视为损坏返回 Err。
pub fn parse_keybinding_list(text: &str) -> Result<Vec<String>, String> {
    let t = text.trim();
    let inner = t.strip_prefix("@as").unwrap_or(t).trim();
    let Some(items_str) = inner.strip_prefix('[').and_then(|s| s.strip_suffix(']')) else {
        return Err(format!("无法解析快捷键列表：{text:?}"));
    };
    let body = items_str.trim();
    let mut items = Vec::new();
    if !body.is_empty() {
        for part in body.split(',') {
            let p = part.trim();
            let Some(s) = p.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) else {
                return Err(format!("快捷键列表元素格式异常：{p:?}（位于 {text:?}）"));
            };
            if s.is_empty() {
                return Err(format!("快捷键列表存在空元素：{text:?}"));
            }
            items.push(s.to_string());
        }
    }
    Ok(items)
}

/// 序列化为 gsettings set 可接受的 GVariant 文本。
pub fn format_keybinding_list(items: &[String]) -> String {
    format!(
        "[{}]",
        items.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(", ")
    )
}

/// 把 `ours` 合并进现有列表（已存在则保持原位），返回新列表文本。
pub fn merge_keybinding_list(existing: &str, ours: &str) -> Result<String, String> {
    let mut items = parse_keybinding_list(existing)?;
    if !items.iter().any(|s| s == ours) {
        items.push(ours.to_string());
    }
    Ok(format_keybinding_list(&items))
}

/// 从现有列表移除 `ours`（不存在则保持原样），返回新列表文本。
pub fn strip_keybinding_list(existing: &str, ours: &str) -> Result<String, String> {
    let items: Vec<String> = parse_keybinding_list(existing)?
        .into_iter()
        .filter(|s| s != ours)
        .collect();
    Ok(format_keybinding_list(&items))
}

// ---- gsettings CLI 封装（与 autodark::run_gsettings 同惯例）----

fn run_gsettings(args: &[&str]) -> io::Result<()> {
    let st = Command::new("gsettings").args(args).status()?;
    if st.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("gsettings {args:?} 失败：{st}")))
    }
}

fn read_gsettings(args: &[&str]) -> io::Result<String> {
    let out = Command::new("gsettings").args(args).output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "gsettings {args:?} 失败：{}",
            out.status
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// 读取当前 custom-keybindings 列表原文。
pub fn read_keybinding_list() -> io::Result<String> {
    read_gsettings(&["get", SCHEMA, LIST_KEY])
}

/// 合并注册快捷键：列表追加我们的路径，并写入 name/command/binding。
/// `command` 应为绝对路径 + 子命令（如 `<安装目录>/yihu-panel toggle`）。
///
/// 真机踩坑（2026-09-16）：注册成功但按下无反应，有两层原因——
/// ① GNOME 内置「激活窗口菜单」默认占用 `<Alt>space`，mutter 会先行拦截；
/// ② 清除内置占用后，gsd-media-keys 不会自动重试按键抓取，必须让它
///    重新扫描 custom-keybindings 列表。
/// 因此本函数：注册前自动解除内置占用（`gsettings reset` 可恢复），
/// 再以「摘除 → 写回」列表的方式强制 gsd 重新抓取。
pub fn register_hotkey(hotkey: &str, command: &str) -> io::Result<()> {
    let existing = read_keybinding_list()?;
    let list_with = merge_keybinding_list(&existing, KEYBINDING_PATH)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let list_without = strip_keybinding_list(&existing, KEYBINDING_PATH)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    clear_wm_conflict(hotkey)?;
    run_gsettings(&["set", SCHEMA, LIST_KEY, &list_without])?;
    let base = format!("{SCHEMA_KEY}:{KEYBINDING_PATH}");
    run_gsettings(&["set", &base, "name", PANEL_NAME])?;
    run_gsettings(&["set", &base, "command", command])?;
    run_gsettings(&["set", &base, "binding", hotkey])?;
    run_gsettings(&["set", SCHEMA, LIST_KEY, &list_with])
}

/// GNOME 内置键与请求的热键相同时，清空之（Windows 惯例的 Alt+Space 正撞此键）。
fn clear_wm_conflict(hotkey: &str) -> io::Result<()> {
    const WM_KEYBINDINGS: &str = "org.gnome.desktop.wm.keybindings";
    let cur = read_gsettings(&["get", WM_KEYBINDINGS, "activate-window-menu"])?;
    if cur.contains(hotkey) {
        run_gsettings(&["set", WM_KEYBINDINGS, "activate-window-menu", "[]"])?;
    }
    Ok(())
}

/// 移除注册：从列表摘除我们的路径并 reset 三个键（对未注册场景幂等）。
pub fn remove_hotkey() -> io::Result<()> {
    let existing = read_keybinding_list()?;
    let merged = strip_keybinding_list(&existing, KEYBINDING_PATH)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    run_gsettings(&["set", SCHEMA, LIST_KEY, &merged])?;
    let base = format!("{SCHEMA_KEY}:{KEYBINDING_PATH}");
    run_gsettings(&["reset", &base, "name"])?;
    run_gsettings(&["reset", &base, "command"])?;
    run_gsettings(&["reset", &base, "binding"])
}

/// 读取我们路径下已注册的快捷键（未注册或读取失败返回 None）。
pub fn registered_hotkey() -> Option<String> {
    let base = format!("{SCHEMA_KEY}:{KEYBINDING_PATH}");
    let v = read_gsettings(&["get", &base, "binding"]).ok()?;
    let s = v.trim().trim_matches('\'');
    (!s.is_empty() && s != "@ss ''").then(|| s.to_string())
}

// ---- 使用历史（默认集「最近使用」的数据源）----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: String,
    pub count: u32,
    /// 最近一次使用的 Unix 秒
    pub last: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct History {
    #[serde(default)]
    pub entries: Vec<HistoryEntry>,
}

impl History {
    pub fn path() -> PathBuf {
        paths::config_file("panel_history.json")
    }

    pub fn load() -> History {
        let result = paths::read_compatible("panel_history.json")
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let _ = paths::migrate_one("panel_history.json");
        result
    }

    pub fn save(&self) -> io::Result<()> {
        paths::write_current(
            "panel_history.json",
            &serde_json::to_string(self).unwrap_or_default(),
        )
    }

    /// 记一次使用（存在则累加并刷新时间，不存在则新增）。
    pub fn bump(&mut self, id: &str) {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        match self.entries.iter_mut().find(|e| e.id == id) {
            Some(e) => {
                e.count += 1;
                e.last = now;
            }
            None => self.entries.push(HistoryEntry {
                id: id.to_string(),
                count: 1,
                last: now,
            }),
        }
    }

    pub fn count_of(&self, id: &str) -> u32 {
        self.entries
            .iter()
            .find(|e| e.id == id)
            .map(|e| e.count)
            .unwrap_or(0)
    }

    pub fn last_of(&self, id: &str) -> u64 {
        self.entries
            .iter()
            .find(|e| e.id == id)
            .map(|e| e.last)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OURS: &str = KEYBINDING_PATH;

    #[test]
    fn merge_into_empty_variants() {
        assert_eq!(
            merge_keybinding_list("[]", OURS).unwrap(),
            format!("['{OURS}']")
        );
        // gsettings 对空 relocatable 数组可能返回类型标注形式
        assert_eq!(
            merge_keybinding_list("@as []", OURS).unwrap(),
            format!("['{OURS}']")
        );
    }

    #[test]
    fn merge_appends_and_dedups() {
        let existing = "['/org/a/custom0/', '/org/b/custom1/']";
        let merged = merge_keybinding_list(existing, OURS).unwrap();
        assert_eq!(
            merged,
            format!("['/org/a/custom0/', '/org/b/custom1/', '{OURS}']")
        );
        // 重复合并不产生重复项
        assert_eq!(merge_keybinding_list(&merged, OURS).unwrap(), merged);
    }

    #[test]
    fn merge_rejects_garbage() {
        assert!(merge_keybinding_list("not-a-list", OURS).is_err());
        assert!(merge_keybinding_list("['/a/', bare]", OURS).is_err());
        assert!(merge_keybinding_list("['/a/', '/a/", OURS).is_err());
    }

    #[test]
    fn strip_removes_only_ours() {
        let existing = format!("['/org/a/custom0/', '{OURS}']");
        assert_eq!(strip_keybinding_list(&existing, OURS).unwrap(), "['/org/a/custom0/']");
        // 不存在时原样保留（空元素也要能表达）
        assert_eq!(
            strip_keybinding_list("['/org/a/custom0/']", OURS).unwrap(),
            "['/org/a/custom0/']"
        );
        assert_eq!(strip_keybinding_list("[]", OURS).unwrap(), "[]");
    }

    #[test]
    fn roundtrip_merge_then_strip() {
        let existing = "['/org/a/custom0/', '/org/b/custom1/']";
        let merged = merge_keybinding_list(existing, OURS).unwrap();
        assert_eq!(strip_keybinding_list(&merged, OURS).unwrap(), existing);
    }

    #[test]
    fn config_roundtrip() {
        let dir = std::env::temp_dir().join(format!("yihu-panel-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("panel.conf");
        fs::write(&p, "hotkey = <Alt>z\nplace_offset_up = 80\n").unwrap();
        let c = Config::read_from(&p).unwrap();
        assert_eq!(c.hotkey, "<Alt>z");
        assert_eq!(c.place_offset_up, 80);
        // 空文件/损坏行回退默认值
        fs::write(&p, "# 注释\nbadline\n").unwrap();
        let c = Config::read_from(&p).unwrap();
        assert_eq!(c.hotkey, DEFAULT_HOTKEY);
        assert_eq!(c.place_offset_up, 100);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn history_bump_and_roundtrip() {
        let mut h = History::default();
        h.bump("app:a.desktop");
        h.bump("app:a.desktop");
        h.bump("cap:center");
        assert_eq!(h.count_of("app:a.desktop"), 2);
        assert_eq!(h.count_of("cap:center"), 1);
        assert_eq!(h.count_of("missing"), 0);
        // 序列化 roundtrip（load/save 只是文件封装，纯格式在此验证）
        let text = serde_json::to_string(&h).unwrap();
        let loaded: History = serde_json::from_str(&text).unwrap();
        assert_eq!(loaded.count_of("app:a.desktop"), 2);
        assert_eq!(loaded.count_of("cap:center"), 1);
        // 损坏文本回退空历史（load 的 unwrap_or_default 行为）
        let bad: Result<History, _> = serde_json::from_str("not json");
        assert!(bad.is_err());
        assert!(History::default().entries.is_empty());
    }
}
