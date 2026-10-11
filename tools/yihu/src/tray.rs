use adw::prelude::*;
use gtk::gdk_pixbuf;
use gtk::{gio, glib};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Command {
    Show,
    Quit,
    ToggleAutostart,
}

static HANDLE: OnceLock<Mutex<ksni::Handle<Tray>>> = OnceLock::new();

struct Tray {
    commands: mpsc::Sender<Command>,
    pixels: Vec<u8>,
    registered: Arc<AtomicBool>,
    autostart: bool,
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
        "一呼 · 工具箱".into()
    }
    fn activate(&mut self, _: i32, _: i32) {
        let _ = self.commands.send(Command::Show);
    }
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![ksni::Icon { width: 32, height: 32, data: self.pixels.clone() }]
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        vec![
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
    let autostart = crate::page_settings::autostart_enabled();
    let service = ksni::TrayService::new(Tray {
        commands,
        pixels,
        registered: registered.clone(),
        autostart,
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
        };
        tray.watcher_online();
        assert!(tray.registered.load(Ordering::Acquire));
        assert!(tray.watcher_offine());
        assert!(!tray.registered.load(Ordering::Acquire));
        tray.activate(0, 0);
        assert_eq!(receiver.try_recv().unwrap(), Command::Show);
        for enabled in [false, true] {
            tray.autostart = enabled;
            if let ksni::MenuItem::Checkmark(item) = &tray.menu()[1] {
                assert_eq!(item.label, "开机启动");
                assert_eq!(item.checked, enabled);
                (item.activate)(&mut tray);
                assert_eq!(receiver.try_recv().unwrap(), Command::ToggleAutostart);
                assert_eq!(tray.autostart, enabled);
            } else { panic!("expected autostart checkmark"); }
        }
        let menu = tray.menu();
        for (index, expected) in [(0, Command::Show), (3, Command::Quit)] {
            if let ksni::MenuItem::Standard(item) = &menu[index] {
                (item.activate)(&mut tray);
                assert_eq!(receiver.try_recv().unwrap(), expected);
            } else { panic!("expected menu action"); }
        }
        assert_eq!(tray.title(), "一呼 · 工具箱");
    }
}
