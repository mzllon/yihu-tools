//! 播放条上的圆形封面盘：播放时封面呈圆形并顺时针旋转（唱片效果）。
//!
//! 自定义 Widget 在 snapshot 阶段完成圆形裁剪与旋转变换，
//! 旋转由 33ms 定时器驱动角度、仅重绘本控件；停止时静止归零。

use gtk::gdk;
use gtk::glib;
use gtk::gsk;
use gtk::subclass::prelude::*;
use gtk::prelude::*;
use std::cell::RefCell;
use std::time::Duration;

/// 每次 tick 的旋转角（度）与定时周期。
const SPIN_STEP: f64 = 4.0;
const TICK_MS: u64 = 33;

#[derive(Debug, Default)]
struct DiscInner {
    texture: Option<gdk::Texture>,
    /// 无封面时的兜底图标名。
    icon: String,
    angle: f64,
    spinning: bool,
    tick: Option<glib::SourceId>,
}

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct CoverDisc {
        pub(super) inner: RefCell<DiscInner>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CoverDisc {
        const NAME: &'static str = "YihuCoverDisc";
        type Type = super::CoverDisc;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for CoverDisc {
        fn dispose(&self) {
            if let Some(tick) = self.inner.borrow_mut().tick.take() {
                tick.remove();
            }
        }
    }

    impl WidgetImpl for CoverDisc {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let (w, h) = (widget.width() as f32, widget.height() as f32);
            if w <= 0.0 || h <= 0.0 {
                return;
            }
            let size = w.min(h);
            let inner = self.inner.borrow();
            snapshot.save();
            snapshot.translate(&gtk::graphene::Point::new(w / 2.0, h / 2.0));
            if inner.spinning {
                snapshot.rotate(inner.angle as f32);
            }
            let rect = gtk::graphene::Rect::new(-size / 2.0, -size / 2.0, size, size);
            let rounded = gsk::RoundedRect::from_rect(rect, size / 2.0);
            snapshot.push_rounded_clip(&rounded);
            match inner.texture.as_ref() {
                Some(texture) => {
                    snapshot.append_texture(texture, &rect);
                }
                None => {
                    // 无封面时的底色圆盘
                    let disc = gdk::RGBA::new(0.28, 0.28, 0.28, 1.0);
                    snapshot.append_color(&disc, &rect);
                }
            }
            snapshot.pop();
            snapshot.restore();
        }
    }
}

glib::wrapper! {
    pub struct CoverDisc(ObjectSubclass<imp::CoverDisc>) @extends gtk::Widget;
}

impl CoverDisc {
    pub fn new(fallback_icon: &str) -> Self {
        let this: Self = glib::Object::builder().build();
        this.imp().inner.borrow_mut().icon = fallback_icon.to_owned();
        this
    }

    pub fn set_texture(&self, texture: Option<&gdk::Texture>) {
        self.imp().inner.borrow_mut().texture = texture.cloned();
        self.queue_draw();
    }

    /// 播放中开转，停止即静止并归零角度。
    pub fn set_spinning(&self, spinning: bool) {
        let imp = self.imp();
        {
            let mut inner = imp.inner.borrow_mut();
            if inner.spinning == spinning {
                return;
            }
            inner.spinning = spinning;
            if spinning {
                inner.angle = 0.0;
            } else if let Some(tick) = inner.tick.take() {
                tick.remove();
            }
        }
        if spinning {
            self.start_tick();
        }
        self.queue_draw();
    }

    fn start_tick(&self) {
        let imp = self.imp();
        if imp.inner.borrow().tick.is_some() {
            return;
        }
        let this = self.clone();
        let tick = glib::timeout_add_local(Duration::from_millis(TICK_MS), move || {
            let inner = &this.imp().inner;
            let mut state = inner.borrow_mut();
            if !state.spinning {
                state.tick = None;
                return glib::ControlFlow::Break;
            }
            state.angle = (state.angle + SPIN_STEP) % 360.0;
            drop(state);
            this.queue_draw();
            glib::ControlFlow::Continue
        });
        imp.inner.borrow_mut().tick = Some(tick);
    }
}
