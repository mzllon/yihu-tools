//! 宿主能力词表（M4 权限声明 v1）。
//!
//! manifest 顶层 `permissions` 列表声明插件需要的宿主能力；
//! 中心「插件」页安装时明示，面板能力代理（sessions → capability_request）
//! 按 manifest 逐请求强制。词表是严格白名单：声明未知名 = manifest 解析失败
//! （拒装），防止插件用未知字段绕过宿主认知。
//!
//! 边界（见 docs/插件基座安全模型与发布策略.md §4）：
//! - 沙箱白名单固定为「插件目录只读 + data_dir 读写」，`fs-read`/`fs-write`
//!   不设字段；
//! - `network.fetch` v1 不提供（沙箱 `--unshare-all` 默认无网络）；
//! - 管道即凭证：能力请求只能从宿主拉起的会话 stdio 发出。

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

/// v1 全部合法能力名
pub const KNOWN: &[&str] = &[
    CLIPBOARD_WRITE,
    OPEN_URI,
    LAUNCH_APP,
    NOTIFY,
    SELECTED_FILES_READ,
];

/// 校验 manifest 的 permissions 列表：全部必须为已知能力名，去重保序返回。
pub fn validate(list: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::with_capacity(list.len());
    for p in list {
        if !KNOWN.contains(&p.as_str()) {
            return Err(format!(
                "未知权限 {p:?}（合法：{}）",
                KNOWN.join("、")
            ));
        }
        if !out.iter().any(|x| x == p) {
            out.push(p.clone());
        }
    }
    Ok(out)
}

/// 中心页展示用中文说明（未知名兜底返回原名，正常路径已被 validate 拦截）。
pub fn label(name: &str) -> &'static str {
    match name {
        CLIPBOARD_WRITE => "写剪贴板（经宿主）",
        OPEN_URI => "打开链接（http/https/mailto）",
        LAUNCH_APP => "启动应用（已安装列表内）",
        NOTIFY => "桌面通知",
        SELECTED_FILES_READ => "读取文件管理器选中项",
        _ => "未知权限",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn accepts_known_and_dedups() {
        let ok = validate(&s(&[CLIPBOARD_WRITE, OPEN_URI, CLIPBOARD_WRITE])).unwrap();
        assert_eq!(ok, s(&[CLIPBOARD_WRITE, OPEN_URI]));
        assert!(validate(&[]).unwrap().is_empty());
    }

    #[test]
    fn rejects_unknown_name() {
        for bad in ["network", "fs-read", "clipboard_read", "", "Clipboard.write"] {
            let e = validate(&s(&[bad])).unwrap_err();
            assert!(e.contains("未知权限"), "{bad:?}: {e}");
        }
    }

    #[test]
    fn labels_cover_vocabulary() {
        for name in KNOWN {
            assert!(label(name) != "未知权限", "{name} 缺中文说明");
        }
    }
}
