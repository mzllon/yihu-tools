//! 插件基座：manifest 解析、注册表路径、启停状态。
//!
//! 插件 = 独立进程，宿主经 stdio 行式 JSON 通信（协议 v0，主版本 1：
//! init / query / results / activate，只加不改）。manifest 用 TOML；
//! `api` 用 semver 主版本 range（"^1"），宿主承诺 v1 内向后兼容，
//! 不做 GNOME 式枚举版本锁定。

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// 宿主支持的插件协议主版本
pub const PROTOCOL_MAJOR: u32 = 1;

/// 插件触发声明（v0：keywords / regex 二选一，仅元数据，查询全量广播）
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Trigger {
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub regex: String,
    #[serde(default)]
    pub label: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
    /// 插件协议版本 range，如 "^1"
    pub api: String,
    /// 相对插件目录的可执行入口
    pub entry: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub triggers: Vec<Trigger>,
    #[serde(default)]
    pub permissions: Vec<String>,
}

/// 解析并校验 manifest。校验项：id/name/entry 非空、id 字符集、
/// api range 与宿主协议主版本匹配。
pub fn parse_manifest(text: &str) -> Result<Manifest, String> {
    let m: Manifest = toml::from_str(text).map_err(|e| format!("manifest 解析失败：{e}"))?;
    if m.id.is_empty() || !m.id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err(format!("id 非法（须为小写字母/数字/-）：{:?}", m.id));
    }
    if m.name.is_empty() {
        return Err("name 不能为空".into());
    }
    if m.entry.is_empty() {
        return Err("entry 不能为空".into());
    }
    if !api_matches(&m.api, PROTOCOL_MAJOR) {
        return Err(format!(
            "插件协议版本 {} 与宿主 v{PROTOCOL_MAJOR} 不匹配",
            m.api
        ));
    }
    Ok(m)
}

/// 判断 api range（"^1" / "=1" / "1" / ">=1"）是否匹配宿主协议主版本。
/// 只认主版本精确匹配；解析失败一律 false。
pub fn api_matches(range: &str, host_major: u32) -> bool {
    let r = range.trim();
    let major = r
        .strip_prefix('^')
        .or_else(|| r.strip_prefix('='))
        .or_else(|| r.strip_prefix(">="))
        .unwrap_or(r);
    major.parse::<u32>() == Ok(host_major)
}

// ---- 注册表路径 ----

fn data_home() -> PathBuf {
    std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|v| v.starts_with('/'))
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
                .join(".local/share")
        })
}

fn config_home() -> PathBuf {
    std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|v| v.starts_with('/'))
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into())).join(".config")
        })
}

/// 已安装插件的注册表根目录
pub fn plugins_dir() -> PathBuf {
    data_home().join("yihu/plugins")
}

/// 插件启停状态文件
pub fn state_path() -> PathBuf {
    config_home().join("minitools/plugins_state.json")
}

/// 已安装插件（manifest + 所在目录）
#[derive(Debug, Clone)]
pub struct Installed {
    pub manifest: Manifest,
    pub dir: PathBuf,
}

impl Installed {
    /// 入口可执行的绝对路径
    pub fn entry_path(&self) -> PathBuf {
        self.dir.join(&self.manifest.entry)
    }
}

/// 扫描注册表：解析成功的插件返回；损坏目录跳过并返回错误列表。
pub fn list_installed_in(base: &Path) -> (Vec<Installed>, Vec<String>) {
    let mut ok = Vec::new();
    let mut errors = Vec::new();
    let Ok(entries) = fs::read_dir(base) else {
        return (ok, errors);
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let manifest_path = dir.join("manifest.toml");
        let Ok(text) = fs::read_to_string(&manifest_path) else {
            continue; // 无 manifest 的目录（如残留空目录）忽略
        };
        match parse_manifest(&text) {
            Ok(m) => {
                if m.entry.contains('/') || m.entry.contains('\\') || m.entry.starts_with('.') {
                    errors.push(format!("{}: entry 非法", m.id));
                    continue;
                }
                ok.push(Installed { manifest: m, dir });
            }
            Err(e) => errors.push(format!("{}: {e}", dir.display())),
        }
    }
    ok.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
    (ok, errors)
}

pub fn list_installed() -> (Vec<Installed>, Vec<String>) {
    list_installed_in(&plugins_dir())
}

/// 从本地目录安装插件：校验 manifest → 递归复制到注册表 → 入口赋执行位。
/// 返回解析后的 manifest。已存在同名插件则覆盖（升级语义）。
pub fn install_from_dir(src: &Path) -> io::Result<Manifest> {
    let text = fs::read_to_string(src.join("manifest.toml"))
        .map_err(|e| io::Error::other(format!("读取 manifest.toml 失败：{e}")))?;
    let manifest = parse_manifest(&text).map_err(io::Error::other)?;
    let entry_path = src.join(&manifest.entry);
    if !entry_path.is_file() {
        return Err(io::Error::other(format!("入口不存在：{}", entry_path.display())));
    }
    let dest = plugins_dir().join(&manifest.id);
    if dest.exists() {
        fs::remove_dir_all(&dest)?;
    }
    copy_dir(src, &dest)?;
    // 入口可执行，其余文件 0644
    let _ = fs::set_permissions(dest.join(&manifest.entry), fs::Permissions::from_mode(0o755));
    for f in walk_files(&dest) {
        let _ = fs::set_permissions(&f, fs::Permissions::from_mode(0o644));
    }
    let _ = fs::set_permissions(dest.join(&manifest.entry), fs::Permissions::from_mode(0o755));
    Ok(manifest)
}

pub fn remove_plugin(id: &str) -> io::Result<()> {
    let dest = plugins_dir().join(id);
    if !dest.is_dir() {
        return Err(io::Error::other(format!("插件 {id} 未安装")));
    }
    fs::remove_dir_all(dest)
}

fn walk_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out
}

fn copy_dir(src: &Path, dest: &Path) -> io::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let p = entry.path();
        if p.is_dir() {
            copy_dir(&p, &dest.join(entry.file_name()))?;
        } else {
            fs::copy(&p, dest.join(entry.file_name()))?;
        }
    }
    Ok(())
}

// ---- 启停状态 ----

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginsState {
    #[serde(default)]
    pub disabled: Vec<String>,
}

impl PluginsState {
    pub fn load() -> PluginsState {
        Self::read_from(&state_path()).unwrap_or_default()
    }

    pub fn read_from(path: &Path) -> io::Result<PluginsState> {
        let text = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(path, serde_json::to_string(self).unwrap_or_default())
    }

    pub fn is_disabled(&self, id: &str) -> bool {
        self.disabled.iter().any(|s| s == id)
    }

    pub fn set_disabled(&mut self, id: &str, on: bool) {
        if on && !self.is_disabled(id) {
            self.disabled.push(id.to_string());
        }
        if !on {
            self.disabled.retain(|s| s != id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"
id = "ts-convert"
name = "时间戳转换"
version = "0.1.0"
api = "^1"
entry = "ts-convert"
icon = "x-office-document"
permissions = []

[[triggers]]
keywords = ["ts", "时间戳"]

[[triggers]]
regex = "^\\d{10}$"
label = "纯数字"
"#;

    #[test]
    fn manifest_parse_full() {
        let m = parse_manifest(FULL).unwrap();
        assert_eq!(m.id, "ts-convert");
        assert_eq!(m.triggers.len(), 2);
        assert_eq!(m.triggers[0].keywords, vec!["ts", "时间戳"]);
        assert_eq!(m.triggers[1].regex, "^\\d{10}$");
        assert!(m.permissions.is_empty());
    }

    #[test]
    fn manifest_rejects_bad() {
        let missing_id = r#"name = "x"
api = "^1"
entry = "x"
"#;
        assert!(parse_manifest(missing_id).is_err());
        let bad_api = r#"id = "a"
name = "x"
api = "^2"
entry = "x"
"#;
        assert!(parse_manifest(bad_api).is_err());
        let no_entry = r#"id = "a"
name = "x"
api = "^1"
"#;
        assert!(parse_manifest(no_entry).is_err());
        let bad_id = r#"id = "Bad_Id"
name = "x"
api = "^1"
entry = "x"
"#;
        assert!(parse_manifest(bad_id).is_err());
    }

    #[test]
    fn api_range_matching() {
        assert!(api_matches("^1", 1));
        assert!(api_matches("1", 1));
        assert!(api_matches("=1", 1));
        assert!(api_matches(">=1", 1));
        assert!(!api_matches("^1", 2));
        assert!(!api_matches("^2", 1));
        assert!(!api_matches("garbage", 1));
    }

    #[test]
    fn state_roundtrip() {
        let mut st = PluginsState::default();
        st.set_disabled("a", true);
        st.set_disabled("a", true); // 重复不开加
        st.set_disabled("b", true);
        st.set_disabled("b", false);
        assert!(st.is_disabled("a"));
        assert!(!st.is_disabled("b"));
        let text = serde_json::to_string(&st).unwrap();
        let back: PluginsState = serde_json::from_str(&text).unwrap();
        assert!(back.is_disabled("a") && !back.is_disabled("b"));
    }
}
