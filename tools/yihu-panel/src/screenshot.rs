//! 宿主截屏能力（M4 截图插件配套）：经 xdg-desktop-portal 执行。
//!
//! Wayland 下插件进程无任何显示服务/总线访问，截屏必须由宿主代理——
//! 这是「能力代理优先」原则的延伸：系统交互 = 窄能力接口 + 宿主校验。
//!
//! 实现：org.freedesktop.portal.Screenshot（GNOME 后端）。
//! - mode=full：非交互，立即全屏 → 响应带 uri；
//! - mode=area：interactive=true，打开 GNOME 截图工具（区域/窗口/录屏
//!   由用户在里面选），完成后响应带 uri；用户取消 → response code 1。
//!
//! 阻塞信号迭代器无超时 API（zbus blocking），用「守护线程 + 应答标志」
//! 实现硬超时：先到者（结果或超时）置 answered 并交付，后到者丢弃——
//! 插件侧按 request_id 幂等，双重投递在协议上安全，这里只做单次交付。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use zbus::zvariant::{ObjectPath, OwnedObjectPath, Value};

/// 交互模式等待上限（用户框选/标注可能较久）
pub const INTERACTIVE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// 非交互全屏上限
pub const FULL_TIMEOUT: Duration = Duration::from_secs(30);

/// 执行一次截屏。阻塞调用（只准后台线程用），返回按需读入的 PNG 字节
///（clipboard=true 时），或 Err(用户取消/超时/失败)。
pub fn take(mode: &str, clipboard: bool) -> Result<Option<Vec<u8>>, String> {
    let timeout = if mode == "area" {
        INTERACTIVE_TIMEOUT
    } else {
        FULL_TIMEOUT
    };
    let answered = Arc::new(AtomicBool::new(false));
    let (tx, rx) = std::sync::mpsc::channel::<Result<String, String>>();

    // portal 调用线程（可能长时间阻塞在信号等待）
    let tx_portal = tx.clone();
    let answered_portal = answered.clone();
    let mode2 = mode.to_string();
    std::thread::Builder::new()
        .name("yihu-screenshot".into())
        .spawn(move || {
            let r = portal_take(&mode2).and_then(|uri| decode_file_uri(&uri));
            if !answered_portal.swap(true, Ordering::SeqCst) {
                let _ = tx_portal.send(r.map_err(|e| e));
            }
        })
        .map_err(|e| format!("截图线程启动失败：{e}"))?;

    // 守护线程：睡满超时后仍未交付 → 交付超时错误（不占用 rx）
    let tx_guard = tx.clone();
    let answered_guard = answered.clone();
    std::thread::Builder::new()
        .name("yihu-screenshot-guard".into())
        .spawn(move || {
            std::thread::sleep(timeout);
            if !answered_guard.swap(true, Ordering::SeqCst) {
                let _ = tx_guard.send(Err("截图超时（未完成或未响应）".into()));
            }
        })
        .map_err(|e| format!("截图守护线程启动失败：{e}"))?;

    drop(tx); // 结果线程/守护线程二选一交付
    let path = rx
        .recv()
        .map_err(|_| "截图结果通道已关闭".to_string())??;
    let png = if clipboard {
        Some(
            std::fs::read(&path).map_err(|e| format!("读取截图失败：{e}"))?,
        )
    } else {
        None
    };
    Ok(png)
}

/// 调 portal：返回截图文件 URI。阻塞（含信号等待），只准专用线程调用。
fn portal_take(mode: &str) -> Result<String, String> {
    let conn = zbus::blocking::Connection::session()
        .map_err(|e| format!("DBus 连接失败：{e}"))?;
    let proxy = zbus::blocking::Proxy::new(
        &conn,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.Screenshot",
    )
    .map_err(|e| format!("portal 代理失败：{e}"))?;

    let mut options: HashMap<&str, Value> = HashMap::new();
    options.insert("handle_token", Value::from("yihu"));
    options.insert("interactive", Value::from(mode == "area"));
    let req: OwnedObjectPath = proxy
        .call("Screenshot", &("", &options))
        .map_err(|e| format!("portal Screenshot 调用失败：{e}"))?;

    // 请求对象上的 Response 信号：body = (u32 code, a{sv} results)
    let req_proxy = zbus::blocking::Proxy::new(
        &conn,
        "org.freedesktop.portal.Desktop",
        ObjectPath::from(req),
        "org.freedesktop.portal.Request",
    )
    .map_err(|e| format!("request 代理失败：{e}"))?;
    let mut signals = req_proxy
        .receive_signal("Response")
        .map_err(|e| format!("等待截图响应失败：{e}"))?;
    for msg in signals.by_ref() {
        let body = msg.body();
        let (code, results): (u32, HashMap<String, Value>) = body
            .deserialize()
            .map_err(|e| format!("响应解析失败：{e}"))?;
        if code != 0 {
            return Err("已取消".into());
        }
        return match results.get("uri") {
            Some(Value::Str(s)) if !s.is_empty() => Ok(s.to_string()),
            _ => Err("响应缺少截图 URI".into()),
        };
    }
    Err("portal 响应流意外结束".into())
}

/// file:// URI → 本地路径（file:// + 百分号转义解码；非 file URI 拒绝）
pub fn decode_file_uri(uri: &str) -> Result<String, String> {
    let Some(rest) = uri.strip_prefix("file://") else {
        return Err(format!("非 file URI：{uri}"));
    };
    // 去掉 host 段（file://localhost/... → /...）
    let path = rest
        .strip_prefix("localhost")
        .unwrap_or(rest);
    if !path.starts_with('/') {
        return Err(format!("URI 路径非法：{uri}"));
    }
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 3 <= bytes.len() {
            match std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|s| u8::from_str_radix(s, 16).ok())
            {
                Some(b) => {
                    out.push(b);
                    i += 3;
                }
                None => {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| "URI 解码后非 UTF-8".into())
}

/// 通知文案用：取截图目录里最新的 PNG 路径（portal 的 URI 在 take
/// 内部只用于读盘，这里从 Pictures/Screenshots 兜底推断；失败返回空）。
pub fn latest_shot_hint() -> String {
    let base = std::env::var("XDG_PICTURES_DIR")
        .ok()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
                .join("图片")
        });
    let dir = if base.join("Screenshots").is_dir() {
        base.join("Screenshots")
    } else {
        base
    };
    std::fs::read_dir(&dir)
        .ok()
        .and_then(|rd| {
            rd.flatten()
                .filter(|e| {
                    e.path()
                        .extension()
                        .is_some_and(|x| x.eq_ignore_ascii_case("png"))
                })
                .map(|e| e.path())
                .max_by_key(|p| {
                    p.metadata()
                        .and_then(|m| m.modified())
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
                })
        })
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_plain_and_escaped_uris() {
        assert_eq!(decode_file_uri("file:///home/u/Pictures/a.png").unwrap(), "/home/u/Pictures/a.png");
        assert_eq!(
            decode_file_uri("file:///home/u/%E5%9B%BE%E7%89%87/a%20b.png").unwrap(),
            "/home/u/图片/a b.png"
        );
        assert_eq!(decode_file_uri("file://localhost/tmp/x.png").unwrap(), "/tmp/x.png");
        assert!(decode_file_uri("https://x/a.png").is_err());
        assert!(decode_file_uri("file://relative/path").is_err());
        // 坏转义按字面保留
        assert_eq!(decode_file_uri("file:///tmp/%zz.png").unwrap(), "/tmp/%zz.png");
    }

    #[test]
    fn take_fails_gracefully_without_portal_response() {
        // 无 portal 的环境下应返回 Err 而不是 panic/挂死
        //（有 portal 的桌面机会走真实调用——测试只在无会话总线时断言）
        if std::env::var("DBUS_SESSION_BUS_ADDRESS").is_ok() {
            return; // 桌面会话内不真截屏
        }
        let r = take("full", false);
        assert!(r.is_err());
    }
}
