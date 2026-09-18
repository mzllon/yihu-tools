//! zbus 服务与薄 CLI。
//!
//! daemon：在主线程同步声明会话总线名（重名即退出，天然单实例），
//! 服务对象的方法回调发生在 zbus 执行线程，经 mpsc 转回 GTK 主循环消费
//! （同托盘 ksni 的既有惯例）。
//! CLI：连总线调用；bus 名不存在时后台拉起 daemon 并等待就绪后重试
//! （等效 DBus activation，免装 service 文件）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::Duration;

use zbus::blocking::connection::Builder;
use zbus::interface;

pub const BUS_NAME: &str = "tools.yihu.Panel";
pub const OBJ_PATH: &str = "/tools/yihu/Panel";

#[derive(Debug, Clone, Copy)]
pub enum Cmd {
    Toggle,
    Show,
    Hide,
    Quit,
}

pub struct PanelService {
    tx: Mutex<mpsc::Sender<Cmd>>,
    visible: std::sync::Arc<AtomicBool>,
}

#[interface(name = "tools.yihu.Panel")]
impl PanelService {
    fn toggle(&self) {
        let _ = self.tx.lock().unwrap().send(Cmd::Toggle);
    }
    fn show(&self) {
        let _ = self.tx.lock().unwrap().send(Cmd::Show);
    }
    fn hide(&self) {
        let _ = self.tx.lock().unwrap().send(Cmd::Hide);
    }
    fn quit(&self) {
        let _ = self.tx.lock().unwrap().send(Cmd::Quit);
    }
    #[zbus(property)]
    fn visible(&self) -> bool {
        self.visible.load(Ordering::Relaxed)
    }
}

/// 在当前线程同步声明总线名并返回常驻服务连接。
/// 失败（多为名字已被占用）时返回 Err，调用方应静默退出。
pub fn claim(
    tx: mpsc::Sender<Cmd>,
    visible: std::sync::Arc<AtomicBool>,
) -> zbus::Result<zbus::blocking::Connection> {
    Builder::session()?
        .name(BUS_NAME)?
        .serve_at(
            OBJ_PATH,
            PanelService {
                tx: Mutex::new(tx),
                visible,
            },
        )?
        .build()
}

// ---- CLI ----

fn call(conn: &zbus::blocking::Connection, cmd: Cmd) -> zbus::Result<()> {
    let proxy = zbus::blocking::Proxy::new(conn, BUS_NAME, OBJ_PATH, BUS_NAME)?;
    match cmd {
        Cmd::Toggle => { let _: () = proxy.call("Toggle", &())?; }
        Cmd::Show => { let _: () = proxy.call("Show", &())?; }
        Cmd::Hide => { let _: () = proxy.call("Hide", &())?; }
        Cmd::Quit => { let _: () = proxy.call("Quit", &())?; }
    }
    Ok(())
}

fn has_owner(conn: &zbus::blocking::Connection) -> zbus::Result<bool> {
    let dbus = zbus::blocking::Proxy::new(
        conn,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )?;
    let owned: bool = dbus.call("NameHasOwner", &BUS_NAME)?;
    Ok(owned)
}

pub fn run_cli(cmd: Cmd) -> i32 {
    let conn = match zbus::blocking::Connection::session() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("yihu-panel: {e}");
            return 1;
        }
    };
    match has_owner(&conn) {
        Ok(true) => match call(&conn, cmd) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("yihu-panel: {e}");
                1
            }
        },
        Ok(false) => ensure_and_call(cmd),
        Err(e) => {
            eprintln!("yihu-panel: {e}");
            1
        }
    }
}

/// daemon 未运行：后台拉起本二进制（无参 = daemon），等总线名就绪后重试调用。
fn ensure_and_call(cmd: Cmd) -> i32 {
    let spawned = std::env::current_exe().ok().is_some_and(|exe| {
        std::process::Command::new(exe)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .is_ok()
    });
    if !spawned {
        eprintln!("yihu-panel: 无法拉起守护进程");
        return 1;
    }
    let conn = match zbus::blocking::Connection::session() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("yihu-panel: {e}");
            return 1;
        }
    };
    for _ in 0..60 {
        std::thread::sleep(Duration::from_millis(50));
        if has_owner(&conn).unwrap_or(false) {
            return match call(&conn, cmd) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("yihu-panel: {e}");
                    1
                }
            };
        }
    }
    eprintln!("yihu-panel: 守护进程 3s 内未就绪");
    1
}

pub fn run_status() -> i32 {
    match zbus::blocking::Connection::session().and_then(|c| has_owner(&c)) {
        Ok(true) => {
            println!("运行中");
            0
        }
        Ok(false) => {
            println!("未运行");
            1
        }
        Err(e) => {
            eprintln!("yihu-panel: {e}");
            2
        }
    }
}

/// 通知 GNOME Shell 定位扩展（可选组件）把面板摆到上部居中并置顶。
/// 扩展未安装时静默忽略，位置由系统默认摆放。
/// 窗口映射与扩展查找有先后，分多个时间点重试。
pub fn call_placer(offset_up: i32) {
    std::thread::spawn(move || {
        for delay in [80u64, 200, 450] {
            std::thread::sleep(Duration::from_millis(delay));
            let ok = (|| -> zbus::Result<()> {
                let conn = zbus::blocking::Connection::session()?;
                let proxy = zbus::blocking::Proxy::new(
                    &conn,
                    "tools.yihu.ShellPlacer",
                    "/tools/yihu/ShellPlacer",
                    "tools.yihu.ShellPlacer",
                )?;
                // 新签名带偏移；对未更新的旧扩展回退无参调用
                if proxy.call::<_, _, ()>("PlaceTop", &offset_up).is_ok() {
                    return Ok(());
                }
                let _: () = proxy.call("PlaceTop", &())?;
                Ok(())
            })()
            .is_ok();
            if ok {
                return;
            }
        }
    });
}
