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
    /// 定位线程确认「窗口已配置尺寸且完成映射后摆放」后触发淡入；
    /// gen 为呼出代数，防止迟到的淡入作用于新一代窗口
    FadeIn(u64),
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
        // FadeIn 是定位线程回传守护进程的内部命令，CLI 不会发送
        Cmd::FadeIn(_) => {}
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

/// 通知 GNOME Shell 定位扩展（可选组件）把面板摆到上部居中并按配置上移，
/// 定位落地后经 `fade_tx` 发 `Cmd::FadeIn(gen)` 让主循环淡入窗口。
///
/// 为什么不能摆完就淡入：mutter 50 对映射前的 move_frame 会在窗口真正
/// 映射时重置为默认摆放（日志 "Buggy client ... working around"），
/// 所以映射前的摆放只对旧版 Shell 有效；真正落地的一次必须在窗口
/// 配置好尺寸之后（轮询扩展 Where 看到 W>0），淡入等它完成——
/// 否则用户会看到「先错位后跳转」。扩展缺失/无响应时发 FadeIn 交由
/// 兜底定时器保证面板终会出现。
pub fn call_placer(offset_up: i32, gen: u64, fade_tx: mpsc::Sender<Cmd>) {
    std::thread::spawn(move || {
        let fade = || {
            let _ = fade_tx.send(Cmd::FadeIn(gen));
        };
        let conn = match zbus::blocking::Connection::session() {
            Ok(c) => c,
            Err(_) => return fade(),
        };
        let proxy = match zbus::blocking::Proxy::new(
            &conn,
            "tools.yihu.ShellPlacer",
            "/tools/yihu/ShellPlacer",
            "tools.yihu.ShellPlacer",
        ) {
            Ok(p) => p,
            Err(_) => return fade(),
        };
        // 新签名带偏移；对未更新的旧扩展回退无参调用。
        // 摆放失败不阻断流程：旧扩展在挪完窗口后才抛错，位置照样生效
        let place = || {
            let _ = proxy.call::<_, _, ()>("PlaceTop", &offset_up);
            let _ = proxy.call::<_, _, ()>("PlaceTop", &());
        };
        // Where 应答 Ok(Some((w,h)))；"none" → Ok(None)；总线错误 → Err
        let where_rect = || -> Result<Option<(u32, u32)>, ()> {
            let s = proxy.call::<&str, (), String>("Where", &()).map_err(|_| ())?;
            let Some(dim) = s.split_whitespace().nth(1) else {
                return Ok(None);
            };
            let (w, h) = dim.split_once('x').ok_or(())?;
            Ok(Some((w.parse().map_err(|_| ())?, h.parse().map_err(|_| ())?)))
        };
        let t0 = std::time::Instant::now();
        let mut err_streak = 0;
        let mut tick = 0usize;
        let mut last_size: Option<(u32, u32)> = None;
        let mut stable_ticks = 0usize;
        while t0.elapsed() < Duration::from_secs(5) {
            match where_rect() {
                Err(_) => {
                    // 扩展缺失/长期不响应：不等待，按系统默认位置淡入
                    err_streak += 1;
                    if err_streak >= 8 {
                        return fade();
                    }
                }
                Ok(Some((w, h))) if w > 0 => {
                    err_streak = 0;
                    if last_size != Some((w, h)) {
                        // 尺寸变化（首次配置、GTK 校正默认尺寸）会伴随
                        // mutter 的位置重置，必须跟着重摆一次
                        place();
                        stable_ticks = 0;
                    } else {
                        stable_ticks += 1;
                    }
                    last_size = Some((w, h));
                    if stable_ticks >= 2 {
                        // 尺寸连续多轮稳定：最后一次摆放已落地，可以淡入
                        std::thread::sleep(Duration::from_millis(40));
                        return fade();
                    }
                }
                Ok(_) => {
                    // 尚未映射（none / 0x0）：映射前的摆放只在旧版 Shell
                    // 有效，低频尝试
                    err_streak = 0;
                    if tick % 5 == 0 {
                        place();
                    }
                }
            }
            tick += 1;
            std::thread::sleep(Duration::from_millis(30));
        }
        // 5s 兜底：宁可出现在系统默认位置，也不能永不出现
        fade();
    });
}
