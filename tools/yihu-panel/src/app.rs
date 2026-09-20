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
use crate::providers;
use crate::service::{Cmd, BUS_NAME};
use crate::sessions::PluginMgr;

const MAX_SHOWN: usize = 100;

/// 一条可展示/可执行的条目（nucleo 按标题匹配，其余字段随条目带回）
#[derive(Clone)]
pub struct PanelEntry {
    pub title: String,
    pub subtitle: String,
    pub icon_spec: String,
    pub kind: &'static str,
    pub payload: String,
}

/// 跨呼出共享的状态：窗口每次呼出重建，这些保持不变。
struct Deps {
    app: gtk::Application,
    plugins: Rc<RefCell<PluginMgr>>,
    visible: Arc<AtomicBool>,
    history: Rc<RefCell<yihu_core::panel::History>>,
    center: PathBuf,
    entries: Rc<RefCell<Vec<PanelEntry>>>,
    nuc: Rc<RefCell<nucleo::Nucleo<PanelEntry>>>,
    query: Rc<RefCell<String>>,
    calc_row: Rc<RefCell<Option<PanelEntry>>>,
    dirty: Rc<Cell<bool>>,
    /// 呼出代数：每次 summon 递增；定位线程/兜底定时器凭它确认
    /// 自己仍属于当前这次呼出，防止迟到回调作用于新一代窗口
    gen: Rc<Cell<u64>>,
    /// 本次呼出是否已淡入；5s 最后兜底定时器凭它避免重复/提前淡入
    faded: Rc<Cell<bool>>,
    /// 定位线程回传 FadeIn 用（与 zbus 命令共用一条泵）
    fade_tx: mpsc::Sender<Cmd>,
}

/// 当前面板窗口及其专属控件（每次呼出重建一份）
struct PanelUi {
    win: Window,
    entry: SearchEntry,
    tick: Option<glib::SourceId>,
}

impl Drop for PanelUi {
    fn drop(&mut self) {
        if let Some(id) = self.tick.take() {
            id.remove();
        }
    }
}

type Slot = Rc<RefCell<Option<PanelUi>>>;

pub fn run_daemon() {
    let (tx, rx) = mpsc::channel::<Cmd>();
    let visible = Arc::new(AtomicBool::new(false));
    // 先于 GTK 声明总线名：占用即静默退出（天然单实例）；
    // tx 留一个克隆给 Deps，定位线程凭它回传淡入
    let conn = match crate::service::claim(tx.clone(), visible.clone()) {
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
    app.connect_activate(move |app| {
        let rx = rx_cell.borrow_mut().take().expect("activate 仅发生一次");
        // 无窗口待命：hold 保证主循环常驻（泄漏 guard = 永久持有）
        std::mem::forget(app.hold());
        load_css();

        let history = Rc::new(RefCell::new(yihu_core::panel::History::load()));
        let caps = providers::BuiltinProvider::capabilities();
        let apps = collect_apps();
        // 条目全集 = 内置能力 + 应用（供默认集「最近」回查）；nucleo 只匹配应用
        let entries = Rc::new(RefCell::new({
            let mut v = caps.clone();
            v.extend(apps.clone());
            v
        }));
        let nuc = Rc::new(RefCell::new(build_nucleo(&apps)));
        let plugins = Rc::new(RefCell::new(PluginMgr::new()));
        let deps = Rc::new(Deps {
            app: app.clone(),
            plugins,
            visible: visible.clone(),
            history,
            center: sibling("yihu"),
            entries,
            nuc,
            query: Rc::new(RefCell::new(String::new())),
            calc_row: Rc::new(RefCell::new(None)),
            dirty: Rc::new(Cell::new(false)),
            gen: Rc::new(Cell::new(0)),
            faded: Rc::new(Cell::new(false)),
            fade_tx: tx.clone(),
        });

        let slot: Slot = Rc::new(RefCell::new(None));

        // —— 命令消费：zbus → mpsc → 主循环（托盘既有惯例）——
        {
            let deps = deps.clone();
            let slot = slot.clone();
            let app = app.clone();
            glib::timeout_add_local(Duration::from_millis(50), move || {
                let mut quit = false;
                while let Ok(cmd) = rx.try_recv() {
                    match cmd {
                        Cmd::Toggle => {
                            if deps.visible.load(Ordering::Relaxed) {
                                hide_panel(&deps, &slot);
                            } else {
                                summon(&deps, &slot);
                            }
                        }
                        Cmd::Show => summon(&deps, &slot),
                        Cmd::Hide => hide_panel(&deps, &slot),
                        Cmd::Quit => quit = true,
                        Cmd::FadeIn(gen) => {
                            if deps.gen.get() != gen {
                                continue; // 上一代呼出的迟到淡入，丢弃
                            }
                            deps.faded.set(true);
                            if let Some(ui) = slot.borrow().as_ref() {
                                ui.win.set_opacity(1.0);
                            }
                            if std::env::var_os("YIHU_PANEL_DEBUG").is_some() {
                                let ms = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_millis())
                                    .unwrap_or(0);
                                eprintln!("yihu-panel: 淡入 gen={gen} 墙钟={ms}ms");
                            }
                        }
                    }
                }
                if quit {
                    app.quit();
                    return glib::ControlFlow::Break;
                }
                glib::ControlFlow::Continue
            });
        }
    });
    app.run();
    drop(conn); // 连接存活到进程退出
}

/// 无头基准（YIHU_PANEL_BENCH=1）：枚举真实应用、测典型查询耗时后退出。
pub fn run_bench() {
    let t0 = Instant::now();
    let apps = collect_apps();
    let t_apps = t0.elapsed();
    let mut entries = providers::BuiltinProvider::capabilities();
    entries.extend(apps.clone());
    let total = entries.len();
    let mut nuc = build_nucleo(&entries);
    settle(&mut nuc);
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

/// 每次呼出销毁旧窗口、新建一份：Wayland 下客户端无定位接口，
/// 隐藏后重新 present 不会重新走合成器的居中摆放，只有新映射才会。
/// 共享状态（条目/搜索池/历史）在 Deps 中跨呼出保持，重建仅有控件成本。
fn summon(deps: &Rc<Deps>, slot: &Slot) {
    dispose(slot);
    deps.plugins.borrow_mut().ensure_sessions();
    let ui = build_panel_ui(deps, slot);
    *slot.borrow_mut() = Some(ui);
    let (win, entry) = {
        let ui = slot.borrow();
        let ui = ui.as_ref().expect("刚插入");
        (ui.win.clone(), ui.entry.clone())
    };
    win.present();
    entry.grab_focus();
    // 淡入由定位流程驱动：定位线程确认「窗口已配置尺寸且完成映射后摆放」
    // 后发 FadeIn（gen 防跨呼出误伤）；扩展缺失/超时由下面的兜底定时器淡入。
    // 固定延时会撞上 mutter 50 对映射前摆放的重置——先错位后跳转的根源。
    deps.gen.set(deps.gen.get() + 1);
    let gen = deps.gen.get();
    deps.faded.set(false);
    let offset = yihu_core::panel::Config::load().place_offset_up;
    crate::service::call_placer(offset, gen, deps.fade_tx.clone());
    {
        // 最后兜底：正常由定位线程在定位落地后发 FadeIn；此处仅防线程
        // 意外消亡导致窗口永不显示。faded 标志防止与正常路径重复淡入
        let gen_cell = deps.gen.clone();
        let faded = deps.faded.clone();
        let w = win.clone();
        glib::timeout_add_local_once(Duration::from_secs(5), move || {
            if gen_cell.get() == gen && !faded.get() {
                w.set_opacity(1.0);
            }
        });
    }
    deps.visible.store(true, Ordering::Relaxed);
    deps.dirty.set(true);
    if std::env::var_os("YIHU_PANEL_DEBUG").is_some() {
        let w = win.clone();
        glib::timeout_add_local_once(Duration::from_millis(200), move || {
            eprintln!("yihu-panel: 呼出后实际尺寸 {}x{}", w.width(), w.height());
        });
    }
}

/// 收起面板：隐藏窗口、杀灭全部插件会话（待命零进程）、延迟销毁窗口。
fn hide_panel(deps: &Deps, slot: &Slot) {
    if let Some(ui) = slot.borrow().as_ref() {
        ui.win.set_visible(false);
    }
    deps.visible.store(false, Ordering::Relaxed);
    deps.plugins.borrow_mut().kill_all();
    dispose(slot);
    // 归还给 OS，守住待命 RSS（长驻进程堆回收惯例）
    unsafe { libc::malloc_trim(0) };
}

fn dispose(slot: &Slot) {
    if let Some(ui) = slot.borrow_mut().take() {
        ui.win.set_visible(false);
        // ui.drop 负责移除刷新心跳；延迟销毁避免在信号处理栈内析构控件
        glib::idle_add_local_once(move || drop(ui));
    }
}

/// 构建一局面板窗口并接线全部信号。
fn build_panel_ui(deps: &Rc<Deps>, slot: &Slot) -> PanelUi {
    let win = Window::new();
    // 首帧近乎透明（0.01）：opacity=0 时 GTK 跳过渲染、不提交缓冲，
    // 合成器永远无法完成配置映射，「等定位后再显示」会变成死锁；
    // 0.01 足以触发真实绘制，肉眼不可见。等定位扩展在映射后把窗口
    // 摆到位，再由 FadeIn 升到 1.0，避免「先错位后跳转」
    win.set_opacity(0.01);
    win.set_application(Some(&deps.app));
    win.set_title(Some("一呼"));
    win.set_icon_name(Some("tools.yihu.desktop"));
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

    let store = gio::ListStore::new::<PanelItem>();
    let sel = gtk::SingleSelection::new(Some(store.clone()));
    // 不自动选中：默认集全是标题/胶囊行，高亮没有意义；
    // 搜索结果出现时由刷新逻辑选中第一个可激活行
    sel.set_autoselect(false);
    let factory = gtk::SignalListItemFactory::new();
    {
        let deps = deps.clone();
        let slot = slot.clone();
        factory.connect_setup(setup_row);
        factory.connect_bind(move |f, obj| bind_row(f, obj, &deps, &slot));
    }
    let list = ListView::new(Some(sel.clone()), Some(factory));
    list.add_css_class("panel-list");
    // 启动器惯例：单击即激活（默认是双击）
    list.set_single_click_activate(true);
    let scroll = ScrolledWindow::new();
    scroll.set_child(Some(&list));
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    // 内容自然高度 ≤ 520 时窗口贴合内容（无滚动条）；
    // 超过 520 时窗口高度被 520 封顶，列表滚动
    scroll.set_propagate_natural_height(true);
    scroll.set_max_content_height(520);
    scroll.set_vexpand(true);
    card.append(&scroll);
    win.set_child(Some(&card));

    // —— 主题跟随：gio::Settings 监听，show 路径零 IO ——
    watch_theme(&win);

    {
        let nuc = deps.nuc.clone();
        let query = deps.query.clone();
        let calc_row = deps.calc_row.clone();
        let plugins = deps.plugins.clone();
        let dirty = deps.dirty.clone();
        entry.connect_search_changed(move |e| {
            let text = e.text().to_string();
            *calc_row.borrow_mut() = calc_entry(&text);
            *query.borrow_mut() = text;
            {
                let mut nuc = nuc.borrow_mut();
                nuc.pattern.reparse(
                    0,
                    &query.borrow(),
                    nucleo::pattern::CaseMatching::Smart,
                    nucleo::pattern::Normalization::Smart,
                    false,
                );
            }
            plugins.borrow_mut().broadcast(&query.borrow());
            dirty.set(true);
        });
    }

    // —— 刷新：nucleo 结果 / 分组默认集，计算行置顶 ——
    let tick = {
        let nuc = deps.nuc.clone();
        let entries = deps.entries.clone();
        let history = deps.history.clone();
        let query = deps.query.clone();
        let calc_row = deps.calc_row.clone();
        let dirty = deps.dirty.clone();
        let store = store.clone();
        let sel = sel.clone();
        let win = win.clone();
        let plugins = deps.plugins.clone();
        glib::timeout_add_local(Duration::from_millis(30), move || {
            if plugins.borrow_mut().drain() {
                dirty.set(true);
            }
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
                // 分组默认集：最近 / 快捷能力(胶囊)
                for e in default_rows(&history.borrow(), &entries.borrow()) {
                    if rows.len() >= MAX_SHOWN {
                        break;
                    }
                    rows.push(e.clone());
                }
            } else {
                {
                    let nuc = nuc.borrow();
                    let snap = nuc.snapshot();
                    let n = snap
                        .matched_item_count()
                        .min((MAX_SHOWN - rows.len()) as u32);
                    for it in snap.matched_items(0..n) {
                        rows.push(it.data.clone());
                    }
                }
                // 内置能力 + 插件结果（同受 MAX_SHOWN 约束）
                for e in providers::BuiltinProvider::query(&q) {
                    if rows.len() >= MAX_SHOWN {
                        break;
                    }
                    rows.push(e);
                }
                for e in plugins.borrow().latest_rows() {
                    if rows.len() >= MAX_SHOWN {
                        break;
                    }
                    rows.push(e);
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
            let kinds: Vec<&str> = rows.iter().map(|e| e.kind).collect();
            store.remove_all();
            for e in rows {
                store.append(&PanelItem::from_entry(&e));
            }
            // 高度交给 GTK：滚动窗口把内容自然高度上报为窗口自然高度
            //（propagate-natural-height，封顶由 max-content-height 决定），
            // 窗口高度设为 -1（自然高度）即可贴合内容；无 IO。
            let w = win.clone();
            glib::idle_add_local_once(move || {
                w.set_default_size(720, -1);
                if std::env::var_os("YIHU_PANEL_DEBUG").is_some() {
                    eprintln!("yihu-panel: 高度校正为自然高度，实际 {}x{}", w.width(), w.height());
                }
            });
            // 选中项若落在标题/胶囊等不可激活行上，挪到第一个可激活行
            if !selectable_at(&store, sel.selected()) {
                if let Some(p) = first_selectable(&store) {
                    sel.set_selected(p);
                }
            }
            if std::env::var_os("YIHU_PANEL_DEBUG").is_some() {
                eprintln!("yihu-panel: 行 {kinds:?}，刷新 {:?}", start.elapsed());
            }
            glib::ControlFlow::Continue
        })
    };

    // —— 激活：按 kind 分发真实动作（回车 / 单击 / 双击）——
    {
        let deps = deps.clone();
        let store = store.clone();
        let slot = slot.clone();
        list.connect_activate(move |_, pos| {
            dispatch_item(&deps, &store, pos, &slot);
        });
    }
    {
        // 回车时焦点在搜索框，列表收不到按键——在搜索框上激活当前选中项
        let deps = deps.clone();
        let store = store.clone();
        let sel = sel.clone();
        let slot = slot.clone();
        entry.connect_activate(move |_| {
            let pos = sel.selected();
            if sel.model().map(|m| m.n_items()).unwrap_or(0) > pos {
                dispatch_item(&deps, &store, pos, &slot);
            }
        });
    }

    // —— 键盘：ESC 隐藏；搜索框内 ↑/↓ 移动选择 ——
    let ec = gtk::EventControllerKey::new();
    {
        let deps = deps.clone();
        let slot = slot.clone();
        ec.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape {
                hide_panel(&deps, &slot);
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
    }
    win.add_controller(ec);
    entry.connect_stop_search({
        let deps = deps.clone();
        let slot = slot.clone();
        move |_| hide_panel(&deps, &slot)
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

    // —— 失焦自动隐藏：仅对「曾拿到焦点」的窗口生效。
    // CLI/快捷键触发的映射可能拿不到焦点（focus-stealing-prevention），
    // 此时不能误判为用户点了别处 ——
    {
        let focused = Rc::new(Cell::new(false));
        let deps = deps.clone();
        let slot = slot.clone();
        win.connect_notify_local(Some("is-active"), move |w, _| {
            if w.is_active() {
                focused.set(true);
            } else if focused.get() && w.is_visible() {
                hide_panel(&deps, &slot);
            }
        });
    }

    PanelUi {
        win,
        entry,
        tick: Some(tick),
    }
}

/// 按 kind 分发条目动作：app 启动 / cap 执行 / calc 复制 / plugin 转发。
fn dispatch_item(deps: &Deps, store: &gio::ListStore, pos: u32, slot: &Slot) {
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
        if let Some(ui) = slot.borrow().as_ref() {
            ui.win.clipboard().set_text(&payload);
        }
        item.set_subtitle("已复制".to_string());
        record(&deps.history, "calc");
        return;
    }
    if kind == "plugin" {
        // v0 激活语义：宿主复制 payload（Wayland 下插件进程自行操作剪贴板
        // 不可靠），并转发 activate 让插件感知
        let Some((pid, pl)) = payload.split_once('|') else {
            return;
        };
        if let Some(ui) = slot.borrow().as_ref() {
            ui.win.clipboard().set_text(pl);
        }
        deps.plugins.borrow_mut().activate(pid, pl);
        record(&deps.history, &format!("plugin:{pid}"));
        hide_panel(deps, slot);
        return;
    }
    perform_entry(
        deps,
        slot,
        &PanelEntry {
            title: item.title().to_string(),
            subtitle: item.subtitle().to_string(),
            icon_spec: item.icon_spec().to_string(),
            kind: if kind == "app" { "app" } else { "cap" },
            payload,
        },
    );
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

// ---- 提供者：应用枚举 ----

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
    history: &yihu_core::panel::History,
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
fn default_rows(history: &yihu_core::panel::History, entries: &[PanelEntry]) -> Vec<PanelEntry> {
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
fn chip_buttons(deps: &Rc<Deps>, slot: &Slot) -> Vec<gtk::Button> {
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
            let deps = deps.clone();
            let slot = slot.clone();
            let payload = payload.to_string();
            b.connect_clicked(move |_| {
                providers::activate_capability(&payload, &deps.center);
                record(&deps.history, &format!("cap:{payload}"));
                hide_panel(&deps, &slot);
            });
            b
        })
        .collect()
}

/// 最近胶囊：最近用过的应用/能力（按时间，最多 4 个），点击即触发。
fn recent_chip_buttons(deps: &Rc<Deps>, slot: &Slot) -> Vec<gtk::Button> {
    let recent = recent_entries(&deps.history.borrow(), &deps.entries.borrow(), 4);
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
            let deps = deps.clone();
            let slot = slot.clone();
            b.connect_clicked(move |_| {
                perform_entry(&deps, &slot, &e);
            });
            b
        })
        .collect()
}

/// 执行条目动作：app 启动 / cap 执行；成功后记历史并收起面板。
fn perform_entry(deps: &Deps, slot: &Slot, e: &PanelEntry) {
    match e.kind {
        "app" => {
            if launch_app(&e.payload) {
                record(&deps.history, &format!("app:{}", e.payload));
                hide_panel(deps, slot);
            }
        }
        "cap" => {
            providers::activate_capability(&e.payload, &deps.center);
            record(&deps.history, &format!("cap:{}", e.payload));
            hide_panel(deps, slot);
        }
        _ => {}
    }
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

fn sibling(name: &str) -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|p| p.join(name)))
        .unwrap_or_else(|| PathBuf::from(name))
}

/// 历史记一次并在后台线程落盘（激活路径不做文件 IO）。
fn record(history: &Rc<RefCell<yihu_core::panel::History>>, id: &str) {
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

fn bind_row(_: &gtk::SignalListItemFactory, obj: &glib::Object, deps: &Rc<Deps>, slot: &Slot) {
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
            for b in recent_chip_buttons(deps, slot) {
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
            for b in chip_buttons(deps, slot) {
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
