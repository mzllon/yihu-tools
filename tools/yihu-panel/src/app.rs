//! 面板窗口：隐藏待命 + nucleo 模糊过滤 + ESC/失焦隐藏。
//!
//! 呼出路径零 IO：窗口、列表、演示数据与主题判定都在启动时完成，
//! 之后 `toggle` 只做 present + grab_focus。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk::{
    gio, gdk, Align, Box as GtkBox, ListView, Orientation, ScrolledWindow, SearchEntry, Window,
};

use crate::service::{Cmd, BUS_NAME};

const MAX_SHOWN: usize = 100;
const DEMO_COUNT: usize = 10_000;

pub fn run_daemon() {
    let (tx, rx) = mpsc::channel::<Cmd>();
    let visible = Arc::new(AtomicBool::new(false));
    // 先于 GTK 声明总线名：占用即静默退出（天然单实例）
    let conn = match crate::service::claim(tx, visible.clone()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("yihu-panel: 总线名不可用，可能已有实例在运行：{e}");
            return;
        }
    };

    let app = gtk::Application::builder()
        .application_id(BUS_NAME)
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let rx_cell = RefCell::new(Some(rx));
    let visible_for_ui = visible.clone();
    app.connect_activate(move |app| {
        let rx = rx_cell.borrow_mut().take().expect("activate 仅发生一次");
        activate(app, rx, visible_for_ui.clone());
    });
    app.run();
    drop(conn); // 连接存活到进程退出
}

fn activate(app: &gtk::Application, rx: mpsc::Receiver<Cmd>, visible: Arc<AtomicBool>) {
    load_css();

    let win = Window::new();
    win.set_application(Some(app));
    win.set_title(Some("一呼"));
    win.set_icon_name(Some("tools.yihu.desktop"));
    win.set_default_size(720, 480);
    win.set_resizable(false);
    win.set_hide_on_close(true);
    win.add_css_class("panel-root");

    // —— 布局：搜索框 + 结果列表 ——
    let card = GtkBox::new(Orientation::Vertical, 8);
    card.add_css_class("panel-card");

    let entry = SearchEntry::new();
    entry.set_placeholder_text(Some("一呼即出：输入以搜索（M2 接入真实能力）"));
    entry.add_css_class("panel-search");
    entry.set_search_delay(0);
    card.append(&entry);

    let store = gio::ListStore::new::<DemoItem>();
    let sel = gtk::SingleSelection::new(Some(store.clone()));
    sel.set_autoselect(true);
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(setup_row);
    factory.connect_bind(bind_row);
    let list = ListView::new(Some(sel.clone()), Some(factory));
    list.add_css_class("panel-list");
    let scroll = ScrolledWindow::new();
    scroll.set_child(Some(&list));
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.set_vexpand(true);
    card.append(&scroll);
    win.set_child(Some(&card));

    // —— 主题跟随：gio::Settings 监听，show 路径零 IO ——
    watch_theme(&win);

    // —— nucleo：注入 1 万条演示数据（验证过滤性能门槛），tick 刷新列表 ——
    let nuc = Rc::new(RefCell::new(build_nucleo()));
    {
        let nuc = nuc.clone();
        entry.connect_search_changed(move |e| {
            let mut nuc = nuc.borrow_mut();
            nuc.pattern.reparse(
                0,
                e.text().as_str(),
                nucleo::pattern::CaseMatching::Smart,
                nucleo::pattern::Normalization::Smart,
                false,
            );
        });
    }
    {
        let nuc = nuc.clone();
        let store = store.clone();
        glib::timeout_add_local(Duration::from_millis(30), move || {
            let status = nuc.borrow_mut().tick(4);
            if status.changed {
                let start = std::time::Instant::now();
                refresh(&store, &nuc.borrow().snapshot());
                if std::env::var_os("YIHU_PANEL_DEBUG").is_some() {
                    eprintln!(
                        "yihu-panel: 过滤+刷新 {:?}（1 万条，cairo 渲染另行逐帧）",
                        start.elapsed()
                    );
                }
            }
            glib::ControlFlow::Continue
        });
    }

    // 回车/双击激活：演示回填搜索词（M2 换成执行命令）
    {
        let store = store.clone();
        let entry = entry.clone();
        list.connect_activate(move |_, pos| {
            if let Some(it) = store.item(pos).and_downcast::<DemoItem>() {
                entry.set_text(&it.title());
                entry.set_position(-1);
            }
        });
    }

    // —— 键盘：ESC 隐藏；搜索框内 ↑/↓ 移动选择 ——
    let ec = gtk::EventControllerKey::new();
    {
        let win = win.clone();
        let visible = visible.clone();
        ec.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape {
                hide_panel(&win, &visible);
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
    }
    win.add_controller(ec);
    entry.connect_stop_search({
        let win = win.clone();
        let visible = visible.clone();
        move |_| hide_panel(&win, &visible)
    });
    {
        let sel = sel.clone();
        let list = list.clone();
        let ec_entry = gtk::EventControllerKey::new();
        ec_entry.connect_key_pressed(move |_, key, _, _| {
            let n = sel.model().map(|m| m.n_items()).unwrap_or(0);
            if n == 0 {
                return glib::Propagation::Proceed;
            }
            let next = match key {
                gdk::Key::Down => (sel.selected() as i64 + 1).min(n as i64 - 1),
                gdk::Key::Up => (sel.selected() as i64 - 1).max(0),
                _ => return glib::Propagation::Proceed,
            };
            sel.set_selected(next as u32);
            list.scroll_to(next as u32, gtk::ListScrollFlags::empty(), None);
            glib::Propagation::Stop
        });
        entry.add_controller(ec_entry);
    }

    // —— 失焦自动隐藏（隐藏本身引发的失焦不重复处理）——
    {
        let win = win.clone();
        let visible = visible.clone();
        win.connect_notify(Some("is-active"), move |w, _| {
            if !w.is_active() && w.is_visible() {
                hide_panel(w, &visible);
            }
        });
    }

    // —— 命令消费：zbus → mpsc → 主循环（托盘既有惯例）——
    {
        let win = win.clone();
        let entry = entry.clone();
        let visible = visible.clone();
        let app = app.clone();
        glib::timeout_add_local(Duration::from_millis(50), move || {
            let mut quit = false;
            while let Ok(cmd) = rx.try_recv() {
                match cmd {
                    Cmd::Toggle => toggle_panel(&win, &entry, &visible),
                    Cmd::Show => show_panel(&win, &entry, &visible),
                    Cmd::Hide => hide_panel(&win, &visible),
                    Cmd::Quit => quit = true,
                }
            }
            if quit {
                app.quit();
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
    }

    // 启动即隐藏待命：不 present
}

fn toggle_panel(win: &Window, entry: &SearchEntry, visible: &AtomicBool) {
    if win.is_visible() {
        hide_panel(win, visible);
    } else {
        show_panel(win, entry, visible);
    }
}

fn show_panel(win: &Window, entry: &SearchEntry, visible: &AtomicBool) {
    win.present();
    entry.grab_focus();
    visible.store(true, Ordering::Relaxed);
}

fn hide_panel(win: &Window, visible: &AtomicBool) {
    if !win.is_visible() {
        return;
    }
    win.set_visible(false);
    visible.store(false, Ordering::Relaxed);
    // 归还给 OS，守住待命 RSS（长驻进程堆回收惯例）
    unsafe { libc::malloc_trim(0) };
}

fn watch_theme(win: &Window) {
    let apply = |w: &Window, scheme: &str| {
        w.remove_css_class("dark");
        w.remove_css_class("light");
        w.add_css_class(if scheme == "prefer-dark" { "dark" } else { "light" });
    };
    let s = gio::Settings::new("org.gnome.desktop.interface");
    apply(win, &s.string("color-scheme"));
    {
        let win = win.clone();
        s.connect_changed(Some("color-scheme"), move |s, _| {
            apply(&win, &s.string("color-scheme"));
        });
    }
}

/// 等到 worker 彻底空闲（连续两拍无任务），避免基准过早读快照
fn settle<T: Send + Sync + 'static>(nuc: &mut nucleo::Nucleo<T>) {
    let mut quiet = 0;
    loop {
        quiet = if nuc.tick(20).running { 0 } else { quiet + 1 };
        if quiet >= 2 {
            return;
        }
    }
}

/// 无头基准（YIHU_PANEL_BENCH=1）：注入 1 万条演示数据，测典型查询的
/// 匹配耗时后退出。用于验收「万条过滤 <16ms」门槛，不需要显示服务。
pub fn run_bench() {
    let start = std::time::Instant::now();
    let mut nuc = build_nucleo();
    settle(&mut nuc);
    println!(
        "注入 {} 条并完成首次匹配: {:?}",
        DEMO_COUNT,
        start.elapsed()
    );
    for q in ["终端", "编辑", "编辑器 0", "编辑 00", "截图 9", "终端 12", "yihu"] {
        nuc.pattern.reparse(
            0,
            q,
            nucleo::pattern::CaseMatching::Smart,
            nucleo::pattern::Normalization::Smart,
            false,
        );
        let t = std::time::Instant::now();
        settle(&mut nuc);
        let snap = nuc.snapshot();
        let samples: Vec<String> = snap
            .matched_items(0..3.min(snap.matched_item_count()))
            .map(|it| it.data.0.clone())
            .collect();
        println!(
            "查询 {q:?}: {:?}，命中 {}，样例 {samples:?}",
            t.elapsed(),
            snap.matched_item_count()
        );
    }
}

fn build_nucleo() -> nucleo::Nucleo<(String, String)> {
    let notify: Arc<dyn Fn() + Sync + Send> = Arc::new(|| {});
    let nuc = nucleo::Nucleo::new(nucleo::Config::DEFAULT, notify, Some(2), 1);
    let inj = nuc.injector();
    // 演示数据：10 组词 × 每组 1000 条，覆盖中文与英文前缀
    const BASE: &[&str] = &[
        "终端", "编辑器", "文件管理", "音乐播放", "截图工具", "计算器", "日历",
        "邮件", "浏览器", "笔记",
    ];
    let mut n = 0usize;
    'gen: for base in BASE {
        for i in 0..1000u32 {
            let title = format!("{base} {i:03}");
            let sub = format!("演示条目 · 候选 {n}");
            inj.push((title, sub), |item, cols| cols[0] = item.0.as_str().into());
            n += 1;
            if n >= DEMO_COUNT {
                break 'gen;
            }
        }
    }
    nuc
}

fn refresh(store: &gio::ListStore, snap: &nucleo::Snapshot<(String, String)>) {
    let n = snap.matched_item_count().min(MAX_SHOWN as u32);
    store.remove_all();
    for it in snap.matched_items(0..n) {
        store.append(&DemoItem::new(&it.data.0, &it.data.1));
    }
}

fn setup_row(_: &gtk::SignalListItemFactory, obj: &glib::Object) {
    let li = obj.downcast_ref::<gtk::ListItem>().expect("ListItem");
    let row = GtkBox::new(Orientation::Horizontal, 12);
    let icon = GtkBox::new(Orientation::Vertical, 0);
    icon.add_css_class("row-icon");
    icon.set_valign(Align::Center);
    let col = GtkBox::new(Orientation::Vertical, 2);
    col.set_valign(Align::Center);
    let title = gtk::Label::new(None);
    title.add_css_class("row-title");
    title.set_halign(Align::Start);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let sub = gtk::Label::new(None);
    sub.add_css_class("caption-sm");
    sub.set_halign(Align::Start);
    sub.set_ellipsize(gtk::pango::EllipsizeMode::End);
    col.append(&title);
    col.append(&sub);
    row.append(&icon);
    row.append(&col);
    li.set_child(Some(&row));
    // 安全性：键 "yihu-row" 只在本文件 setup/bind 中使用，类型恒为 Row
    unsafe { li.set_data("yihu-row", Row { title, sub }) };
}

fn bind_row(_: &gtk::SignalListItemFactory, obj: &glib::Object) {
    let li = obj.downcast_ref::<gtk::ListItem>().expect("ListItem");
    let item = li.item().and_downcast::<DemoItem>().expect("DemoItem");
    // 安全性：同 set_data，键与类型由本文件保证
    let row: &Row = unsafe {
        let ptr = li.data::<Row>("yihu-row").expect("row data");
        ptr.as_ref()
    };
    row.title.set_text(&item.title());
    row.sub.set_text(&item.subtitle());
}

struct Row {
    title: gtk::Label,
    sub: gtk::Label,
}

// ---- 演示条目 GObject ----

mod imp {
    use std::cell::RefCell;

    use gtk::glib::{self, Properties};
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;

    #[derive(Properties, Default)]
    #[properties(wrapper_type = super::DemoItem)]
    pub struct DemoItemImp {
        #[property(get, set)]
        pub title: RefCell<String>,
        #[property(get, set)]
        pub subtitle: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for DemoItemImp {
        const NAME: &'static str = "YihuPanelDemoItem";
        type Type = super::DemoItem;
    }

    #[glib::derived_properties]
    impl ObjectImpl for DemoItemImp {}
}

glib::wrapper! {
    pub struct DemoItem(ObjectSubclass<imp::DemoItemImp>);
}

impl DemoItem {
    fn new(title: &str, subtitle: &str) -> Self {
        glib::Object::builder()
            .property("title", title)
            .property("subtitle", subtitle)
            .build()
    }
}

fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(include_str!("style.css"));
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}
