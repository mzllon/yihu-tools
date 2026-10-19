//! 独立系统托盘：SNI（StatusNotifierItem，freedesktop 标准）自注册。
//!
//! 与 yihu 中心应用同一 ksni 通道——每个进程注册自己的托盘项，多应用/
//! 多插件并存互不耦合（这就是 Linux 托盘的标准形态；中心应用「寄宿菜单
//! 项」的方案只在无 watcher 的环境才有意义，且应做成插件协议通用契约，
//! 不与具体插件耦合）。GNOME 需 AppIndicator 扩展提供 watcher，缺失时
//! 静默降级为无托盘（关窗隐藏 + 面板找回的既有路径不受影响）。
//!
//! GTK 无关设计：菜单命令走 mpsc 回主循环（30ms 泵消费），状态更新走
//! ksni Handle（线程安全），本模块可在无 GTK 上下文下单测。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};

use gtk::gio;
use gtk::gio::prelude::*;

/// 托盘菜单命令（D-Bus 工作线程 → GTK 主循环）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// 显示并置前主窗
    Show,
    /// 播放/停止切换（空状态按恢复链选台）
    Toggle,
    /// 退出（停播）
    Quit,
}

static HANDLE: OnceLock<Mutex<ksni::Handle<Tray>>> = OnceLock::new();

struct Tray {
    commands: Sender<Command>,
    pixels_playing: Vec<u8>,
    pixels_stopped: Vec<u8>,
    registered: Arc<AtomicBool>,
    playing: bool,
    station: String,
}

impl Tray {
    fn state_line(&self) -> String {
        if self.playing {
            if self.station.is_empty() {
                "正在播…".into()
            } else {
                format!("正在播：{}", self.station)
            }
        } else {
            "未在播放".into()
        }
    }
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "tools.yihu.radio".into()
    }

    fn title(&self) -> String {
        "一呼电台".into()
    }

    /// GNOME AppIndicator 点图标是开菜单、不触发 activate；其他 DE 左键
    /// 显示主窗最直观
    fn activate(&mut self, _: i32, _: i32) {
        let _ = self.commands.send(Command::Show);
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        let data = if self.playing {
            &self.pixels_playing
        } else {
            &self.pixels_stopped
        };
        vec![ksni::Icon { width: 32, height: 32, data: data.clone() }]
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            icon_name: String::new(),
            icon_pixmap: Vec::new(),
            title: "一呼电台".into(),
            description: self.state_line(),
        }
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        vec![
            // 状态行（不可点，动态刷新）
            ksni::menu::StandardItem {
                label: self.state_line(),
                enabled: false,
                ..Default::default()
            }
            .into(),
            ksni::menu::StandardItem {
                label: if self.playing { "停止".into() } else { "播放".into() },
                activate: Box::new(|t: &mut Self| {
                    let _ = t.commands.send(Command::Toggle);
                }),
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            ksni::menu::StandardItem {
                label: "显示主窗".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.commands.send(Command::Show);
                }),
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            ksni::menu::StandardItem {
                label: "退出（停播）".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.commands.send(Command::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }

    fn watcher_online(&self) {
        self.registered.store(true, Ordering::Release);
    }

    fn watcher_offine(&self) -> bool {
        self.registered.store(false, Ordering::Release);
        true
    }
}

/// 安装托盘。会话里没有 StatusNotifierWatcher（GNOME 未启用 AppIndicator
/// 扩展等）时返回 None 并留日志——静默降级，不阻塞主功能。
pub fn install() -> Option<Receiver<Command>> {
    let watcher = match gio::DBusProxy::for_bus_sync(
        gio::BusType::Session,
        gio::DBusProxyFlags::DO_NOT_AUTO_START,
        None,
        "org.kde.StatusNotifierWatcher",
        "/StatusNotifierWatcher",
        "org.kde.StatusNotifierWatcher",
        gio::Cancellable::NONE,
    ) {
        Ok(w) => w,
        Err(e) => {
            yihu_core::pwarn!("系统托盘不可用（无 StatusNotifierWatcher）：{e}");
            return None;
        }
    };
    if watcher.name_owner().is_none() {
        yihu_core::pwarn!("系统托盘不可用：会话无 StatusNotifierWatcher，降级为无托盘");
        return None;
    }

    let (commands, receiver) = mpsc::channel();
    let registered = Arc::new(AtomicBool::new(false));
    let service = ksni::TrayService::new(Tray {
        commands,
        pixels_playing: draw_icon(PLAYING_COLOR),
        pixels_stopped: draw_icon(STOPPED_COLOR),
        registered,
        playing: false,
        station: String::new(),
    });
    let handle = service.handle();
    let _ = HANDLE.set(Mutex::new(handle));
    let worker = std::thread::Builder::new().name("radio-tray".into()).spawn(move || {
        if let Err(e) = service.run() {
            yihu_core::pwarn!("托盘服务退出：{e}");
        }
    });
    worker.ok()?;
    Some(receiver)
}

/// 播放态/台名变化时刷新托盘（图标、状态行、tooltip、菜单标签）。
/// 未安装托盘时是空操作。
pub fn set_state(playing: bool, station: &str) {
    if let Some(h) = HANDLE.get() {
        let _ = h.lock().unwrap().update(|t| {
            t.playing = playing;
            t.station = station.to_string();
        });
    }
}

/// 应用退出时停掉 ksni 服务线程。
pub fn shutdown() {
    if let Some(h) = HANDLE.get() {
        h.lock().unwrap().shutdown();
    }
}

/// 播放中：一呼强调橙；停止：中性灰
const PLAYING_COLOR: [u8; 3] = [0xc6, 0x46, 0x00];
const STOPPED_COLOR: [u8; 3] = [0x5d, 0x67, 0x70];

/// 32×32 ARGB（ksni 布局：A 前置）：圆角方块底 + 白色播放三角。
/// 纯函数无外部依赖，托盘不嵌 PNG 资源。
fn draw_icon(accent: [u8; 3]) -> Vec<u8> {
    const W: i32 = 32;
    const H: i32 = 32;
    const R: f64 = 8.0;
    let mut px = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let cx = x.min(W - 1 - x) as f64;
            let cy = y.min(H - 1 - y) as f64;
            // 圆角：角区落在以 (R,R) 为圆心、R 为半径的四分之一圆内
            let inside = cx >= R || cy >= R || (cx - R).hypot(cy - R) <= R;
            // 播放三角 (12,10)-(12,22)-(23,16)，半像素中心采样
            let tri = point_in_triangle(
                (x as f64 + 0.5, y as f64 + 0.5),
                ((12.0, 10.0), (12.0, 22.0), (23.0, 16.0)),
            );
            let (a, rgb) = if !inside {
                (0, [0, 0, 0])
            } else if tri {
                (255, [255, 255, 255])
            } else {
                (255, accent)
            };
            px.extend_from_slice(&[a, rgb[0], rgb[1], rgb[2]]);
        }
    }
    px
}

/// 同号叉积点在三角形内（含边界）
fn point_in_triangle(p: (f64, f64), t: ((f64, f64), (f64, f64), (f64, f64))) -> bool {
    let sign = |a: (f64, f64), b: (f64, f64), c: (f64, f64)| {
        (a.0 - c.0) * (b.1 - c.1) - (b.0 - c.0) * (a.1 - c.1)
    };
    let d1 = sign(p, t.0, t.1);
    let d2 = sign(p, t.1, t.2);
    let d3 = sign(p, t.2, t.0);
    let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_neg && has_pos)
}

/// 诊断：把图标转储成 PPM（P6，纯标准库；ImageMagick 可转 PNG 核对）
#[cfg(test)]
fn dump_ppm(path: &std::path::Path) {
    use std::io::Write as _;
    let argb = draw_icon(PLAYING_COLOR);
    let mut ppm = format!("P6\n32 32\n255\n").into_bytes();
    for c in argb.chunks_exact(4) {
        ppm.extend_from_slice(&[c[1], c[2], c[3]]);
    }
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(&ppm).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use ksni::Tray as _;

    fn tray_for_test() -> (Tray, Receiver<Command>) {
        let (tx, rx) = mpsc::channel();
        (
            Tray {
                commands: tx,
                pixels_playing: draw_icon(PLAYING_COLOR),
                pixels_stopped: draw_icon(STOPPED_COLOR),
                registered: Arc::new(AtomicBool::new(false)),
                playing: false,
                station: String::new(),
            },
            rx,
        )
    }

    #[test]
    fn icon_is_32x32_argb_with_shape() {
        let px = draw_icon(PLAYING_COLOR);
        assert_eq!(px.len(), 32 * 32 * 4);
        let at = |x: i32, y: i32| -> [u8; 4] {
            let o = ((y * 32 + x) * 4) as usize;
            [px[o], px[o + 1], px[o + 2], px[o + 3]]
        };
        // 四角在圆角外 → 全透明
        for (x, y) in [(0, 0), (31, 0), (0, 31), (31, 31)] {
            assert_eq!(at(x, y)[0], 0, "角 ({x},{y}) 应透明");
        }
        // 底色区：强调橙；三角中心：白
        assert_eq!(at(4, 16), [255, PLAYING_COLOR[0], PLAYING_COLOR[1], PLAYING_COLOR[2]]);
        assert_eq!(at(14, 16), [255, 255, 255, 255]);
        // 停止态底色是灰：取内部非三角像素 (4,16)
        let off = draw_icon(STOPPED_COLOR);
        let o = ((16 * 32 + 4) * 4) as usize;
        assert_eq!(&off[o..o + 4], &[255, STOPPED_COLOR[0], STOPPED_COLOR[1], STOPPED_COLOR[2]]);
        // 转储核对用（ImageMagick 可转 PNG 肉眼看）
        dump_ppm(&std::env::temp_dir().join("radio-tray-icon.ppm"));
    }

    #[test]
    fn menu_and_state_reflect_playing() {
        let (mut t, rx) = tray_for_test();
        assert_eq!(t.title(), "一呼电台");
        assert_eq!(t.state_line(), "未在播放");
        assert_eq!(t.id(), "tools.yihu.radio");

        t.station = "杭州交通91.8电台".into();
        t.playing = true;
        assert_eq!(t.state_line(), "正在播：杭州交通91.8电台");

        let menu = t.menu();
        // 布局：[0] 状态行(禁用) [1] 停止 | [3] 显示主窗 | [5] 退出
        let label_of =
            |m: &Vec<ksni::MenuItem<Tray>>, i: usize| -> String {
                match &m[i] {
                    ksni::MenuItem::Standard(item) => item.label.clone(),
                    _ => panic!("期望 StandardItem[{i}]"),
                }
            };
        assert!(label_of(&menu, 0).starts_with("正在播："));
        assert!(!matches!(&menu[0], ksni::MenuItem::Standard(s) if s.enabled));
        assert_eq!(label_of(&menu, 1), "停止"); // playing=true → 显示「停止」
        assert_eq!(label_of(&menu, 3), "显示主窗");
        assert_eq!(label_of(&menu, 5), "退出（停播）");
        assert!(matches!(menu[2], ksni::MenuItem::Separator));
        assert!(matches!(menu[4], ksni::MenuItem::Separator));

        for idx in [1usize, 3, 5] {
            if let ksni::MenuItem::Standard(item) = &menu[idx] {
                (item.activate)(&mut t);
            }
        }
        assert_eq!(rx.recv().unwrap(), Command::Toggle);
        assert_eq!(rx.recv().unwrap(), Command::Show);
        assert_eq!(rx.recv().unwrap(), Command::Quit);

        // 停止态：状态行与切换标签翻转（menu 是构造时快照，须重取）
        t.playing = false;
        assert_eq!(t.state_line(), "未在播放");
        let menu = t.menu();
        assert_eq!(label_of(&menu, 1), "播放");
        let o = ((16 * 32 + 4) * 4) as usize;
        assert_eq!(
            &t.icon_pixmap()[0].data[o..o + 4],
            &[255, STOPPED_COLOR[0], STOPPED_COLOR[1], STOPPED_COLOR[2]]
        );
    }
}
