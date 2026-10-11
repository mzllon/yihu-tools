//! 自绘选区覆盖层（M6 截图引擎 v1）：每显示器一个全屏窗口，冻结帧铺底 +
//! 暗遮罩 + 拖框选区。
//!
//! 键位：**Esc 取消 · Enter/双击 复制 · P 贴图 · S 保存**。确认后全部覆盖层
//! 关闭，选区按缩放因子裁帧出图（[`crate::shotengine::crop_pixels`]）。
//!
//! Wayland 注意：窗口定位归合成器——用 fullscreen_on_monitor 把每个覆盖层
//! 钉在对应显示器上；v1 选区限单屏内（跨屏拖拽不支持，坐标系混合缩放同理，
//! 统一取首屏缩放因子）。遮罩按「选区与显示器交集」分解成四块矩形绘制，
//! 选区在其他显示器时本屏全遮罩。

use gtk::gdk;
use gtk::gdk::prelude::{DisplayExt as _, GdkCairoContextExt as _};
use gtk::prelude::*;
use gtk::glib;
use gtk::{DrawingArea, EventControllerKey, GestureClick, GestureDrag, Window};
use std::cell::RefCell;
use std::rc::Rc;

use crate::shotengine::{self, Rect};

/// 选区判定为误触的最小位移（逻辑像素）
const DRAG_MIN: f64 = 4.0;

struct MonGeom {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    scale: i32,
    monitor: gdk::Monitor,
}

struct OverlayState {
    frame: gtk::gdk_pixbuf::Pixbuf,
    mons: Vec<MonGeom>,
    /// 虚拟逻辑坐标选区（所有覆盖层共享显示）
    selection: RefCell<Option<Rect>>,
    audit: Rc<yihu_core::audit::Audit>,
}

struct OverlayWin {
    win: Window,
}

thread_local! {
    static OPEN: RefCell<Vec<Rc<OverlayWin>>> = const { RefCell::new(Vec::new()) };
    static STATE: RefCell<Option<Rc<OverlayState>>> = const { RefCell::new(None) };
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Action {
    Cancel,
    Copy,
    Pin,
    Save,
    Decode,
}

/// 打开选区覆盖层（主线程调用；frame 由 shotengine 抓帧槽回主循环后传入）
pub fn show(frame: gtk::gdk_pixbuf::Pixbuf, audit: &Rc<yihu_core::audit::Audit>) -> Result<(), String> {
    let display = gdk::Display::default().ok_or("无显示服务")?;
    let monitors = display.monitors();
    let n = monitors.n_items() as usize;
    if n == 0 {
        return Err("无显示器".into());
    }
    let mut mons = Vec::new();
    for i in 0..n {
        let m = monitors
            .item(i as u32)
            .and_downcast::<gdk::Monitor>()
            .ok_or_else(|| format!("显示器 {i} 不可用"))?;
        let g = m.geometry();
        mons.push(MonGeom {
            x: g.x(),
            y: g.y(),
            w: g.width(),
            h: g.height(),
            scale: m.scale_factor(),
            monitor: m,
        });
    }
    let state = Rc::new(OverlayState {
        frame,
        mons,
        selection: RefCell::new(None),
        audit: audit.clone(),
    });
    STATE.with(|s| *s.borrow_mut() = Some(state.clone()));

    for idx in 0..state.mons.len() {
        let win = Window::new();
        win.set_decorated(false);
        win.set_resizable(false);
        win.set_title(Some("一呼 · 区域截图"));

        let area = DrawingArea::new();
        {
            let st = state.clone();
            area.set_draw_func(move |_area, cr, w, h| draw(&st, idx, cr, w, h));
        }
        win.set_child(Some(&area));

        // 拖框选区
        {
            let drag = GestureDrag::new();
            {
                let st = state.clone();
                drag.connect_drag_update(move |g, dx, dy| {
                    let Some((sx, sy)) = g.start_point() else { return };
                    st.selection
                        .borrow_mut()
                        .replace(Rect::from_points(sx, sy, sx + dx, sy + dy));
                    queue_all();
                });
            }
            {
                let st = state.clone();
                drag.connect_drag_end(move |_g, dx, dy| {
                    // 位移过小视为误触：清选区
                    if dx.abs() < DRAG_MIN && dy.abs() < DRAG_MIN {
                        st.selection.borrow_mut().take();
                        queue_all();
                    }
                });
            }
            area.add_controller(drag);
        }
        // 双击 = 复制
        {
            let click = GestureClick::new();
            click.set_button(1);
            click.connect_pressed(|_g, n, _x, _y| {
                if n == 2 {
                    finish(Action::Copy);
                }
            });
            area.add_controller(click);
        }
        // 键位
        {
            let key = EventControllerKey::new();
            key.connect_key_pressed(move |_k, key, _c, _m| {
                let action = match key.name().unwrap_or_default().as_str() {
                    "Escape" => Some(Action::Cancel),
                    "Return" | "KP_Enter" => Some(Action::Copy),
                    "p" | "P" => Some(Action::Pin),
                    "s" | "S" => Some(Action::Save),
                    "q" | "Q" => Some(Action::Decode),
                    _ => None,
                };
                if let Some(a) = action {
                    finish(a);
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
            win.add_controller(key);
        }

        win.fullscreen_on_monitor(&state.mons[idx].monitor);
        OPEN.with(|o| o.borrow_mut().push(Rc::new(OverlayWin { win: win.clone() })));
        win.present();
    }
    state.audit.record("host", 0, "shot.region", "grant", "覆盖层已开");
    Ok(())
}

fn queue_all() {
    OPEN.with(|o| {
        for w in o.borrow().iter() {
            w.win.queue_draw();
        }
    });
}

fn close_all() {
    OPEN.with(|o| {
        for w in o.borrow().iter() {
            w.win.close();
        }
        o.borrow_mut().clear();
    });
    STATE.with(|s| *s.borrow_mut() = None);
}

#[derive(Debug, Clone, Copy)]
struct PxRect(i32, i32, i32, i32);

/// 选区裁剪 + 分发完成动作（主线程；覆盖层先关，防重复触发）
fn finish(action: Action) {
    let st = STATE.with(|s| s.borrow_mut().take());
    close_all();
    let Some(st) = st else { return };

    if action == Action::Cancel {
        st.audit.record("host", 0, "shot.region", "done", "取消");
        return;
    }
    let Some(sel) = *st.selection.borrow() else {
        notify("未框选区域（拖动框选后按 Enter）");
        return;
    };
    let scale = st.mons.first().map(|m| m.scale).unwrap_or(1) as f64;
    let Some(px) = shotengine::crop_pixels(st.frame.width(), st.frame.height(), sel, scale) else {
        notify("选区太小或越界");
        return;
    };
    let pb = shotengine::crop_pixbuf(&st.frame, px);
    let detail = format!("{}x{}", px.2, px.3);
    match action {
        Action::Cancel => unreachable!(),
        Action::Copy => {
            if let Some(display) = gdk::Display::default() {
                let tex = gdk::Texture::for_pixbuf(&pb);
                display.clipboard().set_texture(&tex);
                st.audit
                    .record("host", 0, "shot.region", "done", &format!("复制 {detail}"));
                notify(&format!("已复制截图（{detail}）"));
            }
        }
        Action::Pin => {
            crate::pin::pin_pixbuf(pb, &st.audit);
            st.audit
                .record("host", 0, "shot.region", "done", &format!("贴图 {detail}"));
        }
        Action::Decode => {
            // 选区 → 临时 PNG → zbarimg 识别（线程）→ 文本进剪贴板（回主线程）
            let tmp = std::env::temp_dir().join(format!(
                "yihu-shot-decode-{}.png",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ));
            if let Err(e) = pb.savev(&tmp, "png", &[]) {
                notify(&format!("落盘失败：{e}"));
                return;
            }
            st.audit
                .record("host", 0, "shot.region", "done", &format!("识码 {detail}"));
            std::thread::spawn(move || {
                let (ok, error, data) = crate::shotengine::decode_with_zbar(&tmp);
                if ok {
                    if let Some(text) = data {
                        // 剪贴板必须在主线程；文本（Send）投回主循环
                        glib::MainContext::default().invoke(move || {
                            if let Some(d) = gdk::Display::default() {
                                d.clipboard().set_text(&text);
                            }
                            let preview: String = text.chars().take(60).collect();
                            let _ = std::process::Command::new("notify-send")
                                .args(["--app-name=一呼", &format!("识别到：{preview}")])
                                .spawn();
                        });
                    }
                } else {
                    let _ = std::process::Command::new("notify-send")
                        .args(["--app-name=一呼", &error])
                        .spawn();
                }
            });
        }
        Action::Save => match shotengine::save_pixbuf(&pb) {
            Ok(path) => {
                st.audit.record(
                    "host",
                    0,
                    "shot.region",
                    "done",
                    &format!("保存 {detail} → {}", path.display()),
                );
                notify(&format!("已保存：{}", path.display()));
            }
            Err(e) => {
                st.audit
                    .record("host", 0, "shot.region", "error", &e);
                notify(&format!("保存失败：{e}"));
            }
        },
    }
}

fn notify(body: &str) {
    let _ = std::process::Command::new("notify-send")
        .args(["--app-name=一呼", body])
        .spawn();
}

// ---- 绘制 ------------------------------------------------------------------

fn draw(st: &OverlayState, idx: usize, cr: &gtk::cairo::Context, w: i32, h: i32) {
    let mon = &st.mons[idx];
    let s = mon.scale as f64;
    let w = w as f64;
    let h = h as f64;

    // 1) 底图：本显示器对应的帧切片（物理像素 → 逻辑尺寸绘制）
    let slice = clipped_subpixbuf(
        &st.frame,
        (mon.x as f64 * s) as i32,
        (mon.y as f64 * s) as i32,
        (mon.w as f64 * s) as i32,
        (mon.h as f64 * s) as i32,
    );
    cr.save();
    cr.scale(1.0 / s, 1.0 / s);
    match slice {
        Some(sub) => {
            cr.set_source_pixbuf(&sub, 0.0, 0.0);
            let _ = cr.paint();
        }
        None => {
            let _ = cr.set_source_rgb(0.1, 0.1, 0.1);
            let _ = cr.paint();
        }
    }
    cr.restore();

    // 2) 遮罩（选区与显示器的交集之外）+ 边框 + 提示
    let sel = *st.selection.borrow();
    let Some(r) = sel else {
        let _ = cr.set_source_rgba(0.0, 0.0, 0.0, 0.45);
        let _ = cr.paint();
        draw_center_hint(cr, w, h, "拖动框选 · Enter 复制 · P 贴图 · S 保存 · Q 识码 · Esc 取消");
        return;
    };

    // 选区 → 本窗局部坐标，与窗口求交
    let lx = (r.x - mon.x) as f64;
    let ly = (r.y - mon.y) as f64;
    let ix = lx.max(0.0);
    let iy = ly.max(0.0);
    let ix2 = (lx + r.w as f64).min(w);
    let iy2 = (ly + r.h as f64).min(h);
    let _ = cr.set_source_rgba(0.0, 0.0, 0.0, 0.45);
    if ix2 <= ix || iy2 <= iy {
        // 选区不在本屏：整屏遮罩
        let _ = cr.paint();
        return;
    }
    let _ = cr.rectangle(0.0, 0.0, w, iy);
    let _ = cr.fill();
    let _ = cr.rectangle(0.0, iy, ix, iy2 - iy);
    let _ = cr.fill();
    let _ = cr.rectangle(ix2, iy, w - ix2, iy2 - iy);
    let _ = cr.fill();
    let _ = cr.rectangle(0.0, iy2, w, h - iy2);
    let _ = cr.fill();

    // 边框 + 尺寸提示
    let _ = cr.set_source_rgb(0.26, 0.52, 0.96);
    let _ = cr.set_line_width(2.0);
    let _ = cr.rectangle(ix, iy, ix2 - ix, iy2 - iy);
    let _ = cr.stroke();
    draw_hint(
        cr,
        &format!("{} × {}", r.w, r.h),
        ix,
        (iy2 + 8.0).min(h - 26.0),
    );
}

fn draw_hint(cr: &gtk::cairo::Context, text: &str, x: f64, y: f64) {
    let _ = cr.set_font_size(13.0);
    let (tw, th) = cr
        .text_extents(text)
        .map(|e| (e.width(), e.height()))
        .unwrap_or((60.0, 14.0));
    let _ = cr.set_source_rgba(0.08, 0.08, 0.08, 0.85);
    let _ = cr.rectangle(x, y, tw + 12.0, th + 10.0);
    let _ = cr.fill();
    let _ = cr.set_source_rgba(1.0, 1.0, 1.0, 0.95);
    let _ = cr.move_to(x + 6.0, y + th + 5.0);
    let _ = cr.show_text(text);
}

fn draw_center_hint(cr: &gtk::cairo::Context, w: f64, h: f64, text: &str) {
    let _ = cr.set_font_size(15.0);
    let (tw, th) = cr
        .text_extents(text)
        .map(|e| (e.width(), e.height()))
        .unwrap_or((300.0, 16.0));
    draw_hint(cr, text, (w - tw - 12.0) / 2.0, (h + th) / 2.0 + 4.0);
}

/// 帧内裁剪切片（坐标夹到帧边界）
fn clipped_subpixbuf(frame: &gtk::gdk_pixbuf::Pixbuf, x: i32, y: i32, w: i32, h: i32) -> Option<gtk::gdk_pixbuf::Pixbuf> {
    let fw = frame.width();
    let fh = frame.height();
    if w < 1 || h < 1 || fw < 1 || fh < 1 {
        return None;
    }
    let x = x.clamp(0, fw - 1);
    let y = y.clamp(0, fh - 1);
    let w = w.min(fw - x);
    let h = h.min(fh - y);
    if w < 1 || h < 1 {
        return None;
    }
    Some(frame.new_subpixbuf(x, y, w, h))
}
