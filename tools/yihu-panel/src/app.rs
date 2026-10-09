//! 面板窗口：隐藏待命 + nucleo 模糊过滤 + ESC/失焦隐藏。
//!
//! 呼出路径零 IO：应用枚举、历史、主题判定都在启动时完成，
//! 之后 `toggle` 只做 present + grab_focus。
//! 条目分三类：`app`（GIO 应用启动）/ `cap`（一呼内置能力）/ `calc`（算式结果）。

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use gtk::glib;
use gtk::prelude::*;
use gtk::{
    gio, gdk, Align, Box as GtkBox, ListView, Orientation, ScrolledWindow, SearchEntry,
    Window,
};

use crate::calc;
use crate::providers;
use crate::service::{Cmd, BUS_NAME};
use crate::sessions::PluginMgr;

const MAX_SHOWN: usize = 100;

/// 一条可展示/可执行的条目（nucleo 按标题匹配，其余字段随条目带回）
#[derive(Clone, Debug)]
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
    /// 能力代理审计（后台线程写 JSONL，record 永不阻塞主循环）
    audit: Rc<yihu_core::audit::Audit>,
    /// 应用列表热刷新槽：AppInfoMonitor 触发后台重扫，结果经此回主循环
    app_refresh: Arc<Mutex<Option<Vec<PanelEntry>>>>,
    /// 系统插件启用集（呼出时加载缓存；key 路径零 IO，中心页改开关下次呼出生效）
    sys_plugins: Rc<RefCell<std::collections::HashSet<String>>>,
    /// 异步能力结果槽（截屏等：后台线程完成后经此回主循环回包）
    cap_async: Arc<Mutex<Vec<CapOutcome>>>,
}

/// 后台执行的能力结果（截屏）：由常驻泵回主循环回包/审计/副作用
struct CapOutcome {
    plugin: String,
    gen: u64,
    request_id: u64,
    ok: bool,
    error: String,
    /// 截图文件路径（成功时；用于系统通知）
    path: String,
    /// clipboard=true 时的 PNG 字节（主线程写剪贴板）
    png: Option<Vec<u8>>,
    clipboard: bool,
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
        apply_css(); // 动态主题（accent + 深浅）

        let history = Rc::new(RefCell::new(yihu_core::panel::History::load()));
        // 系统插件启用集：启动时加载一次（启动路径 IO 合法），呼出时刷新
        let sys_plugins = Rc::new(RefCell::new(providers::enabled_system_plugins()));
        let caps = providers::BuiltinProvider::capabilities();
        let apps = collect_apps();
        // 条目全集 = 内置能力（按启停过滤）+ 应用（供默认集「最近」回查）；nucleo 只匹配应用
        let entries = Rc::new(RefCell::new({
            let mut v: Vec<PanelEntry> = caps
                .iter()
                .filter(|e| sys_plugins.borrow().contains(providers::owner_of(&e.payload)))
                .cloned()
                .collect();
            v.extend(apps.clone());
            v
        }));
        let nuc = Rc::new(RefCell::new(build_nucleo(&apps)));
        let plugins = Rc::new(RefCell::new(PluginMgr::new()));
        let audit = Rc::new(yihu_core::audit::Audit::open(yihu_core::audit::audit_path()));
        let app_refresh: Arc<Mutex<Option<Vec<PanelEntry>>>> = Arc::new(Mutex::new(None));
        let cap_async: Arc<Mutex<Vec<CapOutcome>>> = Arc::new(Mutex::new(Vec::new()));
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
            audit,
            app_refresh: app_refresh.clone(),
            sys_plugins: sys_plugins.clone(),
            cap_async: cap_async.clone(),
        });

        // —— 应用列表热刷新（AppInfoMonitor，穿插小项）——
        // 新装/卸载应用不再要求重启面板；呼出路径零 IO 不变（重扫在
        // 后台线程，重建在主循环 tick，与启动时同一构建函数）。
        {
            let pending = Rc::new(Cell::new(false));
            let monitor = gio::AppInfoMonitor::get();
            monitor.connect_changed(move |_| {
                if pending.get() {
                    return;
                }
                pending.set(true);
                // 防抖 2s：安装过程会连发多次 changed
                let slot = app_refresh.clone();
                let pending2 = pending.clone();
                glib::timeout_add_local(Duration::from_secs(2), move || {
                    pending2.set(false);
                    let slot = slot.clone();
                    std::thread::spawn(move || {
                        let apps = collect_apps();
                        *slot.lock().unwrap() = Some(apps);
                    });
                    glib::ControlFlow::Break
                });
            });
            // 守护进程生命周期内常驻监听（泄漏 = 持有，同 app.hold 惯例）
            std::mem::forget(monitor);
        }

        let slot: Slot = Rc::new(RefCell::new(None));

        // —— 命令消费：zbus → mpsc → 主循环（托盘既有惯例）——
        {
            let deps = deps.clone();
            let slot = slot.clone();
            let app = app.clone();
            glib::timeout_add_local(Duration::from_millis(50), move || {
                let mut quit = false;
                // 应用列表热刷新消费：后台重扫完成 → 主循环重建（不阻塞）
                if let Some(new_apps) = deps.app_refresh.lock().unwrap().take() {
                    let caps = providers::BuiltinProvider::capabilities();
                    let mut all: Vec<PanelEntry> = caps
                        .iter()
                        .filter(|e| {
                            deps.sys_plugins.borrow().contains(providers::owner_of(&e.payload))
                        })
                        .cloned()
                        .collect();
                    all.extend(new_apps.iter().cloned());
                    *deps.entries.borrow_mut() = all;
                    *deps.nuc.borrow_mut() = build_nucleo(&new_apps);
                    if deps.visible.load(Ordering::Relaxed) {
                        deps.dirty.set(true);
                    }
                }
                // 异步能力结果（截屏等）：回包 + 审计 + 副作用
                for out in deps.cap_async.lock().unwrap().drain(..) {
                    if out.ok {
                        deps.plugins
                            .borrow_mut()
                            .respond_gen(&out.plugin, out.gen, out.request_id, true, "");
                        deps.audit
                            .record(&out.plugin, out.gen, "screenshot.take", "grant", "");
                        if out.clipboard {
                            if let Some(bytes) = &out.png {
                                if let Some(display) = gdk::Display::default() {
                                    let provider = gdk::ContentProvider::for_bytes(
                                        "image/png",
                                        &glib::Bytes::from(bytes.as_slice()),
                                    );
                                    if display.clipboard().set_content(Some(&provider)).is_err() {
                                        eprintln!("yihu-panel: 截图写入剪贴板失败");
                                    }
                                }
                            }
                        }
                        let summary = if out.clipboard { "截图已复制到剪贴板" } else { "截图已保存" };
                        let _ = spawn_detached_checked(
                            "notify-send",
                            &["--app-name=一呼", &format!("{summary}：{}", out.path)],
                        );
                    } else {
                        deps.plugins
                            .borrow_mut()
                            .respond_gen(&out.plugin, out.gen, out.request_id, false, &out.error);
                        deps.audit
                            .record(&out.plugin, out.gen, "screenshot.take", "error", &out.error);
                        // 用户主动取消：只审计，不弹通知打扰
                        if out.error != "已取消" {
                            let _ = spawn_detached_checked(
                                "notify-send",
                                &["--app-name=一呼", &format!("截图失败：{}", out.error)],
                            );
                        }
                    }
                }
                // 常驻 provider 空闲清扫只在隐藏态跑（可见态 keystroke
                // 持续刷新 last_used，清扫无意义且会打断正在交互的插件）
                if !deps.visible.load(Ordering::Relaxed) {
                    let _ = deps.plugins.borrow_mut().sweep_idle();
                }
                while let Ok(cmd) = rx.try_recv() {
                    match cmd {
                        Cmd::Toggle => {
                            if deps.visible.load(Ordering::Relaxed) {
                                hide_panel(&deps, &slot);
                            } else {
                                summon(&deps, &slot);
                            }
                        }
                        Cmd::Screenshot => {
                            // 快捷键截图：后台 portal 交互截屏，结果经
                            // cap_async 槽回常驻泵（回包对未知插件是
                            // no-op，审计/通知统一在泵里做）
                            deps.audit
                                .record("hotkey", 0, "screenshot.take", "grant", "");
                            let slot = deps.cap_async.clone();
                            std::thread::spawn(move || {
                                let (ok, error, path) =
                                    match crate::screenshot::take("area", false) {
                                        Ok(_) => (
                                            true,
                                            String::new(),
                                            crate::screenshot::latest_shot_hint(),
                                        ),
                                        Err(e) => (false, e, String::new()),
                                    };
                                slot.lock().unwrap().push(CapOutcome {
                                    plugin: "hotkey".into(),
                                    gen: 0,
                                    request_id: 0,
                                    ok,
                                    error,
                                    path,
                                    png: None,
                                    clipboard: false,
                                });
                            });
                        }
                        Cmd::Show => summon(&deps, &slot),
                        Cmd::Hide => hide_panel(&deps, &slot),
                        Cmd::Quit => quit = true,
                        Cmd::SelectFiles(files) => {
                            // 选中文件上下文（显式用户动作注入）；净化后
                            // 存入会话管理器，query 按权限转发
                            let clean: Vec<String> = files
                                .into_iter()
                                .filter(|f| !f.is_empty() && f.len() <= 4096)
                                .take(64)
                                .collect();
                            if std::env::var_os("YIHU_PANEL_DEBUG").is_some() {
                                eprintln!("yihu-panel: 上下文选中 {} 项", clean.len());
                            }
                            deps.plugins.borrow_mut().set_context_files(clean);
                        }
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
    let enabled: std::collections::HashSet<String> = providers::SYSTEM_PLUGINS
        .iter()
        .map(|p| p.id.to_string())
        .collect();
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
        let _ = &enabled;
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
    // 系统插件启停可能有变化（中心页改的）：呼出时刷新缓存
    //（此处本就有注册表扫描 IO，多一次状态文件读无新增红线压力）
    *deps.sys_plugins.borrow_mut() = providers::enabled_system_plugins();
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
    win.set_decorated(false); // 无标题栏：启动器是浮层，Esc/失焦即收起
    win.set_hide_on_close(true);
    win.add_css_class("panel-root");

    // —— 布局：搜索框 + 结果列表 ——
    let card = GtkBox::new(Orientation::Vertical, 8);
    card.add_css_class("panel-card");

    let entry = SearchEntry::new();
    entry.set_placeholder_text(Some("搜索 / 计算 / 呼出能力"));
    entry.add_css_class("panel-search");
    entry.set_search_delay(0);
    card.append(&entry);

    // —— 默认页（空查询）：常用应用图标墙 + 能力宫格；搜索时隐藏 ——
    let grid_view = GtkBox::new(Orientation::Vertical, 2);
    grid_view.append(&section_label("常用"));
    grid_view.append(&build_app_wall(deps, slot));
    grid_view.append(&section_label("快捷能力"));
    grid_view.append(&build_cap_grid(deps, slot));
    grid_view.append(&section_label("提示"));
    let tip = gtk::Label::new(Some("输入即搜索；g 词 网页搜索 · 算式直接计算"));
    tip.add_css_class("caption-sm");
    tip.set_halign(Align::Start);
    tip.set_margin_start(6);
    grid_view.append(&tip);
    card.append(&grid_view);

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

    // —— 底部键提示栏 ——
    card.append(&key_bar());
    win.set_child(Some(&card));
    grid_view.set_visible(true); // 空查询默认显示
    scroll.set_visible(false);

    // —— 主题跟随：gio::Settings 监听，show 路径零 IO ——
    watch_theme(&win);

    {
        let nuc = deps.nuc.clone();
        let query = deps.query.clone();
        let calc_row = deps.calc_row.clone();
        let plugins = deps.plugins.clone();
        let dirty = deps.dirty.clone();
        let sys = deps.sys_plugins.clone();
        entry.connect_search_changed(move |e| {
            let text = e.text().to_string();
            *calc_row.borrow_mut() = if sys.borrow().contains("calc") {
                calc_entry(&text)
            } else {
                None
            };
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
        let query = deps.query.clone();
        let calc_row = deps.calc_row.clone();
        let dirty = deps.dirty.clone();
        let store = store.clone();
        let sel = sel.clone();
        let win = win.clone();
        let plugins = deps.plugins.clone();
        let deps_cap = deps.clone();
        let grid_view = grid_view.clone();
        let scroll = scroll.clone();
        glib::timeout_add_local(Duration::from_millis(30), move || {
            let (plugin_dirty, cap_reqs) = plugins.borrow_mut().drain();
            if plugin_dirty {
                dirty.set(true);
            }
            // 能力请求在 drain 归还后处理：执行与回复都要重借 mgr
            for req in cap_reqs {
                handle_capability(&deps_cap, req);
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
                // 默认页 = 网格视图（图标墙 + 能力宫格），列表退场
                grid_view.set_visible(true);
                scroll.set_visible(false);
            } else {
                grid_view.set_visible(false);
                scroll.set_visible(true);
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
                // 内置能力（按系统插件启停过滤）+ 插件结果（同受 MAX_SHOWN 约束）
                let enabled = deps_cap.sys_plugins.borrow().clone();
                for e in providers::BuiltinProvider::query(&q, &enabled) {
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

/// 能力请求处理：授权（manifest 声明 + 参数校验）→ 执行 → 回复 + 审计。
/// 授权失败/执行失败都以 ok:false 回复，插件据此降级。
/// (id, gen) 精确配对：旧代会话的迟到请求只审计丢弃，不借新代执行
/// （BUG-001 同源教训）。
fn handle_capability(deps: &Deps, req: crate::sessions::CapRequest) {
    let declared = deps
        .plugins
        .borrow()
        .permissions_of_gen(&req.plugin, req.gen)
        .cloned();
    let Some(declared) = declared else {
        deps.audit
            .record(&req.plugin, req.gen, &req.capability, "stale", "会话已换代，迟到请求丢弃");
        return;
    };
    let outcome = match crate::caps::evaluate(&declared, &req.capability, &req.params) {
        // 截屏是异步长操作（区域模式等用户框选）：后台执行，常驻泵回包
        Ok(crate::caps::CapAction::Screenshot { mode, clipboard }) => {
            deps.audit
                .record(&req.plugin, req.gen, &req.capability, "grant", "");
            spawn_screenshot_job(deps, &req, &mode, clipboard);
            return;
        }
        Ok(action) => execute_capability(action),
        Err(e) => Err(e),
    };
    match outcome {
        Ok(()) => {
            deps.plugins
                .borrow_mut()
                .respond_gen(&req.plugin, req.gen, req.request_id, true, "");
            deps.audit
                .record(&req.plugin, req.gen, &req.capability, "grant", "");
        }
        Err(e) => {
            deps.plugins
                .borrow_mut()
                .respond_gen(&req.plugin, req.gen, req.request_id, false, &e);
            deps.audit
                .record(&req.plugin, req.gen, &req.capability, "deny", &e);
        }
    }
}

/// 截屏后台作业：portal 调用（可能等用户框选数分钟）→ 结果入槽，
/// 常驻泵回主循环回包。审批（grant 审计）已在 handle_capability 记录。
fn spawn_screenshot_job(deps: &Deps, req: &crate::sessions::CapRequest, mode: &str, clipboard: bool) {
    let slot = deps.cap_async.clone();
    let plugin = req.plugin.clone();
    let gen = req.gen;
    let request_id = req.request_id;
    let mode = mode.to_string();
    std::thread::spawn(move || {
        let (ok, error, path, png) = match crate::screenshot::take(&mode, clipboard) {
            Ok(png) => {
                let path = if mode == "area" {
                    crate::screenshot::latest_shot_hint()
                } else {
                    String::new()
                };
                (true, String::new(), path, png)
            }
            Err(e) => (false, e, String::new(), None),
        };
        slot.lock().unwrap().push(CapOutcome {
            plugin,
            gen,
            request_id,
            ok,
            error,
            path,
            png,
            clipboard,
        });
    });
}

/// 执行已授权的能力动作。GTK 动作在主线程（tick 内天然成立）；
/// 子进程动作 detached，不随面板收起被杀。
fn execute_capability(action: crate::caps::CapAction) -> Result<(), String> {
    match action {
        crate::caps::CapAction::ClipboardWrite(text) => {
            let Some(display) = gdk::Display::default() else {
                return Err("无默认显示".into());
            };
            display.clipboard().set_text(&text);
            Ok(())
        }
        crate::caps::CapAction::OpenUri(uri) => spawn_detached_checked("xdg-open", &[&uri]),
        crate::caps::CapAction::LaunchApp(desktop_id) => {
            if launch_app(&desktop_id) {
                Ok(())
            } else {
                Err(format!("应用不存在或无法启动：{desktop_id}"))
            }
        }
        crate::caps::CapAction::Notify { summary, body } => {
            if body.is_empty() {
                spawn_detached_checked("notify-send", &[&summary])
            } else {
                spawn_detached_checked("notify-send", &[&summary, &body])
            }
        }
        // 截屏走异步作业（handle_capability 拦截），同步路径不可达
        crate::caps::CapAction::Screenshot { .. } => Ok(()),
    }
}

fn spawn_detached_checked(program: &str, args: &[&str]) -> Result<(), String> {
    std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("{program} 启动失败：{e}"))
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
        // 不可靠），并转发 activate 让插件感知。
        // `!` 前缀 = 动作型 payload，不自动复制（如截图插件的 "full"）。
        let Some((pid, pl)) = payload.split_once('|') else {
            return;
        };
        let (copy, pl) = match pl.strip_prefix('!') {
            Some(rest) => (false, rest),
            None => (true, pl),
        };
        if copy {
            if let Some(ui) = slot.borrow().as_ref() {
                ui.win.clipboard().set_text(pl);
            }
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
            apply_css(); // 深浅切换换整套调色板（UI 重设计）
        });
    }
    {
        // 强调色变化：只重建样式表（GNOME 47+；键缺失时回调不触发）
        s.connect_changed(Some("accent-color"), move |_, _| {
            apply_css();
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
    // 单列 haystack = 标题 + 拼音/首字母 + 英文 id 尾段（uTools 式拼音
    // 搜索：输入 "wjgl" 命中「文件管理」、"nautilus" 命中英文 id）。
    // 注：nucleo 的 MultiPattern 是「各列全部须命中」（AND），做不了
    // 任一列命中，所以拼音必须并进同一列而不是开第二列。
    let nuc = nucleo::Nucleo::new(nucleo::Config::DEFAULT, notify, Some(2), 1);
    let inj = nuc.injector();
    for e in entries {
        let e = e.clone();
        inj.push(e, |item, cols| {
            let mut col = String::with_capacity(item.title.len() * 4);
            col.push_str(&item.title);
            col.push(' ');
            col.push_str(&crate::pinyin_index::match_column(&item.title));
            let t = item.payload.trim_end_matches(".desktop");
            let tail = t.rsplit('.').next().unwrap_or(t);
            // 只收纯 ASCII 标识尾段，能力 payload（theme:dark 等）不含冒号进来
            if tail.len() > 1
                && tail
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                col.push(' ');
                col.push_str(&tail.to_ascii_lowercase());
            }
            cols[0] = col.into();
        });
    }
    nuc
}

// ---- 分组默认集：最近 / 快捷能力(胶囊) / 常用应用 ----


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
    let row = GtkBox::new(Orientation::Horizontal, 10);
    // 选中条：行首 3px 强调色竖条（选中态由 CSS .sel-bar 上色）
    let sel_bar = GtkBox::new(Orientation::Vertical, 0);
    sel_bar.add_css_class("sel-bar");
    sel_bar.set_valign(Align::Fill);
    let icon = gtk::Image::new();
    icon.set_pixel_size(24);
    icon.set_valign(Align::Center);
    let col = GtkBox::new(Orientation::Vertical, 2);
    col.set_valign(Align::Center);
    col.set_hexpand(true);
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
    let badge = gtk::Label::new(None);
    badge.add_css_class("badge");
    badge.set_valign(Align::Center);
    badge.set_visible(false);
    row.append(&sel_bar);
    row.append(&icon);
    row.append(&col);
    row.append(&badge);
    li.set_child(Some(&row));
    // 安全性：键 "yihu-row" 只在本文件 setup/bind 中使用，类型恒为 Row
    unsafe {
        li.set_data(
            "yihu-row",
            Row {
                sel_bar,
                icon,
                col,
                title,
                sub,
                badge,
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
    let _ = (deps, slot); // 徽章化后 bind 不再需要闭包上下文（保留签名适配工厂）
    match item.kind().as_str() {
        // 小节标题（搜索场景已不用；保留兜底）：灰字小号，不可激活
        "header" => {
            row.sel_bar.set_visible(false);
            row.icon.set_visible(false);
            row.badge.set_visible(false);
            row.col.set_visible(true);
            row.sub.set_visible(false);
            row.title.set_text(&item.title());
            row.title.add_css_class("section-header");
        }
        // 普通条目：选中条 + 图标 + 标题/副标题 + 行尾类型徽章
        _ => {
            row.sel_bar.set_visible(true);
            row.icon.set_visible(true);
            row.col.set_visible(true);
            row.title.remove_css_class("section-header");
            row.title.set_text(&item.title());
            row.sub.set_visible(true);
            row.sub.set_text(&item.subtitle());
            row.icon.set_from_gicon(&gicon_for_spec(&item.icon_spec()));
            let (text, class) = badge_for(item.kind().as_str(), &item.payload());
            row.badge.set_visible(!text.is_empty());
            row.badge.set_text(&text);
            row.badge.remove_css_class("plugin");
            row.badge.remove_css_class("calc");
            if !class.is_empty() {
                row.badge.add_css_class(class);
            }
        }
    }
}

/// 行尾徽章文本与样式类：应用/能力/算式/插件名（插件行 payload = id|payload）
fn badge_for(kind: &str, payload: &str) -> (String, &'static str) {
    match kind {
        "app" => ("应用".into(), ""),
        "cap" => ("能力".into(), ""),
        "calc" => ("算式".into(), "calc"),
        "plugin" => {
            let id = payload.split('|').next().unwrap_or("插件");
            (format!("{id}"), "plugin")
        }
        _ => (String::new(), ""),
    }
}

// ---- 默认页：常用应用图标墙 + 能力宫格 ----

fn section_label(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class("section-label");
    l.set_halign(Align::Start);
    l.set_margin_start(6);
    l
}

/// 常用应用墙：最近使用（历史时间序）优先补齐到 8 个，tile 点击即启动。
fn build_app_wall(deps: &Rc<Deps>, slot: &Slot) -> gtk::FlowBox {
    let flow = gtk::FlowBox::new();
    flow.add_css_class("appwall");
    flow.set_homogeneous(true);
    flow.set_max_children_per_line(8);
    flow.set_min_children_per_line(4);
    flow.set_column_spacing(4);
    flow.set_row_spacing(2);
    flow.set_selection_mode(gtk::SelectionMode::None);
    flow.set_activate_on_single_click(true);

    let entries = deps.entries.borrow().clone();
    // 最近使用的应用（app kind）优先；不足 8 个用完整应用列表顺序补齐
    let mut picks: Vec<PanelEntry> = recent_entries(&deps.history.borrow(), &entries, 8)
        .into_iter()
        .filter(|e| e.kind == "app")
        .collect();
    for e in &entries {
        if picks.len() >= 8 {
            break;
        }
        if e.kind == "app" && !picks.iter().any(|p| p.payload == e.payload) {
            picks.push(e.clone());
        }
    }
    for e in picks {
        let b = gtk::Button::new();
        b.add_css_class("tile");
        b.set_has_frame(false);
        let v = GtkBox::new(Orientation::Vertical, 4);
        v.set_halign(Align::Center);
        let img = gtk::Image::new();
        img.set_pixel_size(36);
        img.set_from_gicon(&gicon_for_spec(&e.icon_spec));
        let name = gtk::Label::new(Some(&e.title));
        name.add_css_class("tile-name");
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        name.set_max_width_chars(10);
        v.append(&img);
        v.append(&name);
        b.set_child(Some(&v));
        let deps = deps.clone();
        let slot = slot.clone();
        let entry = e;
        b.connect_clicked(move |_| {
            perform_entry(&deps, &slot, &entry);
        });
        flow.append(&b);
    }
    flow
}

/// 能力宫格：深浅色/中心/深链 + 截图·锁屏·夜灯等高频系统命令（4 列）。
fn build_cap_grid(deps: &Rc<Deps>, slot: &Slot) -> gtk::FlowBox {
    const TILES: &[(&str, &str, &str)] = &[
        ("theme:dark", "深色", "weather-clear-night-symbolic"),
        ("theme:light", "浅色", "weather-clear-symbolic"),
        ("page:radio", "广播", "applications-multimedia-symbolic"),
        ("page:autodark", "主题页", "night-light-symbolic"),
        ("sys:screenshot", "截图", "camera-photo-symbolic"),
        ("sys:lock", "锁屏", "system-lock-screen-symbolic"),
        ("sys:night-light", "夜灯", "night-light-symbolic"),
        ("center", "中心", "tools.yihu.desktop"),
    ];
    let flow = gtk::FlowBox::new();
    flow.add_css_class("capgrid");
    flow.set_homogeneous(true);
    flow.set_max_children_per_line(4);
    flow.set_min_children_per_line(4);
    flow.set_column_spacing(6);
    flow.set_row_spacing(6);
    flow.set_selection_mode(gtk::SelectionMode::None);
    flow.set_activate_on_single_click(true);

    for (payload, label, icon) in TILES {
        // 系统插件停用（syscmd 等）时对应 tile 不出现
        if !deps.sys_plugins.borrow().contains(providers::owner_of(payload)) {
            continue;
        }
        let b = gtk::Button::new();
        b.add_css_class("tile");
        let h = GtkBox::new(Orientation::Horizontal, 8);
        h.set_halign(Align::Center);
        let img = gtk::Image::from_icon_name(icon);
        img.set_pixel_size(18);
        let l = gtk::Label::new(Some(label));
        l.add_css_class("tile-name");
        h.append(&img);
        h.append(&l);
        b.set_child(Some(&h));
        let deps = deps.clone();
        let slot = slot.clone();
        let payload = payload.to_string();
        b.connect_clicked(move |_| {
            providers::activate_capability(&payload, &deps.center);
            record(&deps.history, &format!("cap:{payload}"));
            hide_panel(&deps, &slot);
        });
        flow.append(&b);
    }
    flow
}

/// 底部键提示栏
fn key_bar() -> GtkBox {
    let bar = GtkBox::new(Orientation::Horizontal, 10);
    bar.add_css_class("keybar");
    bar.set_halign(Align::Center);
    let item = |key: &str, hint: &str, bar: &GtkBox| {
        let k = gtk::Label::new(Some(key));
        k.add_css_class("kbd");
        let h = gtk::Label::new(Some(hint));
        h.add_css_class("kbd-hint");
        let cell = GtkBox::new(Orientation::Horizontal, 4);
        cell.append(&k);
        cell.append(&h);
        bar.append(&cell);
    };
    item("↑↓", "选择", &bar);
    item("↵", "打开", &bar);
    item("Esc", "关闭", &bar);
    bar
}

struct Row {
    sel_bar: GtkBox,
    icon: gtk::Image,
    col: GtkBox,
    title: gtk::Label,
    sub: gtk::Label,
    badge: gtk::Label,
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

thread_local! {
    /// 当前已挂到 display 的样式表 provider（accent/主题变化时替换）
    static CSS_PROVIDER: std::cell::RefCell<Option<gtk::CssProvider>> =
        const { std::cell::RefCell::new(None) };
}

/// 按「深浅 + 系统强调色」重建注入样式表（UI 重设计：动态主题）。
fn apply_css() {
    let dark = gio::Settings::new("org.gnome.desktop.interface")
        .string("color-scheme")
        .as_str()
        == "prefer-dark";
    let css = crate::theme::build_css(dark, crate::theme::detect_accent());
    CSS_PROVIDER.with(|slot| {
        let display = match gtk::gdk::Display::default() {
            Some(d) => d,
            None => return,
        };
        let mut cur = slot.borrow_mut();
        if let Some(old) = cur.take() {
            gtk::style_context_remove_provider_for_display(&display, &old);
        }
        let provider = gtk::CssProvider::new();
        provider.load_from_string(&css);
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        *cur = Some(provider);
    });
}

#[cfg(test)]
mod pinyin_search_tests {
    use super::*;

    fn entry(title: &str, payload: &str) -> PanelEntry {
        PanelEntry {
            title: title.to_string(),
            subtitle: "应用".into(),
            icon_spec: String::new(),
            kind: "app",
            payload: payload.to_string(),
        }
    }

    fn search(entries: &[PanelEntry], q: &str) -> Vec<String> {
        let mut nuc = build_nucleo(entries);
        settle(&mut nuc);
        settle(&mut nuc);
        nuc.pattern.reparse(
            0,
            q,
            nucleo::pattern::CaseMatching::Smart,
            nucleo::pattern::Normalization::Smart,
            false,
        );
        settle(&mut nuc);
        settle(&mut nuc);
        let snap = nuc.snapshot();
        snap.matched_items(..)
            .map(|it| it.data.title.clone())
            .collect()
    }

    #[test]
    fn pinyin_full_and_initials_hit() {
        let entries = vec![entry("文件管理", "org.gnome.Nautilus.desktop")];
        let titles = search(&entries, "wenjian");
        assert_eq!(titles, vec!["文件管理"], "全拼命中");
        let titles = search(&entries, "wjgl");
        assert_eq!(titles, vec!["文件管理"], "首字母命中");
    }

    #[test]
    fn english_id_tail_hits() {
        let entries = vec![entry("文件", "org.gnome.Nautilus.desktop")];
        assert_eq!(search(&entries, "nautilus"), vec!["文件"], "英文 id 尾段命中");
        // 能力 payload 不进匹配列（冒号被过滤）
        let caps = vec![entry("切换主题", "theme:dark")];
        assert!(search(&caps, "theme").is_empty());
    }

    #[test]
    fn title_column_still_works() {
        let entries = vec![entry("文本编辑器", "org.gnome.TextEditor.desktop")];
        assert_eq!(search(&entries, "编辑"), vec!["文本编辑器"]);
    }
}
