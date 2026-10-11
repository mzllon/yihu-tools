//! 钉住（贴图，M6 截图引擎）：把图片钉在桌面置顶显示（Snipaste 贴图形态）。
//!
//! 入口：面板「贴图」磁贴/搜索行（钉剪贴板里的图片）+ 自研区域截图确认后
//! 按 P（钉选区）。
//! 交互（无边框窗）：**按下拖动 = 原生移动**（gdk begin_move，Wayland 合规
//! 路径——GTK4 无法编程定位窗口，移动必须由合成器接管）；**滚轮缩放**
//! （size_request 驱动窗口大小）；双击 = 复制图像并关；Esc = 关。
//! 层级说明：GTK4/Wayland 不允许应用自设 keep-above，贴图窗为普通层级
//! （无标题栏、点哪钉哪）；随守护进程存活（会话级，不持久化）。
//!
//! 零权限：贴图是宿主自有窗口，纯本地；钉剪贴板图走 gdk 异步读纹理
//!（不阻塞主循环，同 clipboard.read 能力实现）。

use gtk::gio;
use gtk::gdk;
use gtk::gdk::prelude::ToplevelExt as _;
use gtk::prelude::*;
use gtk::glib;
use gtk::{EventControllerScroll, GestureClick, Picture, Window};
use std::cell::Cell;
use std::rc::Rc;

thread_local! {
    static OPEN: std::cell::RefCell<Vec<Rc<PinWin>>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// 缩放上下限
pub const ZOOM_MIN: f64 = 0.1;
pub const ZOOM_MAX: f64 = 8.0;

pub fn zoom_step(zoom: f64, up: bool) -> f64 {
    let z = if up { zoom * 1.1 } else { zoom / 1.1 };
    z.clamp(ZOOM_MIN, ZOOM_MAX)
}

struct PinWin {
    win: Window,
    tex: gdk::Texture,
    zoom: Cell<f64>,
    picture: Picture,
}

impl PinWin {
    fn relayout(&self) {
        let w = (self.tex.width() as f64 * self.zoom.get()).round() as i32;
        let h = (self.tex.height() as f64 * self.zoom.get()).round() as i32;
        // size_request 驱动窗口尺寸（resizable(false) 不拦程序化调整）
        self.picture.set_size_request(w.max(8), h.max(8));
    }
}

/// 钉一张图（主线程调用）
pub fn pin_pixbuf(pb: gtk::gdk_pixbuf::Pixbuf, audit: &yihu_core::audit::Audit) {
    create_pin(gdk::Texture::for_pixbuf(&pb), audit);
}


/// 钉剪贴板里的图片（主线程；gdk 异步读纹理）
pub fn pin_from_clipboard(audit: &Rc<yihu_core::audit::Audit>) {
    let Some(display) = gdk::Display::default() else {
        notify("贴图失败：无显示服务");
        return;
    };
    let audit = audit.clone();
    display
        .clipboard()
        .read_texture_async(gio::Cancellable::NONE, move |res| match res {
            Ok(Some(tex)) => create_pin(tex, &audit),
            Ok(None) => notify("剪贴板里没有图片（先复制一张图再贴）"),
            Err(e) => notify(&format!("读取剪贴板失败：{e}")),
        });
}

fn create_pin(tex: gdk::Texture, audit: &yihu_core::audit::Audit) {
    let win = Window::new();
    win.set_decorated(false);
    win.set_resizable(false);
    win.set_title(Some("一呼 · 贴图"));

    let picture = Picture::new();
    picture.set_paintable(Some(&tex));
    picture.set_can_shrink(true);
    win.set_child(Some(&picture));

    let pw = Rc::new(PinWin {
        win: win.clone(),
        tex: tex.clone(),
        zoom: Cell::new(1.0),
        picture,
    });

    // 按下拖动 = 原生窗口移动（合成器接管；Wayland 下这是合规的移动方式）
    {
        let win_click = win.clone();
        let pw_click = pw.clone();
        let click = GestureClick::new();
        click.set_button(1);
        click.connect_pressed(move |_g, n, x, y| {
            // 双击 = 复制并关
            if n == 2 {
                copy_image(&pw_click);
                return;
            }
            let seat = gtk::prelude::RootExt::display(&pw_click.win).default_seat();
            if let (Some(surface), Some(device)) =
                (win_click.surface(), seat.and_then(|s| s.pointer()))
            {
                // gdk4-rs 未生成 Surface→Toplevel 的 IsA（绑定缺口）；
                // C 层 GdkToplevel* 就是实现该接口的 GdkSurface*，按 C 语义直转
                use gtk::glib::translate::FromGlibPtrBorrow;
                let raw = <gdk::Surface as gtk::glib::translate::ToGlibPtr<
                    *const gdk::ffi::GdkSurface,
                >>::to_glib_none(&surface)
                .0 as *mut gdk::ffi::GdkToplevel;
                let toplevel = unsafe { gdk::Toplevel::from_glib_borrow(raw) };
                toplevel.begin_move(&device, 1, x, y, gtk::gdk::CURRENT_TIME);
            }
        });
        pw.win.add_controller(click);
    }

    // 滚轮缩放
    {
        let pw2 = pw.clone();
        let scroll = EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        scroll.connect_scroll(move |_s, _dx, dy| {
            let z = zoom_step(pw2.zoom.get(), dy < 0.0);
            pw2.zoom.set(z);
            pw2.relayout();
            glib::Propagation::Stop
        });
        pw.win.add_controller(scroll);
    }

    // 键位：Esc 关 · c 复制
    {
        let pw2 = pw.clone();
        let key = gtk::EventControllerKey::new();
        key.connect_key_pressed(move |_k, key, _code, _st| {
            match key.name().unwrap_or_default().as_str() {
                "Escape" => {
                    close_one(&pw2);
                    glib::Propagation::Stop
                }
                "c" | "C" => {
                    copy_image(&pw2);
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        pw.win.add_controller(key);
    }

    OPEN.with(|o| o.borrow_mut().push(pw.clone()));
    pw.relayout();
    win.present();
    audit.record("host", 0, "shot.pin", "grant", "");
}

fn copy_image(pw: &Rc<PinWin>) {
    let Some(display) = gdk::Display::default() else { return };
    display.clipboard().set_texture(&pw.tex);
    notify("已复制贴图内容");
    close_one(pw);
}

fn close_one(pw: &Rc<PinWin>) {
    pw.win.close();
    OPEN.with(|o| o.borrow_mut().retain(|w| !Rc::ptr_eq(w, pw)));
}

fn notify(body: &str) {
    let _ = std::process::Command::new("notify-send")
        .args(["--app-name=一呼", body])
        .spawn();
}
