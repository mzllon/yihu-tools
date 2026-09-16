use adw::prelude::*;
use gtk::gdk_pixbuf;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Command {
    Show,
    Quit,
    ToggleAutostart,
    PlayPause,
    OpenRadio,
}

/// 「暂停/继续」由广播页注入（转发到 MPRIS 命令通道，主线程消费）。
static PLAY_HOOK: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();
static HANDLE: OnceLock<Mutex<ksni::Handle<Tray>>> = OnceLock::new();

thread_local! {
    /// 「打开广播页」需要操作 GTK 对象（非 Send），仅主线程设置与调用。
    static OPEN_RADIO_HOOK: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
}

/// 托盘菜单顶部的「正在播放」行：节目 + 播放/暂停 合并在同一行，
/// 行图标为灰色圆盘悬浮状态角标（⏸/▶ 画在圆盘上）。
pub struct NowPlaying {
    pub playing: bool,
    /// 「电台」或「电台 · 节目」；从未播放时为空（显示引导文案）。
    pub text: String,
}

pub fn set_now_playing(np: NowPlaying) {
    let icon = compose_icon(np.playing);
    if let Some(handle) = HANDLE.get() {
        handle.lock().unwrap().update(|tray| {
            *tray.now.lock().unwrap() = NowState {
                playing: np.playing,
                text: np.text,
                icon,
            };
        });
    }
}

/// 注入「暂停/继续」动作（广播页在主线程消费）。
pub fn set_play_hook(hook: Box<dyn Fn() + Send + Sync>) {
    let _ = PLAY_HOOK.set(hook);
}

/// 注入「打开广播页」动作（需操作 GTK 对象，主线程调用）。
pub fn set_open_radio_page_hook(hook: Box<dyn Fn()>) {
    OPEN_RADIO_HOOK.with(|slot| *slot.borrow_mut() = Some(hook));
}

fn invoke_open_radio() {
    OPEN_RADIO_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow().as_ref() {
            hook();
        }
    });
}

#[derive(Debug, Clone)]
struct NowState {
    playing: bool,
    text: String,
    icon: Vec<u8>,
}

struct Tray {
    commands: mpsc::Sender<Command>,
    pixels: Vec<u8>,
    registered: Arc<AtomicBool>,
    autostart: bool,
    now: Mutex<NowState>,
}

impl ksni::Tray for Tray {
    fn watcher_online(&self) {
        self.registered.store(true, Ordering::Release);
    }
    fn watcher_offine(&self) -> bool {
        self.registered.store(false, Ordering::Release);
        true
    }
    fn id(&self) -> String { super::APP_ID.into() }
    fn title(&self) -> String {
        let now = self.now.lock().unwrap();
        if now.playing && !now.text.is_empty() {
            format!("一呼 · 正在播放:{}", now.text)
        } else {
            "一呼 · 工具箱".into()
        }
    }
    fn activate(&mut self, _: i32, _: i32) {
        let _ = self.commands.send(Command::Show);
    }
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![ksni::Icon { width: 32, height: 32, data: self.pixels.clone() }]
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        let now = self.now.lock().unwrap();
        let (label, click) = if now.text.is_empty() {
            ("▶ 打开广播页,开始收听".to_owned(), Command::OpenRadio)
        } else {
            (now.text.clone(), Command::PlayPause)
        };
        vec![
            ksni::menu::StandardItem {
                label,
                icon_data: now.icon.clone(),
                activate: Box::new(move |tray: &mut Self| {
                    let _ = tray.commands.send(click);
                }),
                ..Default::default()
            }.into(),
            ksni::menu::StandardItem {
                label: "打开一呼".into(),
                activate: Box::new(|tray: &mut Self| { let _ = tray.commands.send(Command::Show); }),
                ..Default::default()
            }.into(),
            ksni::menu::CheckmarkItem {
                label: "开机启动".into(),
                checked: self.autostart,
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.commands.send(Command::ToggleAutostart);
                }),
                ..Default::default()
            }.into(),
            ksni::MenuItem::Separator,
            ksni::menu::StandardItem {
                label: "退出".into(),
                activate: Box::new(|tray: &mut Self| { let _ = tray.commands.send(Command::Quit); }),
                ..Default::default()
            }.into(),
        ]
    }
}

/// 合成菜单行图标：灰色圆盘，中央悬浮 ⏸（播放中）/ ▶（已停止）状态角标。
/// 不用节目封面——缩到菜单图标尺寸后照片不可辨识。输出 64×64 PNG。
pub fn compose_icon(playing: bool) -> Vec<u8> {
    const S: i32 = 64;
    let disc = gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, true, 8, S, S)
        .expect("64×64 RGBA Pixbuf 必可创建");
    let rowstride = disc.rowstride() as usize;
    let n = disc.n_channels() as usize;
    let mut pixels = disc.read_pixel_bytes().to_vec();
    for y in 0..S as usize {
        for x in 0..S as usize {
            let i = y * rowstride + x * n;
            pixels[i..i + n].copy_from_slice(&[71, 71, 71, 255]);
        }
    }

    // 中央悬浮角标：半透明深色圆 + 白色 ⏸ / ▶
    let size = S as f32;
    let badge_r = size * 0.24;
    let (cx, cy) = (size / 2.0, size / 2.0);
    for y in 0..S as usize {
        for x in 0..S as usize {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            if dx * dx + dy * dy <= badge_r * badge_r {
                let i = y * rowstride + x * n;
                pixels[i] = 0;
                pixels[i + 1] = 0;
                pixels[i + 2] = 0;
                pixels[i + 3] = 165;
            }
        }
    }
    let white = [255u8, 255, 255, 255];
    let mut put = |x: i32, y: i32| {
        if x < 0 || y < 0 || x >= S || y >= S {
            return;
        }
        let i = y as usize * rowstride + x as usize * n;
        pixels[i..i + n].copy_from_slice(&white);
    };
    let (icx, icy) = (cx as i32, cy as i32);
    if playing {
        // ⏸：两根竖条
        for y in (icy - 8)..=(icy + 8) {
            for dx in -7..=-3 {
                put(icx + dx, y);
            }
            for dx in 3..=7 {
                put(icx + dx, y);
            }
        }
    } else {
        // ▶：三角形
        let a = (icx - 6, icy - 9);
        let b = (icx - 6, icy + 9);
        let c = (icx + 11, icy);
        for y in (icy - 9)..=(icy + 9) {
            for x in (icx - 6)..=(icx + 11) {
                let d1 = (x - a.0) * (b.1 - a.1) - (b.0 - a.0) * (y - a.1);
                let d2 = (x - b.0) * (c.1 - b.1) - (c.0 - b.0) * (y - b.1);
                let d3 = (x - c.0) * (a.1 - c.1) - (a.0 - c.0) * (y - c.1);
                let negative = d1 < 0 || d2 < 0 || d3 < 0;
                let positive = d1 > 0 || d2 > 0 || d3 > 0;
                if !(negative && positive) {
                    put(x, y);
                }
            }
        }
    }
    let out = gdk_pixbuf::Pixbuf::from_bytes(
        &glib::Bytes::from_owned(pixels),
        gdk_pixbuf::Colorspace::Rgb,
        true,
        8,
        S,
        S,
        rowstride as i32,
    );
    out.save_to_bufferv("png", &[]).unwrap_or_default()
}

fn icon_pixels() -> Result<Vec<u8>, glib::Error> {
    let stream =
        gio::MemoryInputStream::from_bytes(&glib::Bytes::from_static(include_bytes!("../icons/32x32.png")));
    let icon = gdk_pixbuf::Pixbuf::from_stream(&stream, gio::Cancellable::NONE)?;
    let bytes = icon.read_pixel_bytes();
    let mut pixels = Vec::with_capacity(32 * 32 * 4);
    for y in 0..32 {
        for x in 0..32 {
            let offset = y * icon.rowstride() as usize + x * icon.n_channels() as usize;
            let rgb = &bytes.as_ref()[offset..];
            pixels.extend_from_slice(&[
                if icon.has_alpha() { rgb[3] } else { 255 },
                rgb[0],
                rgb[1],
                rgb[2],
            ]);
        }
    }
    Ok(pixels)
}

pub fn install(app: &adw::Application, window: &adw::ApplicationWindow) {
    let pixels = match icon_pixels() {
        Ok(pixels) => pixels,
        Err(error) => { eprintln!("无法加载托盘图标：{error}"); return; }
    };
    let watcher = match gio::DBusProxy::for_bus_sync(
        gio::BusType::Session,
        gio::DBusProxyFlags::DO_NOT_AUTO_START,
        None,
        "org.kde.StatusNotifierWatcher",
        "/StatusNotifierWatcher",
        "org.kde.StatusNotifierWatcher",
        gio::Cancellable::NONE,
    ) {
        Ok(watcher) => watcher,
        Err(error) => { eprintln!("系统托盘不可用：{error}"); return; }
    };
    let (commands, receiver) = mpsc::channel();
    let registered = Arc::new(AtomicBool::new(false));
    let now = Mutex::new(NowState {
        playing: false,
        text: String::new(),
        icon: compose_icon(false),
    });
    let autostart = crate::page_settings::autostart_enabled();
    let service = ksni::TrayService::new(Tray {
        commands,
        pixels,
        registered: registered.clone(),
        autostart,
        now,
    });
    let handle = service.handle();
    let menu_handle = handle.clone();
    let _ = HANDLE.set(Mutex::new(handle.clone()));
    let running = Arc::new(AtomicBool::new(true));
    let worker_running = running.clone();
    let worker = std::thread::Builder::new()
        .name("yihu-tray".into())
        .spawn(move || {
            if let Err(error) = service.run() {
                eprintln!("系统托盘服务停止：{error}");
            }
            worker_running.store(false, Ordering::Release);
        });
    if let Err(error) = worker { eprintln!("无法启动系统托盘：{error}"); return; }

    let available = {
        let running = running.clone();
        move || {
            running.load(Ordering::Acquire)
                && registered.load(Ordering::Acquire)
                && watcher.name_owner().is_some()
                && watcher
                    .cached_property("IsStatusNotifierHostRegistered")
                    .and_then(|value| value.get::<bool>())
                    .unwrap_or(false)
        }
    };
    let available = Rc::new(available);
    window.connect_close_request({
        let available = available.clone();
        move |window| {
            if available() {
                window.set_visible(false);
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        }
    });
    let weak_app = app.downgrade();
    let weak_window = window.downgrade();
    // 托盘回调来自 D-Bus 工作线程，GTK 操作只在主线程执行。
    let mut autostart = crate::page_settings::autostart_enabled();
    let source = glib::timeout_add_local(Duration::from_millis(200), move || {
        let (Some(app), Some(window)) = (weak_app.upgrade(), weak_window.upgrade()) else {
            return glib::ControlFlow::Break;
        };
        for command in receiver.try_iter() {
            match command {
                Command::Show => window.present(),
                Command::ToggleAutostart => {
                    let enabled = crate::page_settings::autostart_enabled();
                    if let Err(error) = crate::page_settings::set_autostart(!enabled) {
                        window.present();
                        let dialog = adw::MessageDialog::builder()
                            .transient_for(&window)
                            .modal(true)
                            .heading("无法修改开机启动")
                            .body(error.to_string())
                            .build();
                        dialog.add_response("close", "关闭");
                        dialog.set_close_response("close");
                        dialog.present();
                    }
                }
                Command::PlayPause => {
                    if let Some(hook) = PLAY_HOOK.get() {
                        hook();
                    }
                }
                Command::OpenRadio => invoke_open_radio(),
                Command::Quit => {
                    app.quit();
                    return glib::ControlFlow::Continue;
                }
            }
        }
        let enabled = crate::page_settings::autostart_enabled();
        if enabled != autostart {
            autostart = enabled;
            menu_handle.update(|tray| tray.autostart = enabled);
        }
        if !available() && !window.is_visible() { window.present(); }
        glib::ControlFlow::Continue
    });
    let source = std::cell::RefCell::new(Some(source));
    app.connect_shutdown(move |_| {
        if let Some(handle) = HANDLE.get() {
            handle.lock().unwrap().shutdown();
        }
        if let Some(source) = source.borrow_mut().take() { source.remove(); }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use ksni::Tray as _;

    #[test]
    fn embedded_icon_is_argb32() {
        assert_eq!(icon_pixels().unwrap().len(), 32 * 32 * 4);
    }

    #[test]
    fn tray_activation_and_menu_send_commands() {
        let (commands, receiver) = mpsc::channel();
        let mut tray = Tray {
            commands,
            pixels: vec![],
            registered: Arc::new(AtomicBool::new(false)),
            autostart: false,
            now: Mutex::new(NowState {
                playing: false,
                text: String::new(),
                icon: Vec::new(),
            }),
        };
        tray.watcher_online();
        assert!(tray.registered.load(Ordering::Acquire));
        assert!(tray.watcher_offine());
        assert!(!tray.registered.load(Ordering::Acquire));
        tray.activate(0, 0);
        assert_eq!(receiver.try_recv().unwrap(), Command::Show);
        // 未播放：首行点击 = 打开广播页
        if let ksni::MenuItem::Standard(item) = &tray.menu()[0] {
            (item.activate)(&mut tray);
            assert_eq!(receiver.try_recv().unwrap(), Command::OpenRadio);
        } else { panic!("expected merged row"); }
        for enabled in [false, true] {
            tray.autostart = enabled;
            if let ksni::MenuItem::Checkmark(item) = &tray.menu()[2] {
                assert_eq!(item.label, "开机启动");
                assert_eq!(item.checked, enabled);
                (item.activate)(&mut tray);
                assert_eq!(receiver.try_recv().unwrap(), Command::ToggleAutostart);
                assert_eq!(tray.autostart, enabled);
            } else { panic!("expected autostart checkmark"); }
        }
        let menu = tray.menu();
        for (index, expected) in [(1, Command::Show), (4, Command::Quit)] {
            if let ksni::MenuItem::Standard(item) = &menu[index] {
                (item.activate)(&mut tray);
                assert_eq!(receiver.try_recv().unwrap(), expected);
            } else { panic!("expected menu action"); }
        }
    }

    #[test]
    fn merged_row_click_toggles_playback() {
        let (commands, receiver) = mpsc::channel();
        let mut tray = Tray {
            commands,
            pixels: vec![],
            registered: Arc::new(AtomicBool::new(false)),
            autostart: false,
            now: Mutex::new(NowState {
                playing: true,
                text: "杭州交通91.8电台 · 快活晚高峰".into(),
                icon: Vec::new(),
            }),
        };
        let menu = tray.menu();
        if let ksni::MenuItem::Standard(item) = &menu[0] {
            assert_eq!(item.label, "杭州交通91.8电台 · 快活晚高峰");
            (item.activate)(&mut tray);
            assert_eq!(receiver.try_recv().unwrap(), Command::PlayPause);
        } else { panic!("expected merged row"); }
        assert_eq!(
            tray.title(),
            "一呼 · 正在播放:杭州交通91.8电台 · 快活晚高峰"
        );
    }

    #[test]
    fn composes_disc_icon_png() {
        let png = compose_icon(true);
        assert!(!png.is_empty());
        let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from_owned(png));
        let icon =
            gdk_pixbuf::Pixbuf::from_stream(&stream, gio::Cancellable::NONE).unwrap();
        assert_eq!((icon.width(), icon.height()), (64, 64));
    }
}
