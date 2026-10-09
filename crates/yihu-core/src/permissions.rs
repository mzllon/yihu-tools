//! 宿主能力词表（M4 权限声明 v1 → M5 开放 API v2：能力@参数声明制）。
//!
//! manifest 顶层 `permissions` 列表声明插件需要的宿主能力；中心「插件」页
//! 安装时明示（含参数），面板能力代理按 manifest 逐请求强制。
//!
//! v2 语法（Chrome/Firefox 声明制 + Raycast API 抽象的路线，2026-10-10 拍板）：
//! - 无参能力：`clipboard.write`、`open_uri`、`launch_app`、`notify`、
//!   `selected_files.read`、`screenshot.take`——带 `@` 参数即非法；
//! - `network.fetch@<host>`：允许经宿主向该 host 发请求（可多条，host 精确
//!   匹配，可带端口）；
//! - `settings.write@<schema>`：允许写该 gsettings schema 下的键（多条）；
//! - `fs.write@<glob>`：允许写匹配的路径（`~` 展开 HOME；`*` 通配、`**` 跨层），
//!   多条。
//!
//! 未知名/非法参数 = manifest 解析失败（拒装）。管道即凭证：请求只能来自
//! 宿主拉起的会话 stdio。沙箱白名单本身不变（插件进程依然无网无 FS）——
//! 所有系统访问都经宿主代执行、逐次校验、审计留痕。

/// 经宿主写剪贴板（Wayland 下插件进程自写不可靠，宿主托管）
pub const CLIPBOARD_WRITE: &str = "clipboard.write";
/// 经宿主打开链接（scheme 白名单：http/https/mailto）
pub const OPEN_URI: &str = "open_uri";
/// 经宿主启动已安装应用（仅限宿主应用列表中的 desktop id）
pub const LAUNCH_APP: &str = "launch_app";
/// 经宿主发桌面通知
pub const NOTIFY: &str = "notify";
/// query 携带文件管理器选中项上下文（`context.files`）
pub const SELECTED_FILES_READ: &str = "selected_files.read";
/// 经宿主截屏（xdg-desktop-portal；mode = full/area，可选拷贝到剪贴板）
pub const SCREENSHOT_TAKE: &str = "screenshot.take";
/// 经宿主发 HTTP 请求（声明 host 白名单）
pub const NETWORK_FETCH: &str = "network.fetch";
/// 经宿主写 gsettings（声明 schema 白名单，仅字符串值）
pub const SETTINGS_WRITE: &str = "settings.write";
/// 经宿主写文件（声明路径 glob，UTF-8 文本，原子写）
pub const FS_WRITE: &str = "fs.write";

/// v2 全部合法能力名（无参能力 + 带参能力的基名）
pub const KNOWN: &[&str] = &[
    CLIPBOARD_WRITE,
    OPEN_URI,
    LAUNCH_APP,
    NOTIFY,
    SELECTED_FILES_READ,
    SCREENSHOT_TAKE,
    NETWORK_FETCH,
    SETTINGS_WRITE,
    FS_WRITE,
];

/// 允许带 @参数的能力基名
pub const PARAMETRIC: &[&str] = &[NETWORK_FETCH, SETTINGS_WRITE, FS_WRITE];

/// 一条声明：基名 + 参数（无参能力 param 为 None）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    pub capability: String,
    pub param: Option<String>,
}

/// 解析一条声明字符串（`cap` 或 `cap@param`），校验词表与参数合法性。
pub fn parse_declaration(raw: &str) -> Result<Declaration, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("空权限声明".into());
    }
    let (cap, param) = match raw.split_once('@') {
        Some((c, p)) => (c, Some(p)),
        None => (raw, None),
    };
    if !KNOWN.contains(&cap) {
        return Err(format!(
            "未知权限 {cap:?}（合法：{}）",
            KNOWN.join("、")
        ));
    }
    let param = match (PARAMETRIC.contains(&cap), param) {
        (false, None) => None,
        (false, Some(p)) => return Err(format!("{cap} 不接受参数（得到 @{p}）")),
        (true, None) => {
            return Err(format!("{cap} 必须带 @参数（如 {cap}@example.com）"));
        }
        (true, Some(p)) => Some(p.to_string()),
    };
    if let Some(p) = &param {
        validate_param(cap, p)?;
    }
    Ok(Declaration {
        capability: cap.to_string(),
        param,
    })
}

fn validate_param(cap: &str, p: &str) -> Result<(), String> {
    if p.is_empty() || p.len() > 256 {
        return Err(format!("{cap} 参数长度非法"));
    }
    match cap {
        NETWORK_FETCH => {
            // host[:port]；仅字母数字.-_ 与可选端口
            if p.starts_with('.') || p.ends_with('.') || p.contains("..") {
                return Err(format!("host 非法：{p}"));
            }
            let host = p.split(':').next().unwrap_or(p);
            if !host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
                || host.is_empty()
            {
                return Err(format!("host 非法：{p}"));
            }
            if let Some(port) = p.split_once(':').map(|(_, x)| x) {
                if port.parse::<u16>().is_err() {
                    return Err(format!("端口非法：{p}"));
                }
            }
            Ok(())
        }
        SETTINGS_WRITE => {
            // gsettings schema：点分段，仅字母数字与点
            if p.starts_with('.')
                || p.ends_with('.')
                || p.contains("..")
                || !p.chars().all(|c| c.is_ascii_alphanumeric() || c == '.')
            {
                return Err(format!("schema 非法：{p}"));
            }
            Ok(())
        }
        FS_WRITE => {
            // ~ 开头（将展开为 HOME）或绝对路径；其余交给 glob 匹配
            if p.starts_with('~') || p.starts_with('/') {
                Ok(())
            } else {
                Err(format!("路径必须以 ~ 或 / 开头：{p}"))
            }
        }
        _ => Ok(()),
    }
}

/// 校验整张 manifest 的 permissions 列表：逐条解析、去重保序。
pub fn validate(list: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::with_capacity(list.len());
    for raw in list {
        parse_declaration(raw)?;
        if !out.iter().any(|x| x == raw) {
            out.push(raw.clone());
        }
    }
    Ok(out)
}

/// 某能力声明的全部参数（仅带参能力有值；未声明/无参能力 = 空列表）
pub fn params_of(declared: &[String], capability: &str) -> Vec<String> {
    declared
        .iter()
        .filter_map(|raw| {
            let d = parse_declaration(raw).ok()?;
            if d.capability != capability {
                return None;
            }
            // 无参能力没有参数语义，返回空（与「未声明」同）
            d.param.filter(|_| PARAMETRIC.contains(&d.capability.as_str()))
        })
        .collect()
}

/// 无参能力是否已声明
pub fn has_plain(declared: &[String], capability: &str) -> bool {
    declared.iter().any(|raw| {
        parse_declaration(raw)
            .map(|d| d.capability == capability && d.param.is_none())
            .unwrap_or(false)
    })
}

/// 中心页展示用中文说明（按声明条目；非法条目兜底原名）。
pub fn label(declared: &str) -> String {
    match parse_declaration(declared) {
        Ok(d) => match (d.capability.as_str(), d.param.as_deref()) {
            (CLIPBOARD_WRITE, _) => "写剪贴板（经宿主）".into(),
            (OPEN_URI, _) => "打开链接（http/https/mailto）".into(),
            (LAUNCH_APP, _) => "启动应用（已安装列表内）".into(),
            (NOTIFY, _) => "桌面通知".into(),
            (SELECTED_FILES_READ, _) => "读取文件管理器选中项".into(),
            (SCREENSHOT_TAKE, _) => "截屏（经系统截图接口）".into(),
            (NETWORK_FETCH, Some(host)) => format!("访问网络 {host}"),
            (SETTINGS_WRITE, Some(schema)) => format!("改系统设置 {schema}"),
            (FS_WRITE, Some(path)) => format!("写文件 {path}"),
            _ => d.capability,
        },
        Err(_) => declared.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn accepts_known_plain_and_dedups() {
        let ok = validate(&s(&[CLIPBOARD_WRITE, OPEN_URI, CLIPBOARD_WRITE])).unwrap();
        assert_eq!(ok, s(&[CLIPBOARD_WRITE, OPEN_URI]));
    }

    #[test]
    fn accepts_parametric_declarations() {
        validate(&s(&[
            "network.fetch@api.example.com",
            "network.fetch@10.0.0.8:8080",
            "settings.write@org.gnome.desktop.interface",
            "fs.write@~/.config/Code*/settings.json",
            "fs.write@/opt/share/**/*.conf",
        ]))
        .unwrap();
    }

    #[test]
    fn rejects_unknown_and_malformed() {
        for bad in [
            "network",
            "fs-read",
            "",
            "Clipboard.write",
            // 无参能力带参数
            "clipboard.write@x",
            // 带参能力缺参数
            "network.fetch",
            "fs.write",
            "settings.write",
            // 参数形状非法
            "network.fetch@..",
            "network.fetch@host:99999",
            "network.fetch@host:notaport",
            "settings.write@org.gnome..x",
            "settings.write@schema space",
            "fs.write@relative/path",
            "fs.write@",
        ] {
            assert!(validate(&s(&[bad])).is_err(), "{bad:?} 应被拒");
        }
    }

    #[test]
    fn params_of_and_has_plain() {
        let d = s(&[
            "network.fetch@a.com",
            "network.fetch@b.com",
            "clipboard.write",
            "fs.write@~/.config/x.json",
        ]);
        assert_eq!(params_of(&d, NETWORK_FETCH), s(&["a.com", "b.com"]));
        assert_eq!(params_of(&d, FS_WRITE), s(&["~/.config/x.json"]));
        assert_eq!(params_of(&d, CLIPBOARD_WRITE), s(&[]));
        assert!(has_plain(&d, CLIPBOARD_WRITE));
        assert!(!has_plain(&d, NETWORK_FETCH)); // 带参不算 plain
        assert!(!has_plain(&d, NOTIFY));
    }

    #[test]
    fn labels_show_params() {
        assert_eq!(label("network.fetch@api.x.com"), "访问网络 api.x.com");
        assert_eq!(
            label("fs.write@~/.config/Code*/settings.json"),
            "写文件 ~/.config/Code*/settings.json"
        );
        assert_eq!(
            label("settings.write@org.gnome.desktop.interface"),
            "改系统设置 org.gnome.desktop.interface"
        );
        assert_eq!(label("clipboard.write"), "写剪贴板（经宿主）");
        assert_eq!(label("garbage"), "garbage");
    }
}
