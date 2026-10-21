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
    pixels_playing: ksni::Icon,
    pixels_stopped: ksni::Icon,
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
        vec![ksni::Icon {
            width: data.width,
            height: data.height,
            data: data.data.clone(),
        }]
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
    let (playing_icon, stopped_icon) = crate::icons::tray_pixmaps();
    let service = ksni::TrayService::new(Tray {
        commands,
        pixels_playing: playing_icon,
        pixels_stopped: stopped_icon,
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

// 托盘图标：icons.rs 内嵌的 radio 样式 PNG（播放=彩色/停止=灰度），
// install() 时在主线程解码一次为 ksni ARGB32 像素。

#[cfg(test)]
mod tests {
    use super::*;
    use ksni::Tray as _;

    fn tray_for_test() -> (Tray, Receiver<Command>) {
        let (tx, rx) = mpsc::channel();
        let (playing, stopped) = crate::icons::tray_pixmaps();
        (
            Tray {
                commands: tx,
                pixels_playing: playing,
                pixels_stopped: stopped,
                registered: Arc::new(AtomicBool::new(false)),
                playing: false,
                station: String::new(),
            },
            rx,
        )
    }

    #[test]
    fn icon_is_32x32_radio_artwork() {
        let (playing, stopped) = crate::icons::tray_pixmaps();
        for icon in [&playing, &stopped] {
            assert_eq!(icon.width, 32);
            assert_eq!(icon.height, 32);
            assert_eq!(icon.data.len(), 32 * 32 * 4);
        }
        // 中心是白色发射源（圆点圆心）
        let at = |icon: &ksni::Icon, x: i32, y: i32| -> [u8; 4] {
            let o = ((y * 32 + x) * 4) as usize;
            [icon.data[o], icon.data[o + 1], icon.data[o + 2], icon.data[o + 3]]
        };
        let c = at(&playing, 16, 16);
        assert_eq!(c[0], 255, "中心应不透明");
        assert_eq!((c[1], c[2], c[3]), (255, 255, 255), "中心应纯白");
        // 播放态中心区带彩色底（非灰度）：取弧外内部点 (16, 5)
        // 停止态整体灰度：所有像素 R==G==B
        for p in stopped.data.chunks_exact(4) {
            assert_eq!(p[1], p[2], "停止态应灰度（R==G）");
            assert_eq!(p[2], p[3], "停止态应灰度（G==B）");
        }
        assert_ne!(playing.data, stopped.data);
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
        // 停止态切到灰度图标（与播放态像素不同）
        assert_ne!(
            t.icon_pixmap()[0].data,
            t.pixels_playing.data,
            "停止态应切换为灰度图标"
        );
    }
}
