//! 中心「插件」页（2026-10-09 插件中心重构）：市场主入口 + 已装卡片网格。
//!
//! 对标 uTools/ZTools 的插件中心形态：图标卡片网格（40px 图标 + 名称 +
//! 版本 + 权限徽章 + 启停/安装），不再是密集文字行。目录/zip 安装收敛
//! 为「高级安装」一卡。启停/移除/安装/市场逻辑与信号保持不变。
//!
//! M5 插件数据管理（2026-10-11，对标 Rubick「多端数据同步」页）：每张
//! 已装卡片带数据行——文件数/体积统计 + 打开目录 / 导出 zip / 导入 zip /
//! 清空。统计与导入导出走后台线程 + 轮询回填（本页既有惯例）；
//! 导入校验与插件包安装同强度（yihu-core::datactl）。

use adw::prelude::*;
use gtk::glib;
use gtk::{
    Align, Box as GtkBox, Button, Entry, FlowBox, Image, Label, Orientation, Switch,
};
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gtk::gio;
use yihu_core::plugins;

use crate::market::{self, MarketPlugin};
use crate::page_shell;

struct Ui {
    busy: Cell<bool>,
    installed_flow: FlowBox,
    market_flow: FlowBox,
    market_hint: Label,
    state_hint: Label,
    market_plugins: Rc<std::cell::RefCell<Vec<MarketPlugin>>>,
    /// 已装卡片的数据统计行（id → label），后台统计/操作结果回填用
    data_labels: std::cell::RefCell<HashMap<String, Label>>,
    /// 数据统计后台作业回填槽（id → (文件数, 字节数)；None = 目录不存在）
    stats_slot: Arc<Mutex<Option<HashMap<String, Option<(u64, u64)>>>>>,
    stats_pending: Cell<bool>,
    /// 运行状态行（id → label），面板 D-Bus 查询回填
    run_labels: std::cell::RefCell<HashMap<String, Label>>,
    /// 面板 PluginStatus 查询结果槽（原始 JSON）
    run_slot: Arc<Mutex<Option<String>>>,
}

/// 后台任务结果槽（阻塞调用不入 UI 主线程的既有惯例）
type Slot = Arc<Mutex<Option<String>>>;
/// 市场刷新结果槽
type MarketSlot = Arc<Mutex<Option<Result<Vec<MarketPlugin>, String>>>>;

pub fn build_page() -> gtk::Widget {
    let main_box = GtkBox::new(Orientation::Vertical, 14);
    main_box.set_margin_top(20);
    main_box.set_margin_bottom(28);
    main_box.set_margin_start(24);
    main_box.set_margin_end(24);
    main_box.set_valign(Align::Start);

    // ============ 插件市场（主入口）============
    let market_card = card();
    let mtitle = section_header("插件市场", "社区与官方插件；下载前强制核验 sha256 与权限");
    market_card.append(&mtitle.0);
    market_card.append(&mtitle.1);
    let mrow = GtkBox::new(Orientation::Horizontal, 8);
    let url_entry = Entry::new();
    url_entry.set_placeholder_text(Some("registry 地址（留空用默认源）"));
    url_entry.set_hexpand(true);
    let refresh_btn = Button::with_label("刷新");
    refresh_btn.add_css_class("suggested-action");
    mrow.append(&url_entry);
    mrow.append(&refresh_btn);
    market_card.append(&mrow);
    let market_hint = Label::new(None);
    market_hint.add_css_class("dim-label");
    market_hint.add_css_class("caption-sm");
    market_hint.set_halign(Align::Start);
    market_hint.set_wrap(true);
    market_card.append(&market_hint);
    let market_flow = mk_flow();
    market_card.append(&market_flow);
    let market_empty = Label::new(Some("点「刷新」浏览插件；直连失败会自动换镜像源"));
    market_empty.add_css_class("dim-label");
    market_empty.add_css_class("caption-sm");
    market_empty.set_halign(Align::Center);
    market_empty.set_margin_top(8);
    market_flow.insert(&market_empty, -1);
    if let Ok(saved) = yihu_core::paths::read_compatible("market_url") {
        let saved = saved.trim().to_string();
        if !saved.is_empty() {
            url_entry.set_text(&saved);
        }
    }

    // ============ 已安装 ============
    let list_card = card();
    let ltitle = section_header("已安装", "开关停用后下次呼出生效；移除会删除插件目录");
    list_card.append(&ltitle.0);
    list_card.append(&ltitle.1);
    let installed_flow = mk_flow();
    list_card.append(&installed_flow);

    // ============ 高级安装（目录 / zip）============
    let adv_card = card();
    let atitle = section_header("高级安装", "本地开发调试用；正式渠道走上面的市场");
    adv_card.append(&atitle.0);
    adv_card.append(&atitle.1);
    let adv_grid = GtkBox::new(Orientation::Horizontal, 12);
    let (dir_entry, dir_btn) = install_row("插件目录（含 manifest.toml）");
    let (zip_entry, zip_btn) = install_row("插件包 .zip");
    adv_grid.append(&dir_entry);
    adv_grid.append(&dir_btn);
    adv_grid.append(&zip_entry);
    adv_grid.append(&zip_btn);
    adv_card.append(&adv_grid);
    let state_hint = Label::new(None);
    state_hint.add_css_class("dim-label");
    state_hint.add_css_class("caption-sm");
    state_hint.set_halign(Align::Start);
    state_hint.set_wrap(true);
    adv_card.append(&state_hint);

    for c in [&market_card, &list_card, &adv_card] {
        c.set_hexpand(true);
        main_box.append(c);
    }

    let ui = Rc::new(Ui {
        busy: Cell::new(false),
        installed_flow,
        market_flow,
        market_hint,
        state_hint,
        market_plugins: Rc::new(std::cell::RefCell::new(Vec::new())),
        data_labels: std::cell::RefCell::new(HashMap::new()),
        stats_slot: Arc::new(Mutex::new(None)),
        stats_pending: Cell::new(false),
        run_labels: std::cell::RefCell::new(HashMap::new()),
        run_slot: Arc::new(Mutex::new(None)),
    });

    // —— 运行状态自动刷新（3s；面板 /proc 实扫，查询本身线程化）——
    {
        let ui2 = ui.clone();
        glib::timeout_add_local(Duration::from_secs(3), move || {
            ui2.spawn_run_query();
            glib::ControlFlow::Continue
        });
    }

    // —— 信号：目录/zip 安装（后台线程 + 轮询回填）——
    spawn_on_click(dir_btn, dir_entry, ui.clone(), move |src| {
        plugins::install_from_dir(std::path::Path::new(&src))
            .map(|m| format!("已安装：{}（{}）", m.name, m.id))
            .map_err(|e| e.to_string())
    });
    spawn_on_click(zip_btn, zip_entry, ui.clone(), move |src| {
        yihu_core::zipfile::install_from_zip(std::path::Path::new(&src))
            .map(|m| format!("已安装：{}（{}）", m.name, m.id))
    });

    // —— 信号：市场刷新 ——
    {
        let ui = ui.clone();
        let slot: MarketSlot = Arc::new(Mutex::new(None));
        refresh_btn.connect_clicked(move |_| {
            if ui.busy.get() {
                return;
            }
            ui.busy.set(true);
            ui.market_hint.set_text("拉取中…");
            let url = url_entry.text().trim().to_string();
            let url = if url.is_empty() {
                market::DEFAULT_REGISTRY_URL.to_string()
            } else {
                url
            };
            let _ = yihu_core::paths::write_current("market_url", &url);
            let slot2 = slot.clone();
            std::thread::spawn(move || {
                *slot2.lock().unwrap() = Some(market::fetch_registry(&url));
            });
            let ui = ui.clone();
            let slot = slot.clone();
            glib::timeout_add_local(Duration::from_millis(150), move || {
                let Some(res) = slot.lock().unwrap().take() else {
                    return glib::ControlFlow::Continue;
                };
                ui.busy.set(false);
                match res {
                    Ok(list) => {
                        ui.market_hint
                            .set_text(&format!("市场条目 {} 个 · 直连失败自动换镜像", list.len()));
                        *ui.market_plugins.borrow_mut() = list;
                        ui.refresh_market();
                    }
                    Err(e) => ui.market_hint.set_text(&format!("失败：{e}")),
                }
                glib::ControlFlow::Break
            });
        });
    }

    ui.refresh_list();
    page_shell("插件", &crate::scroll_clamp(&main_box, 720))
}

// ---- 构建块 ----

fn card() -> GtkBox {
    let b = GtkBox::new(Orientation::Vertical, 10);
    b.add_css_class("card");
    b.add_css_class("card-pad");
    b
}

/// 小节标题（主标题 + 一句副标题）
fn section_header(title: &str, sub: &str) -> (GtkBox, Label) {
    let t = Label::new(Some(title));
    t.add_css_class("title-3");
    t.set_halign(Align::Start);
    let s = Label::new(Some(sub));
    s.add_css_class("caption-sm");
    s.add_css_class("dim-label");
    s.set_halign(Align::Start);
    s.set_wrap(true);
    let v = GtkBox::new(Orientation::Vertical, 2);
    v.append(&t);
    v.append(&s);
    (v, s)
}

fn mk_flow() -> FlowBox {
    let f = FlowBox::new();
    f.set_homogeneous(true);
    f.set_min_children_per_line(2);
    f.set_max_children_per_line(3);
    f.set_column_spacing(10);
    f.set_row_spacing(10);
    f.set_selection_mode(gtk::SelectionMode::None);
    f.set_activate_on_single_click(false);
    f
}

/// 高级安装行：占位输入 + 安装钮
fn install_row(placeholder: &str) -> (Entry, Button) {
    let e = Entry::new();
    e.set_placeholder_text(Some(placeholder));
    e.set_hexpand(true);
    let b = Button::with_label("安装");
    (e, b)
}

/// 权限徽章行：短名 pill + 中文说明 tooltip（含参数，如「访问网络
/// api.x.com」「写文件 ~/.config/…」）；空权限显示「无权限」灰徽章。
/// 用 WrapBox 自动换行——横向 Box 的行最小宽度 = 全部 pill 之和，会一路
/// 顶进 Stack/窗口最小宽（电台 8 条权限曾把窗口撑到 ~3400px 且缩不小）；
/// pill 另加省略上限兜底，超长参数以 tooltip 兜底展示
fn perm_badges(perms: &[String]) -> adw::WrapBox {
    let row = adw::WrapBox::builder()
        .orientation(gtk::Orientation::Horizontal)
        .child_spacing(6)
        .line_spacing(4)
        .build();
    if perms.is_empty() {
        let l = Label::new(Some("无权限"));
        l.add_css_class("perm-badge");
        l.add_css_class("dim");
        row.append(&l);
        return row;
    }
    for p in perms {
        let l = Label::new(Some(p));
        l.add_css_class("perm-badge");
        l.set_ellipsize(gtk::pango::EllipsizeMode::End);
        l.set_max_width_chars(38);
        l.set_tooltip_text(Some(&yihu_core::permissions::label(p)));
        row.append(&l);
    }
    row
}

/// 插件图标（manifest icon 名 → 主题图标，兜底通用插件图标）
fn plugin_icon(icon_spec: &str) -> Image {
    let img = Image::new();
    img.set_pixel_size(34);
    let name = if icon_spec.trim().is_empty() {
        "application-x-addon-symbolic"
    } else {
        icon_spec
    };
    let icon = gtk::gio::ThemedIcon::new(name);
    img.set_from_gicon(&icon);
    img.add_css_class("plugin-icon");
    img
}

impl Ui {
    /// 重建已装插件卡片网格（&Rc<Self>：卡片内按钮闭包要克隆 Rc 回调刷新）
    fn refresh_list(self: &Rc<Self>) {
        while let Some(child) = self.installed_flow.first_child() {
            self.installed_flow.remove(&child);
        }
        self.data_labels.borrow_mut().clear();
        self.run_labels.borrow_mut().clear();

        let (installed, errors) = plugins::list_installed();
        let state = plugins::PluginsState::load();

        for e in &errors {
            let l = Label::new(Some(&format!("⚠ {e}")));
            l.add_css_class("caption-sm");
            l.add_css_class("error");
            l.set_halign(Align::Start);
            l.set_wrap(true);
            self.installed_flow.insert(&l, -1);
        }

        if installed.is_empty() {
            let empty = Label::new(Some("暂无插件——从上方市场安装，或用高级安装导入本地插件"));
            empty.add_css_class("dim-label");
            empty.add_css_class("caption-sm");
            empty.set_halign(Align::Center);
            empty.set_margin_top(8);
            self.installed_flow.insert(&empty, -1);
            return;
        }

        for inst in &installed {
            let id = inst.manifest.id.clone();
            let enabled = !state.is_disabled(&id);

            let card = GtkBox::new(Orientation::Vertical, 6);
            card.add_css_class("plugin-card");

            // 主行：图标 + 名称/版本 + 开关
            let head = GtkBox::new(Orientation::Horizontal, 10);
            head.append(&plugin_icon(&inst.manifest.icon));
            let col = GtkBox::new(Orientation::Vertical, 1);
            col.set_valign(Align::Center);
            col.set_hexpand(true);
            let t = Label::new(Some(&inst.manifest.name));
            t.add_css_class("row-title");
            t.set_halign(Align::Start);
            t.set_ellipsize(gtk::pango::EllipsizeMode::End);
            let v = Label::new(Some(&format!("v{} · {}", inst.manifest.version, inst.manifest.id)));
            v.add_css_class("caption-sm");
            v.add_css_class("dim-label");
            v.set_halign(Align::Start);
            v.set_ellipsize(gtk::pango::EllipsizeMode::End);
            col.append(&t);
            col.append(&v);
            head.append(&col);
            let sw = Switch::new();
            sw.set_active(enabled);
            sw.set_valign(Align::Center);
            {
                let id = id.clone();
                sw.connect_active_notify(move |sw| {
                    let mut st = plugins::PluginsState::load();
                    st.set_disabled(&id, !sw.is_active());
                    let _ = st.save_to(&plugins::state_path());
                });
            }
            head.append(&sw);
            card.append(&head);

            // 权限徽章行
            card.append(&perm_badges(&inst.manifest.permissions));

            // 运行状态行（M5 追加）：运行中/未运行 + 进程数/内存/CPU + 停止
            card.append(&self.run_row(&id));

            // 数据行（M5）：统计 + 打开/导出/导入/清空
            card.append(&self.data_row(&id));

            // 尾行：入口信息 + 移除
            let foot = GtkBox::new(Orientation::Horizontal, 6);
            let entry_lbl = Label::new(Some(&inst.manifest.entry));
            entry_lbl.add_css_class("caption-sm");
            entry_lbl.add_css_class("dim-label");
            entry_lbl.set_hexpand(true);
            entry_lbl.set_halign(Align::Start);
            entry_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            foot.append(&entry_lbl);
            let rm = Button::with_label("移除");
            rm.add_css_class("flat");
            rm.add_css_class("caption-sm");
            {
                let id = id.clone();
                let ui_card = self.clone();
                rm.connect_clicked(move |_| {
                    let _ = plugins::remove_plugin(&id);
                    let mut st = plugins::PluginsState::load();
                    st.set_disabled(&id, false);
                    let _ = st.save_to(&plugins::state_path());
                    ui_card.refresh_list();
                });
            }
            foot.append(&rm);
            card.append(&foot);

            self.installed_flow.insert(&card, -1);
        }
        self.spawn_all_stats();
    }

    // ---- 插件数据管理（M5，对标 Rubick 数据同步页）----

    /// 构建单张卡片的数据行：统计文本 + 四个图标按钮（打开/导出/导入/清空）
    fn data_row(self: &Rc<Self>, id: &str) -> GtkBox {
        let row = GtkBox::new(Orientation::Horizontal, 4);
        let stat = Label::new(Some("统计中…"));
        stat.add_css_class("caption-sm");
        stat.add_css_class("dim-label");
        stat.set_halign(Align::Start);
        stat.set_hexpand(true);
        stat.set_ellipsize(gtk::pango::EllipsizeMode::End);
        row.append(&stat);
        self.data_labels.borrow_mut().insert(id.to_string(), stat);

        row.append(&self.data_btn(
            id,
            "folder-open-symbolic",
            "打开数据目录",
            false,
            open_data_dir,
        ));
        row.append(&self.data_btn(
            id,
            "document-save-symbolic",
            "导出数据为 zip",
            false,
            export_data_dialog,
        ));
        row.append(&self.data_btn(
            id,
            "document-revert-symbolic",
            "从 zip 导入（整体替换现有数据）",
            false,
            import_data_dialog,
        ));
        row.append(&self.data_btn(id, "user-trash-symbolic", "清空数据", true, clear_data_confirmed));
        row
    }

    fn data_btn(
        self: &Rc<Self>,
        id: &str,
        icon: &str,
        tip: &str,
        destructive: bool,
        action: fn(Rc<Ui>, Option<gtk::Window>, String),
    ) -> Button {
        let btn = Button::new();
        let img = Image::from_icon_name(icon);
        btn.set_child(Some(&img));
        btn.add_css_class("flat");
        btn.add_css_class("caption-sm");
        if destructive {
            btn.add_css_class("destructive-action");
        }
        btn.set_tooltip_text(Some(tip));
        let ui = self.clone();
        let id = id.to_string();
        btn.connect_clicked(move |b| {
            if ui.busy.get() {
                return;
            }
            // FileDialog / AlertDialog 的父窗口；拿不到则以无父窗口弹出。
            // 窗口是引用计数对象，Clone 后可安全进异步回调。
            let win = b.root().and_then(|r| r.dynamic_cast::<gtk::Window>().ok());
            action(ui.clone(), win, id.clone());
        });
        btn
    }

    /// 后台统计全部已装插件的数据量，完成后逐卡回填
    fn spawn_all_stats(self: &Rc<Self>) {
        if self.stats_pending.get() {
            return;
        }
        self.stats_pending.set(true);
        let slot = self.stats_slot.clone();
        std::thread::spawn(move || {
            let (installed, _) = plugins::list_installed();
            let mut map = HashMap::new();
            for inst in &installed {
                let id = inst.manifest.id.clone();
                let s = yihu_core::datactl::data_stats(&plugins::plugin_data_dir(&id));
                map.insert(id, s);
            }
            *slot.lock().unwrap() = Some(map);
        });
        let ui = self.clone();
        glib::timeout_add_local(Duration::from_millis(150), move || {
            let Some(map) = ui.stats_slot.lock().unwrap().take() else {
                return glib::ControlFlow::Continue;
            };
            ui.stats_pending.set(false);
            for (id, s) in &map {
                if let Some(lbl) = ui.data_labels.borrow().get(id) {
                    lbl.set_text(&fmt_data_stats(*s));
                }
            }
            glib::ControlFlow::Break
        });
    }

    /// 运行状态查询（面板 D-Bus PluginStatus，/proc 实扫聚合）
    fn spawn_run_query(self: &Rc<Self>) {
        let slot = self.run_slot.clone();
        std::thread::spawn(move || {
            let out = query_plugin_status();
            *slot.lock().unwrap() = Some(out);
        });
        let ui = self.clone();
        glib::timeout_add_local(Duration::from_millis(200), move || {
            let Some(raw) = ui.run_slot.lock().unwrap().take() else {
                return glib::ControlFlow::Continue;
            };
            apply_run_status(&ui, &raw);
            glib::ControlFlow::Break
        });
    }

    /// 运行状态行：状态文本 + 停止按钮
    fn run_row(self: &Rc<Self>, id: &str) -> GtkBox {
        let row = GtkBox::new(Orientation::Horizontal, 6);
        let lbl = Label::new(Some("○ 状态查询中…"));
        lbl.add_css_class("caption-sm");
        lbl.set_halign(Align::Start);
        lbl.set_hexpand(true);
        lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
        row.append(&lbl);
        self.run_labels.borrow_mut().insert(id.to_string(), lbl);

        let stop = Button::with_label("停止");
        stop.add_css_class("flat");
        stop.add_css_class("caption-sm");
        stop.set_tooltip_text(Some("终止该插件的全部进程（不计失败，可重新呼出）"));
        {
            let id_owned = id.to_string();
            stop.connect_clicked(move |_| {
                let id2 = id_owned.clone();
                std::thread::spawn(move || {
                    let params = (id2,).to_variant();
                    let _ = panel_call("PluginStop", Some(params));
                });
            });
        }
        row.append(&stop);
        row
    }

    /// 数据行内联反馈（导出中…/已导出 等），不打断卡片布局
    fn set_stat(self: &Rc<Self>, id: &str, text: &str) {
        if let Some(lbl) = self.data_labels.borrow().get(id) {
            lbl.set_text(text);
        }
    }

    /// 重建市场卡片网格
    fn refresh_market(self: &Rc<Self>) {
        while let Some(child) = self.market_flow.first_child() {
            self.market_flow.remove(&child);
        }
        let list = self.market_plugins.borrow().clone();
        if list.is_empty() {
            let empty = Label::new(Some("（空）"));
            empty.add_css_class("dim-label");
            empty.add_css_class("caption-sm");
            empty.set_halign(Align::Center);
            self.market_flow.insert(&empty, -1);
            return;
        }
        for p in &list {
            let card = GtkBox::new(Orientation::Vertical, 6);
            card.add_css_class("plugin-card");

            let head = GtkBox::new(Orientation::Horizontal, 10);
            head.append(&plugin_icon("application-x-addon"));
            let col = GtkBox::new(Orientation::Vertical, 1);
            col.set_valign(Align::Center);
            col.set_hexpand(true);
            let t = Label::new(Some(&p.name));
            t.add_css_class("row-title");
            t.set_halign(Align::Start);
            t.set_ellipsize(gtk::pango::EllipsizeMode::End);
            let v = Label::new(Some(&format!("v{} · {}", p.version, p.id)));
            v.add_css_class("caption-sm");
            v.add_css_class("dim-label");
            v.set_halign(Align::Start);
            v.set_ellipsize(gtk::pango::EllipsizeMode::End);
            col.append(&t);
            col.append(&v);
            head.append(&col);
            card.append(&head);

            if !p.desc.is_empty() {
                let d = Label::new(Some(&p.desc));
                d.add_css_class("caption-sm");
                d.add_css_class("dim-label");
                d.set_halign(Align::Start);
                d.set_ellipsize(gtk::pango::EllipsizeMode::End);
                d.set_lines(2);
                d.set_wrap(true);
                card.append(&d);
            }

            let mid = GtkBox::new(Orientation::Horizontal, 6);
            let badges = perm_badges(&p.permissions);
            badges.set_hexpand(true);
            badges.set_halign(Align::Start);
            mid.append(&badges);
            let install = Button::with_label("安装");
            install.add_css_class("suggested-action");
            install.add_css_class("pill");
            {
                let p = p.clone();
                let ui_row = self.clone();
                install.connect_clicked(move |_| {
                    install_market_plugin(&ui_row, p.clone());
                });
            }
            mid.append(&install);
            card.append(&mid);

            self.market_flow.insert(&card, -1);
        }
    }
}

/// 通用「点按钮 → 后台执行 → 轮询回填」模式（目录/zip 安装共用）
fn spawn_on_click<F>(btn: Button, entry: Entry, ui: Rc<Ui>, f: F)
where
    F: Fn(String) -> Result<String, String> + Send + Sync + 'static,
{
    let f = Arc::new(f);
    let slot: Slot = Arc::new(Mutex::new(None));
    btn.connect_clicked(move |_| {
        let src = entry.text().to_string();
        if src.trim().is_empty() || ui.busy.get() {
            return;
        }
        ui.busy.set(true);
        ui.state_hint.set_text("安装中…");
        let slot2 = slot.clone();
        let f = f.clone();
        std::thread::spawn(move || {
            *slot2.lock().unwrap() = Some(match f(src.trim().to_string()) {
                Ok(msg) => msg,
                Err(e) => format!("失败：{e}"),
            });
        });
        let ui = ui.clone();
        let slot = slot.clone();
        glib::timeout_add_local(Duration::from_millis(150), move || {
            if let Some(res) = slot.lock().unwrap().take() {
                ui.busy.set(false);
                ui.state_hint.set_text(&res);
                ui.refresh_list();
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
    });
}

/// 市场安装：下载 → 体积核对 → sha256 核验 → 原子安装（后台线程）。
fn install_market_plugin(ui: &Rc<Ui>, p: MarketPlugin) {
    if ui.busy.get() {
        return;
    }
    ui.busy.set(true);
    ui.state_hint.set_text(&format!("下载 {}…", p.name));
    let slot: Slot = Arc::new(Mutex::new(None));
    let slot2 = slot.clone();
    std::thread::spawn(move || {
        let tmp =
            std::env::temp_dir().join(format!("yihu-market-{}-{}.zip", std::process::id(), p.id));
        let res = (|| -> Result<String, String> {
            let n = market::download_to(&p.url, &tmp)?;
            if p.size > 0 && n != p.size {
                return Err(format!("体积不符：期望 {} 字节，实际 {n}", p.size));
            }
            let m = yihu_core::zipfile::install_from_zip_verified(&tmp, &p.sha256, &p.url)?;
            // 授权时点闭环：包内实际权限必须与市场条目展示权限一致，
            // 否则回滚已安装内容并拒装
            if !market::permissions_match(&p.permissions, &m.permissions) {
                let _ = yihu_core::plugins::remove_plugin(&m.id);
                return Err(format!(
                    "安全拒绝：包内权限（{}）与市场条目声明（{}）不符",
                    if m.permissions.is_empty() { "无".to_string() } else { m.permissions.join("、") },
                    if p.permissions.is_empty() { "无".to_string() } else { p.permissions.join("、") },
                ));
            }
            Ok(format!("已安装：{}（{}）", m.name, m.id))
        })();
        let _ = std::fs::remove_file(&tmp);
        *slot2.lock().unwrap() = Some(match res {
            Ok(msg) => msg,
            Err(e) => format!("失败：{e}"),
        });
    });
    let ui = ui.clone();
    let slot = slot.clone();
    glib::timeout_add_local(Duration::from_millis(150), move || {
        if let Some(res) = slot.lock().unwrap().take() {
            ui.busy.set(false);
            ui.state_hint.set_text(&res);
            ui.refresh_list();
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    });
}

// ---- 插件数据管理动作（M5）----

/// 打开数据目录（文件管理器）。目录不存在则先建（0700）。
fn open_data_dir(_ui: Rc<Ui>, _win: Option<gtk::Window>, id: String) {
    let Ok(dir) = plugins::ensure_plugin_data_dir(&id) else {
        return;
    };
    let _ = std::process::Command::new("xdg-open")
        .arg(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// 导出数据：FileDialog::save 选位置 → 后台打 zip → 行内回填。
fn export_data_dialog(ui: Rc<Ui>, win: Option<gtk::Window>, id: String) {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let dlg = gtk::FileDialog::new();
    dlg.set_initial_name(Some(&format!("yihu-{id}-data-{ts}.zip")));
    dlg.save(win.as_ref(), gtk::gio::Cancellable::NONE, move |res| match res {
        Ok(file) => {
            let Some(path) = file.path() else { return };
            if ui.busy.get() {
                return;
            }
            ui.busy.set(true);
            ui.set_stat(&id, "导出中…");
            let data_dir = plugins::plugin_data_dir(&id);
            let slot: Arc<Mutex<Option<Result<String, String>>>> = Arc::new(Mutex::new(None));
            let slot2 = slot.clone();
            let id2 = id.clone();
            std::thread::spawn(move || {
                let r = yihu_core::datactl::export_data_zip(&data_dir, &id2, &path)
                    .map(|n| format!("已导出 {n} 个文件 → {}", path.display()));
                *slot2.lock().unwrap() = Some(r);
            });
            let ui = ui.clone();
            let id = id.clone();
            glib::timeout_add_local(Duration::from_millis(150), move || {
                let Some(r) = slot.lock().unwrap().take() else {
                    return glib::ControlFlow::Continue;
                };
                ui.busy.set(false);
                match r {
                    Ok(msg) => ui.set_stat(&id, &msg),
                    Err(e) => ui.set_stat(&id, &format!("导出失败：{e}")),
                }
                glib::ControlFlow::Break
            });
        }
        // 用户取消（Esc / 关闭）静默返回
        Err(e) if dialog_dismissed(&e) => {}
        Err(e) => ui.set_stat(&id, &format!("选择保存位置失败：{e}")),
    });
}

/// 导入数据：确认覆盖 → FileDialog::open 选 zip → 后台整目录替换。
fn import_data_dialog(ui: Rc<Ui>, win: Option<gtk::Window>, id: String) {
    confirm_action(
        win,
        "导入数据？",
        &format!("将从 zip 导入「{id}」的数据，现有数据会被整体替换。"),
        "选择文件并导入",
        false,
        ui.clone(),
        id,
        import_data_pick,
    );
}

fn import_data_pick(ui: Rc<Ui>, win: Option<gtk::Window>, id: String) {
    let dlg = gtk::FileDialog::new();
    dlg.open(win.as_ref(), gtk::gio::Cancellable::NONE, move |res| match res {
        Ok(file) => {
            let Some(path) = file.path() else { return };
            if ui.busy.get() {
                return;
            }
            ui.busy.set(true);
            ui.set_stat(&id, "导入中…");
            let data_dir = plugins::plugin_data_dir(&id);
            let slot: Arc<Mutex<Option<Result<u64, String>>>> = Arc::new(Mutex::new(None));
            let slot2 = slot.clone();
            let id2 = id.clone();
            std::thread::spawn(move || {
                let r = yihu_core::datactl::import_data_zip(&path, &id2, &data_dir);
                *slot2.lock().unwrap() = Some(r);
            });
            let ui = ui.clone();
            let id = id.clone();
            glib::timeout_add_local(Duration::from_millis(150), move || {
                let Some(r) = slot.lock().unwrap().take() else {
                    return glib::ControlFlow::Continue;
                };
                ui.busy.set(false);
                match r {
                    Ok(n) => ui.set_stat(&id, &format!("已导入 {n} 个文件")),
                    Err(e) => ui.set_stat(&id, &format!("导入失败：{e}")),
                }
                glib::ControlFlow::Break
            });
        }
        // 用户取消（Esc / 关闭）静默返回
        Err(e) if dialog_dismissed(&e) => {}
        Err(e) => ui.set_stat(&id, &format!("选择文件失败：{e}")),
    });
}

/// 清空数据：确认（破坏性）→ 后台清空重建 → 行内回填「空目录」。
fn clear_data_confirmed(ui: Rc<Ui>, win: Option<gtk::Window>, id: String) {
    confirm_action(
        win,
        "清空数据？",
        &format!("将删除「{id}」的全部插件数据文件，不可恢复。"),
        "清空",
        true,
        ui.clone(),
        id,
        clear_data_action,
    );
}

fn clear_data_action(ui: Rc<Ui>, _win: Option<gtk::Window>, id: String) {
    if ui.busy.get() {
        return;
    }
    ui.busy.set(true);
    ui.set_stat(&id, "清空中…");
    let slot: Arc<Mutex<Option<Result<(), String>>>> = Arc::new(Mutex::new(None));
    let slot2 = slot.clone();
    let id2 = id.clone();
    std::thread::spawn(move || {
        let r = yihu_core::datactl::clear_data(&plugins::plugin_data_dir(&id2))
            .map_err(|e| e.to_string());
        *slot2.lock().unwrap() = Some(r);
    });
    let ui = ui.clone();
    let id = id.clone();
    glib::timeout_add_local(Duration::from_millis(150), move || {
        let Some(r) = slot.lock().unwrap().take() else {
            return glib::ControlFlow::Continue;
        };
        ui.busy.set(false);
        match r {
            Ok(()) => ui.set_stat(&id, "空目录"),
            Err(e) => ui.set_stat(&id, &format!("清空失败：{e}")),
        }
        glib::ControlFlow::Break
    });
}

/// 两键确认弹窗（取消 / 确认）：确认后以 (ui, win, id) 继续动作。
/// 用 adw::MessageDialog（GtkAlertDialogButton 属 GTK 4.14，本机特性集
/// 未开）；确认响应即 "ok"。
fn confirm_action(
    win: Option<gtk::Window>,
    title: &str,
    detail: &str,
    ok_label: &str,
    destructive: bool,
    ui: Rc<Ui>,
    id: String,
    cont: fn(Rc<Ui>, Option<gtk::Window>, String),
) {
    let dlg = adw::MessageDialog::new(win.as_ref(), Some(title), Some(detail));
    dlg.add_response("cancel", "取消");
    dlg.set_close_response("cancel");
    dlg.add_response("ok", ok_label);
    dlg.set_response_appearance(
        "ok",
        if destructive {
            adw::ResponseAppearance::Destructive
        } else {
            adw::ResponseAppearance::Suggested
        },
    );
    dlg.choose(gtk::gio::Cancellable::NONE, move |res| {
        if res.as_str() == "ok" {
            cont(ui, win, id);
        }
    });
}

/// FileDialog 的「用户取消/关闭」错误（静默忽略，不算失败）
fn dialog_dismissed(e: &glib::Error) -> bool {
    e.matches(gtk::DialogError::Cancelled) || e.matches(gtk::DialogError::Dismissed)
}

/// 统计文本：目录不存在 → 无数据；空 → 空目录；否则 N 个文件 · 体积
fn fmt_data_stats(s: Option<(u64, u64)>) -> String {
    match s {
        None => "无数据".into(),
        Some((0, _)) => "空目录".into(),
        Some((n, b)) => format!("{n} 个文件 · {}", fmt_bytes(b)),
    }
}

fn fmt_bytes(b: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut u = 0usize;
    while v >= 1024.0 && u < units.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", units[u])
    }
}

// ---- 插件运行状态（面板 D-Bus） --------------------------------------------

/// 面板 D-Bus 方法调用（tools.yihu.Panel）
fn panel_call(method: &str, params: Option<glib::Variant>) -> Result<glib::Variant, glib::Error> {
    let conn = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)?;
    conn.call_sync(
        Some("tools.yihu.Panel"),
        "/tools/yihu/Panel",
        "tools.yihu.Panel",
        method,
        params.as_ref(),
        None,
        gio::DBusCallFlags::NONE,
        3000,
        gio::Cancellable::NONE,
    )
}

fn query_plugin_status() -> String {
    match panel_call("PluginStatus", None) {
        Ok(v) => v.str().unwrap_or("[]").to_string(),
        Err(_) => "[]".into(),
    }
}

/// 解析状态 JSON 并回填各卡片状态行
fn apply_run_status(ui: &Rc<Ui>, raw: &str) {
    let Ok(items) = serde_json::from_str::<Vec<serde_json::Value>>(raw) else {
        return;
    };
    for it in items {
        let Some(id) = it.get("id").and_then(|x| x.as_str()) else { continue };
        let lbl = ui.run_labels.borrow().get(id).cloned();
        let running = it.get("running").and_then(|x| x.as_bool()).unwrap_or(false);
        let resident = it.get("resident").and_then(|x| x.as_bool()).unwrap_or(false);
        let enabled = it.get("enabled").and_then(|x| x.as_bool()).unwrap_or(true);
        let procs = it.get("procs").and_then(|x| x.as_u64()).unwrap_or(0);
        let rss_kb = it.get("rss_kb").and_then(|x| x.as_u64()).unwrap_or(0);
        let cpu = it.get("cpu_percent").and_then(|x| x.as_f64()).unwrap_or(0.0);
        let Some(lbl) = lbl else { continue };
        let text = if running {
            let tag = if resident { "（常驻）" } else { "" };
            format!("● 运行中{tag} · {procs} 进程 · {:.1} MB · CPU {cpu:.0}%", rss_kb as f64 / 1024.0)
        } else if !enabled {
            "○ 已停用".to_string()
        } else if resident {
            "○ 未运行 · 常驻位空闲".to_string()
        } else {
            "○ 未运行 · 呼出面板时拉起".to_string()
        };
        lbl.set_text(&text);
        lbl.remove_css_class("success");
        if running {
            lbl.add_css_class("success");
        }
    }
}
