//! 面板窗口：隐藏待命 + nucleo 模糊过滤 + ESC/失焦隐藏。
//!
//! 呼出路径零 IO：应用枚举、历史、主题判定都在启动时完成，
//! 之后 `toggle` 只做 present + grab_focus。
//! 条目分三类：`app`（GIO 应用启动）/ `cap`（一呼内置能力）/ `calc`（算式结果）。

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use gtk::glib;
use gtk::prelude::*;
use gtk::{
    gio, gdk, Align, Box as GtkBox, Label, ListView, Orientation, ScrolledWindow, SearchEntry,
    Window,
};

use crate::calc;
use crate::service::{Cmd, BUS_NAME};

const MAX_SHOWN: usize = 100;

/// 一条可展示/可执行的条目（nucleo 按标题匹配，其余字段随条目带回）
#[derive(Clone)]
struct PanelEntry {
    title: String,
    subtitle: String,
    icon_spec: String,
    kind: &'static str,
    payload: String,
}

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

/// 无头基准（YIHU_PANEL_BENCH=1）：枚举真实应用、测典型查询耗时后退出。
pub fn run_bench() {
    let t0 = Instant::now();
    let apps = collect_apps();
    let t_apps = t0.elapsed();
    let history = mt_core::panel::History::load();
    let entries = build_entries(&history, apps);
    let total = entries.len();
    let mut nuc = build_nucleo(&entries);
    settle(&mut nuc);
    println!(
        "枚举应用 {} 条，总条目 {} 条，注入并完成首次匹配: {:?}",
        total,
        total,
        t0.elapsed()
    );
    println!("应用枚举本身耗时: {t_apps:?}");
    for q in ["终端", "设置", "firefox", "文件", "切换到深色"] {
        nuc.pattern.reparse(
            0,
            q,
            nucleo::pattern::CaseMatching::Smart,
            nucleo::pattern::Normalization::Smart,
            false,
        );
        let t = Instant::now();
        settle(&mut nuc);
        let snap = nuc.snapshot();
        let samples: Vec<String> = snap
            .matched_items(0..3.min(snap.matched_item_count()))
            .map(|it| it.data.title.clone())
            .collect();
        println!(
            "查询 {q:?}: {:?}，命中 {}，样例 {samples:?}",
            t.elapsed(),
            snap.matched_item_count()
        );
    }
}

fn activate(app: &gtk::Application, rx: mpsc::Receiver<Cmd>, visible: Arc<AtomicBool>) {
    load_css();

    let win = Window::new();
    win.set_application(Some(app));
    win.set_title(Some("一呼"));
    win.set_icon_name(Some("tools.yihu.desktop"));
    win.set_default_size(720, 440);
    win.set_resizable(false);
    win.set_hide_on_close(true);
    win.add_css_class("panel-root");

    // —— 布局：搜索框 + 结果列表 ——
    let card = GtkBox::new(Orientation::Vertical, 8);
    card.add_css_class("panel-card");

    let entry = SearchEntry::new();
    entry.set_placeholder_text(Some("一呼即出：搜索应用 / 能力 / 算式"));
    entry.add_css_class("panel-search");
    entry.set_search_delay(0);
    card.append(&entry);

    // —— 数据：能力 + 已安装应用（启动时枚举，呼出路径零 IO）——
    let history = Rc::new(RefCell::new(mt_core::panel::History::load()));
    let center = sibling("yihu");
    let entries = Rc::new(RefCell::new(build_entries(
        &history.borrow(),
        collect_apps(),
    )));
    let nuc = Rc::new(RefCell::new(build_nucleo(&entries.borrow())));

    let store = gio::ListStore::new::<PanelItem>();
    let sel = gtk::SingleSelection::new(Some(store.clone()));
    // 不自动选中：默认集全是标题/胶囊行，高亮没有意义；
    // 搜索结果出现时由刷新逻辑选中第一个可激活行
    sel.set_autoselect(false);
    let factory = gtk::SignalListItemFactory::new();
    {
        let win = win.clone();
        let visible = visible.clone();
        let history = history.clone();
        let center = center.clone();
        let entries = entries.clone();
        factory.connect_setup(setup_row);
        factory.connect_bind(move |f, obj| {
            bind_row(f, obj, &win, &visible, &history, &center, &entries)
        });
    }
    let list = ListView::new(Some(sel.clone()), Some(factory));
    list.add_css_class("panel-list");
    // 启动器惯例：单击即激活（默认是双击）
    list.set_single_click_activate(true);
    let scroll = ScrolledWindow::new();
    scroll.set_child(Some(&list));
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.set_vexpand(true);
    card.append(&scroll);
    win.set_child(Some(&card));

    // —— 主题跟随：gio::Settings 监听，show 路径零 IO ——
    watch_theme(&win);

    let query = Rc::new(RefCell::new(String::new()));
    let calc_row: Rc<RefCell<Option<PanelEntry>>> = Rc::new(RefCell::new(None));
    let dirty = Cell::new(true);

    {
        let nuc = nuc.clone();
        let query = query.clone();
        let calc_row = calc_row.clone();
        let dirty = dirty.clone();
        entry.connect_search_changed(move |e| {
            let text = e.text().to_string();
            *calc_row.borrow_mut() = calc_entry(&text);
            *query.borrow_mut() = text;
            let mut nuc = nuc.borrow_mut();
            nuc.pattern.reparse(
                0,
                &query.borrow(),
                nucleo::pattern::CaseMatching::Smart,
                nucleo::pattern::Normalization::Smart,
                false,
            );
            dirty.set(true);
        });
    }

    // —— 刷新：nucleo 结果 / 分组默认集，计算行置顶 ——
    {
        let nuc = nuc.clone();
        let entries = entries.clone();
        let history = history.clone();
        let query = query.clone();
        let calc_row = calc_row.clone();
        let dirty = dirty.clone();
        let store = store.clone();
        let sel = sel.clone();
        let win = win.clone();
        glib::timeout_add_local(Duration::from_millis(30), move || {
            let changed = nuc.borrow_mut().tick(4).changed;
            if !(changed || dirty.get()) {
                return glib::ControlFlow::Continue;
            }
            dirty.set(false);
            let start = Instant::now();
            let mut rows: Vec<PanelEntry> = Vec::with_capacity(MAX_SHOWN);
            if let Some(c) = calc_row.borrow().clone() {
                rows.push(c);
            }
            let q = query.borrow().clone();
            if q.is_empty() {
                // 分组默认集：最近 / 快捷能力(胶囊) / 常用应用
                for e in default_rows(&history.borrow(), &entries.borrow()) {
                    if rows.len() >= MAX_SHOWN {
                        break;
                    }
                    rows.push(e.clone());
                }
            } else {
                let nuc = nuc.borrow();
                let snap = nuc.snapshot();
                let n = snap
                    .matched_item_count()
                    .min((MAX_SHOWN - rows.len()) as u32);
                for it in snap.matched_items(0..n) {
                    rows.push(it.data.clone());
                }
            }
            if rows.is_empty() {
                rows.push(PanelEntry {
                    title: "无匹配结果".into(),
                    subtitle: "试试其他关键词".into(),
                    icon_spec: "edit-find-symbolic".into(),
                    kind: "none",
                    payload: String::new(),
                });
            }
            // 高度随内容自适应，封顶 MAX_WINDOW_H
            let height = estimated_height(&rows);
            let kinds: Vec<&str> = rows.iter().map(|e| e.kind).collect();
            store.remove_all();
            for e in rows {
                store.append(&PanelItem::from_entry(&e));
            }
            win.set_default_size(720, height);
            // 选中项若落在标题/胶囊等不可激活行上，挪到第一个可激活行
            if !selectable_at(&store, sel.selected()) {
                if let Some(p) = first_selectable(&store) {
                    sel.set_selected(p);
                }
            }
            if std::env::var_os("YIHU_PANEL_DEBUG").is_some() {
                eprintln!(
                    "yihu-panel: 行 {kinds:?}，目标高 {height}px，实际 {}x{}，刷新 {:?}",
                    win.width(),
                    win.height(),
                    start.elapsed()
                );
            }
            glib::ControlFlow::Continue
        });
    }

    // —— 激活：按 kind 分发真实动作（回车 / 单击 / 双击）——
    {
        let win = win.clone();
        let visible = visible.clone();
        let history = history.clone();
        let center = center.clone();
        let store = store.clone();
        list.connect_activate(move |_, pos| {
            dispatch_item(&win, &visible, &history, &center, &store, pos);
        });
    }
    {
        // 回车时焦点在搜索框，列表收不到按键——在搜索框上激活当前选中项
        let win = win.clone();
        let visible = visible.clone();
        let history = history.clone();
        let center = center.clone();
        let store = store.clone();
        let sel = sel.clone();
        entry.connect_activate(move |_| {
            let pos = sel.selected();
            if sel.model().map(|m| m.n_items()).unwrap_or(0) > pos {
                dispatch_item(&win, &visible, &history, &center, &store, pos);
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
        let store = store.clone();
        let ec_entry = gtk::EventControllerKey::new();
        ec_entry.connect_key_pressed(move |_, key, _, _| {
            let n = sel.model().map(|m| m.n_items()).unwrap_or(0);
            if n == 0 {
                return glib::Propagation::Proceed;
            }
            let dir: i64 = match key {
                gdk::Key::Down => 1,
                gdk::Key::Up => -1,
                _ => return glib::Propagation::Proceed,
            };
            // 从当前选中项出发，跳过标题/胶囊等不可激活行
            let mut cur = sel.selected() as i64;
            if cur < 0 || cur >= n as i64 {
                cur = if dir > 0 { -1 } else { n as i64 };
            }
            let mut next = cur.clamp(0, n as i64 - 1);
            let mut cand = cur;
            for _ in 0..n {
                cand += dir;
                if cand < 0 || cand >= n as i64 {
                    break;
                }
                if selectable_at(&store, cand as u32) {
                    next = cand;
                    break;
                }
            }
            // 默认集可能整屏都是标题/胶囊行：没有可激活行就不动选择
            if selectable_at(&store, next as u32) {
                sel.set_selected(next as u32);
                list.scroll_to(next as u32, gtk::ListScrollFlags::empty(), None);
            }
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
                    Cmd::Toggle => toggle_panel(&win, &entry, &visible, &dirty),
                    Cmd::Show => show_panel(&win, &entry, &visible, &dirty),
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

/// 按 kind 分发条目动作：app 启动 / cap 执行 / calc 复制。
fn dispatch_item(
    win: &Window,
    visible: &AtomicBool,
    history: &Rc<RefCell<mt_core::panel::History>>,
    center: &PathBuf,
    store: &gio::ListStore,
    pos: u32,
) {
    let Some(obj) = store.item(pos) else {
        return;
    };
    let Some(item) = obj.downcast_ref::<PanelItem>() else {
        return;
    };
    let kind = item.kind();
    let payload = item.payload().to_string();
    // 重建窗口期的竞态防护：读到空 payload 的条目直接忽略
    if payload.is_empty() && kind != "calc" {
        return;
    }
    if kind == "calc" {
        win.clipboard().set_text(&payload);
        item.set_subtitle("已复制".to_string());
        record(history, "calc");
        return;
    }
    perform_entry(
        win,
        visible,
        history,
        center,
        &PanelEntry {
            title: item.title().to_string(),
            subtitle: item.subtitle().to_string(),
            icon_spec: item.icon_spec().to_string(),
            kind: if kind == "app" { "app" } else { "cap" },
            payload,
        },
    );
}

fn toggle_panel(win: &Window, entry: &SearchEntry, visible: &AtomicBool, dirty: &Cell<bool>) {
    if win.is_visible() {
        hide_panel(win, visible);
    } else {
        show_panel(win, entry, visible, dirty);
    }
}

fn show_panel(win: &Window, entry: &SearchEntry, visible: &AtomicBool, dirty: &Cell<bool>) {
    win.present();
    entry.grab_focus();
    visible.store(true, Ordering::Relaxed);
    // 呼出后立即按当前内容校正一次高度
    dirty.set(true);
    if std::env::var_os("YIHU_PANEL_DEBUG").is_some() {
        let w = win.clone();
        glib::timeout_add_local_once(Duration::from_millis(200), move || {
            eprintln!("yihu-panel: 呼出后实际尺寸 {}x{}", w.width(), w.height());
        });
    }
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

// ---- 提供者：能力 + 应用 ----

/// 一呼内置能力（进程内执行；payload 见 run_cap）
fn capabilities() -> Vec<PanelEntry> {
    vec![
        PanelEntry {
            title: "切换到深色模式".into(),
            subtitle: "一呼 · 能力".into(),
            icon_spec: "weather-clear-night-symbolic".into(),
            kind: "cap",
            payload: "theme:dark".into(),
        },
        PanelEntry {
            title: "切换到浅色模式".into(),
            subtitle: "一呼 · 能力".into(),
            icon_spec: "weather-clear-symbolic".into(),
            kind: "cap",
            payload: "theme:light".into(),
        },
        PanelEntry {
            title: "打开一呼中心".into(),
            subtitle: "一呼 · 能力".into(),
            icon_spec: "tools.yihu.desktop".into(),
            kind: "cap",
            payload: "center".into(),
        },
        PanelEntry {
            title: "打开广播页".into(),
            subtitle: "一呼 · 能力".into(),
            icon_spec: "applications-multimedia-symbolic".into(),
            kind: "cap",
            payload: "page:radio".into(),
        },
        PanelEntry {
            title: "打开主题切换页".into(),
            subtitle: "一呼 · 能力".into(),
            icon_spec: "night-light-symbolic".into(),
            kind: "cap",
            payload: "page:autodark".into(),
        },
    ]
}

/// 枚举当前用户可见的已安装应用（排除 NoDisplay/Hidden 等）。
/// 仅在启动时调用，呼出路径不做任何枚举。
fn collect_apps() -> Vec<PanelEntry> {
    gio::AppInfo::all()
        .into_iter()
        .filter(|app| app.should_show())
        .filter_map(|app| {
            let id = app.id()?.to_string();
            let title = app.display_name().to_string();
            if title.is_empty() {
                return None;
            }
            let icon_spec = app.icon().and_then(icon_spec_of).unwrap_or_default();
            Some(PanelEntry {
                title,
                subtitle: "应用".into(),
                icon_spec,
                kind: "app",
                payload: id,
            })
        })
        .collect()
}

fn icon_spec_of(icon: gio::Icon) -> Option<String> {
    if let Some(t) = icon.downcast_ref::<gio::ThemedIcon>() {
        t.names().first().map(|s| s.to_string())
    } else if let Some(f) = icon.downcast_ref::<gio::FileIcon>() {
        f.file().path().map(|p| p.to_string_lossy().into_owned())
    } else {
        None
    }
}

fn gicon_for_spec(spec: &str) -> gio::Icon {
    if spec.contains('/') {
        gio::FileIcon::new(&gio::File::for_path(spec)).upcast()
    } else if spec.is_empty() {
        gio::ThemedIcon::new("application-x-executable-symbolic").upcast()
    } else {
        gio::ThemedIcon::new(spec).upcast()
    }
}

/// 主列表：能力固定在头部，应用按使用次数/最近时间/名称排序。
fn build_entries(history: &mt_core::panel::History, apps: Vec<PanelEntry>) -> Vec<PanelEntry> {
    let mut apps = apps;
    apps.sort_by(|a, b| {
        let (ca, cb) = (history.count_of(&a.payload), history.count_of(&b.payload));
        let (la, lb) = (history.last_of(&a.payload), history.last_of(&b.payload));
        cb.cmp(&ca).then(lb.cmp(&la)).then(a.title.cmp(&b.title))
    });
    let mut all = capabilities();
    all.extend(apps);
    all
}

fn build_nucleo(entries: &[PanelEntry]) -> nucleo::Nucleo<PanelEntry> {
    let notify: Arc<dyn Fn() + Sync + Send> = Arc::new(|| {});
    let nuc = nucleo::Nucleo::new(nucleo::Config::DEFAULT, notify, Some(2), 1);
    let inj = nuc.injector();
    for e in entries {
        let e = e.clone();
        inj.push(e, |item, cols| cols[0] = item.title.as_str().into());
    }
    nuc
}

// ---- 分组默认集：最近 / 快捷能力(胶囊) / 常用应用 ----

fn header_row(t: &str) -> PanelEntry {
    PanelEntry {
        title: t.to_string(),
        subtitle: String::new(),
        icon_spec: String::new(),
        kind: "header",
        payload: String::new(),
    }
}

/// 最近使用条目：历史按时间排序后映射回条目（应用/能力），取前 n 个。
fn recent_entries(
    history: &mt_core::panel::History,
    entries: &[PanelEntry],
    n: usize,
) -> Vec<PanelEntry> {
    let mut hs = history.entries.clone();
    hs.sort_by(|a, b| b.last.cmp(&a.last).then(b.count.cmp(&a.count)));
    hs.iter()
        .filter_map(|h| {
            entries
                .iter()
                .find(|e| format!("{}:{}", e.kind, e.payload) == h.id)
                .cloned()
        })
        .take(n)
        .collect()
}

/// 空输入时的分组默认集：最近（胶囊，应用+能力按时间，最多 4 个）→
/// 快捷能力（胶囊 5 个）。全部为点击即触发的按钮，无普通行。
fn default_rows(history: &mt_core::panel::History, entries: &[PanelEntry]) -> Vec<PanelEntry> {
    let mut rows = Vec::new();

    let recent = recent_entries(history, entries, 3);
    if !recent.is_empty() {
        rows.push(header_row("最近"));
        rows.push(PanelEntry {
            title: String::new(),
            subtitle: String::new(),
            icon_spec: String::new(),
            kind: "recent_chips",
            payload: String::new(),
        });
    }

    rows.push(header_row("快捷能力"));
    rows.push(PanelEntry {
        title: String::new(),
        subtitle: String::new(),
        icon_spec: String::new(),
        kind: "chips",
        payload: String::new(),
    });
    rows
}

/// 快捷能力胶囊：一行小按钮，点击直接触发（无需选中回车）。
fn chip_buttons(
    win: &Window,
    visible: &Arc<AtomicBool>,
    history: &Rc<RefCell<mt_core::panel::History>>,
    center: &PathBuf,
) -> Vec<gtk::Button> {
    const CHIPS: &[(&str, &str, &str)] = &[
        ("theme:dark", "深色", "weather-clear-night-symbolic"),
        ("theme:light", "浅色", "weather-clear-symbolic"),
        ("center", "中心", "tools.yihu.desktop"),
        ("page:radio", "广播", "applications-multimedia-symbolic"),
        ("page:autodark", "主题页", "night-light-symbolic"),
    ];
    CHIPS
        .iter()
        .map(|(payload, label, icon)| {
            let b = gtk::Button::new();
            b.add_css_class("panel-chip");
            let bx = GtkBox::new(Orientation::Horizontal, 6);
            let img = gtk::Image::from_icon_name(icon);
            img.set_pixel_size(16);
            bx.append(&img);
            bx.append(&Label::new(Some(label)));
            b.set_child(Some(&bx));
            let win = win.clone();
            let visible = visible.clone();
            let history = history.clone();
            let center = center.clone();
            let payload = payload.to_string();
            b.connect_clicked(move |_| {
                run_cap(&payload, &center);
                record(&history, &format!("cap:{payload}"));
                hide_panel(&win, &visible);
            });
            b
        })
        .collect()
}

/// 最近胶囊：最近用过的应用/能力（按时间，最多 4 个），点击即触发。
fn recent_chip_buttons(
    win: &Window,
    visible: &Arc<AtomicBool>,
    history: &Rc<RefCell<mt_core::panel::History>>,
    center: &PathBuf,
    entries: &Rc<RefCell<Vec<PanelEntry>>>,
) -> Vec<gtk::Button> {
    let recent = recent_entries(&history.borrow(), &entries.borrow(), 4);
    if std::env::var_os("YIHU_PANEL_DEBUG").is_some() {
        eprintln!(
            "yihu-panel: 最近胶囊 {} 个：{:?}",
            recent.len(),
            recent.iter().map(|e| &e.title).collect::<Vec<_>>()
        );
    }
    recent
        .iter()
        .map(|e| {
            let b = gtk::Button::new();
            b.add_css_class("panel-chip");
            let bx = GtkBox::new(Orientation::Horizontal, 6);
            let img = gtk::Image::new();
            img.set_pixel_size(16);
            img.set_from_gicon(&gicon_for_spec(&e.icon_spec));
            bx.append(&img);
            let lbl = Label::new(Some(&e.title));
            lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            lbl.set_max_width_chars(12);
            bx.append(&lbl);
            b.set_child(Some(&bx));
            let e = e.clone();
            let win = win.clone();
            let visible = visible.clone();
            let history = history.clone();
            let center = center.clone();
            b.connect_clicked(move |_| {
                perform_entry(&win, &visible, &history, &center, &e);
            });
            b
        })
        .collect()
}

/// 执行条目动作：app 启动 / cap 执行；成功后记历史并收起面板。
fn perform_entry(
    win: &Window,
    visible: &AtomicBool,
    history: &Rc<RefCell<mt_core::panel::History>>,
    center: &PathBuf,
    e: &PanelEntry,
) {
    match e.kind {
        "app" => {
            if launch_app(&e.payload) {
                record(history, &format!("app:{}", e.payload));
                hide_panel(win, visible);
            }
        }
        "cap" => {
            run_cap(&e.payload, center);
            record(history, &format!("cap:{}", e.payload));
            hide_panel(win, visible);
        }
        _ => {}
    }
}

/// 窗口高度：随内容行数自适应，封顶 520（呼出路径只是改一个数值，无 IO）。
fn estimated_height(rows: &[PanelEntry]) -> i32 {
    const MAX_WINDOW_H: i32 = 520;
    let content: i32 = rows
        .iter()
        .map(|e| match e.kind {
            "header" => 30,
            "chips" | "recent_chips" => 50,
            _ => 58,
        })
        .sum();
    // 搜索区 + 上下留白
    (76 + content + 14).clamp(240, MAX_WINDOW_H)
}

fn selectable_at(store: &gio::ListStore, pos: u32) -> bool {
    store
        .item(pos)
        .and_downcast::<PanelItem>()
        .map(|i| matches!(i.kind().as_str(), "app" | "cap" | "calc"))
        .unwrap_or(false)
}

fn first_selectable(store: &gio::ListStore) -> Option<u32> {
    (0..store.n_items()).find(|p| selectable_at(store, *p))
}

/// 输入若是完整算式（含至少一个运算符），生成置顶的计算结果条目。
fn calc_entry(text: &str) -> Option<PanelEntry> {
    if text.len() < 3 || !text.chars().any(|c| "+-*/%^".contains(c)) {
        return None;
    }
    let v = calc::evaluate(text)?;
    let r = calc::format_result(v);
    Some(PanelEntry {
        title: format!("= {r}"),
        subtitle: "回车复制结果".into(),
        icon_spec: "accessories-calculator-symbolic".into(),
        kind: "calc",
        payload: r,
    })
}

fn launch_app(desktop_id: &str) -> bool {
    match gio::DesktopAppInfo::new(desktop_id) {
        Some(info) => match info.launch(&[], None::<&gio::AppLaunchContext>) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("yihu-panel: 启动 {desktop_id} 失败：{e}");
                false
            }
        },
        None => {
            eprintln!("yihu-panel: 未找到桌面条目 {desktop_id}");
            false
        }
    }
}

fn run_cap(payload: &str, center: &PathBuf) {
    match payload {
        "theme:dark" => {
            std::thread::spawn(|| {
                let _ = mt_core::autodark::set_scheme(mt_core::autodark::Theme::Dark);
            });
        }
        "theme:light" => {
            std::thread::spawn(|| {
                let _ = mt_core::autodark::set_scheme(mt_core::autodark::Theme::Light);
            });
        }
        "center" => {
            spawn_detached(center, &[]);
        }
        p if p.starts_with("page:") => {
            let page = &p["page:".len()..];
            spawn_detached(center, &["--page", page]);
        }
        _ => {}
    }
}

fn spawn_detached(program: &PathBuf, args: &[&str]) {
    if let Err(e) = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        eprintln!("yihu-panel: 启动 {} 失败：{e}", program.display());
    }
}

fn sibling(name: &str) -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|p| p.join(name)))
        .unwrap_or_else(|| PathBuf::from(name))
}

/// 历史记一次并在后台线程落盘（激活路径不做文件 IO）。
fn record(history: &Rc<RefCell<mt_core::panel::History>>, id: &str) {
    let snapshot = {
        let mut h = history.borrow_mut();
        h.bump(id);
        h.clone()
    };
    std::thread::spawn(move || {
        let _ = snapshot.save();
    });
}

fn settle<T: Send + Sync + 'static>(nuc: &mut nucleo::Nucleo<T>) {
    let mut quiet = 0;
    loop {
        quiet = if nuc.tick(20).running { 0 } else { quiet + 1 };
        if quiet >= 2 {
            return;
        }
    }
}

// ---- 列表行 UI ----

fn setup_row(_: &gtk::SignalListItemFactory, obj: &glib::Object) {
    let li = obj.downcast_ref::<gtk::ListItem>().expect("ListItem");
    let row = GtkBox::new(Orientation::Horizontal, 12);
    let icon = gtk::Image::new();
    icon.set_pixel_size(24);
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
    let chips = GtkBox::new(Orientation::Horizontal, 8);
    chips.set_valign(Align::Center);
    chips.set_visible(false);
    row.append(&icon);
    row.append(&col);
    row.append(&chips);
    li.set_child(Some(&row));
    // 安全性：键 "yihu-row" 只在本文件 setup/bind 中使用，类型恒为 Row
    unsafe {
        li.set_data(
            "yihu-row",
            Row {
                icon,
                col,
                title,
                sub,
                chips,
            },
        )
    };
}

fn bind_row(
    _: &gtk::SignalListItemFactory,
    obj: &glib::Object,
    win: &Window,
    visible: &Arc<AtomicBool>,
    history: &Rc<RefCell<mt_core::panel::History>>,
    center: &PathBuf,
    entries: &Rc<RefCell<Vec<PanelEntry>>>,
) {
    let li = obj.downcast_ref::<gtk::ListItem>().expect("ListItem");
    let item = li.item().and_downcast::<PanelItem>().expect("PanelItem");
    // 安全性：同 set_data，键与类型由本文件保证
    let row: &Row = unsafe {
        let ptr = li.data::<Row>("yihu-row").expect("row data");
        ptr.as_ref()
    };
    match item.kind().as_str() {
        // 小节标题：灰字小号，不可激活
        "header" => {
            row.icon.set_visible(false);
            row.chips.set_visible(false);
            row.col.set_visible(true);
            row.sub.set_visible(false);
            row.title.set_text(&item.title());
            row.title.add_css_class("section-header");
        }
        // 最近胶囊行：最近用过的应用/能力，点击即触发
        "recent_chips" => {
            row.icon.set_visible(false);
            row.col.set_visible(false);
            row.chips.set_visible(true);
            while let Some(c) = row.chips.first_child() {
                row.chips.remove(&c);
            }
            for b in recent_chip_buttons(win, visible, history, center, entries) {
                row.chips.append(&b);
            }
        }
        // 快捷能力胶囊行
        "chips" => {
            row.icon.set_visible(false);
            row.col.set_visible(false);
            row.chips.set_visible(true);
            while let Some(c) = row.chips.first_child() {
                row.chips.remove(&c);
            }
            for b in chip_buttons(win, visible, history, center) {
                row.chips.append(&b);
            }
        }
        // 普通条目：图标 + 标题 + 副标题
        _ => {
            row.icon.set_visible(true);
            row.col.set_visible(true);
            row.chips.set_visible(false);
            row.title.remove_css_class("section-header");
            row.title.set_text(&item.title());
            row.sub.set_visible(true);
            row.sub.set_text(&item.subtitle());
            row.icon.set_from_gicon(&gicon_for_spec(&item.icon_spec()));
        }
    }
}

struct Row {
    icon: gtk::Image,
    col: GtkBox,
    title: gtk::Label,
    sub: gtk::Label,
    chips: GtkBox,
}

// ---- 列表条目 GObject ----

mod imp {
    use std::cell::RefCell;

    use gtk::glib::{self, Properties};
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;

    #[derive(Properties, Default)]
    #[properties(wrapper_type = super::PanelItem)]
    pub struct PanelItemImp {
        #[property(get, set)]
        pub title: RefCell<String>,
        #[property(get, set)]
        pub subtitle: RefCell<String>,
        #[property(get, set)]
        pub icon_spec: RefCell<String>,
        #[property(get, set)]
        pub kind: RefCell<String>,
        #[property(get, set)]
        pub payload: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PanelItemImp {
        const NAME: &'static str = "YihuPanelItem";
        type Type = super::PanelItem;
    }

    #[glib::derived_properties]
    impl ObjectImpl for PanelItemImp {}
}

glib::wrapper! {
    pub struct PanelItem(ObjectSubclass<imp::PanelItemImp>);
}

impl PanelItem {
    fn from_entry(e: &PanelEntry) -> Self {
        glib::Object::builder()
            .property("title", &e.title)
            .property("subtitle", &e.subtitle)
            .property("icon_spec", &e.icon_spec)
            .property("kind", e.kind)
            .property("payload", &e.payload)
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
