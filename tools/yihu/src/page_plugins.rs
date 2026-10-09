//! 中心「插件」页（2026-10-09 插件中心重构）：市场主入口 + 已装卡片网格。
//!
//! 对标 uTools/ZTools 的插件中心形态：图标卡片网格（40px 图标 + 名称 +
//! 版本 + 权限徽章 + 启停/安装），不再是密集文字行。目录/zip 安装收敛
//! 为「高级安装」一卡。启停/移除/安装/市场逻辑与信号保持不变。

use adw::prelude::*;
use gtk::glib;
use gtk::{
    Align, Box as GtkBox, Button, Entry, FlowBox, Image, Label, Orientation, Switch,
};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
    });

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
/// api.x.com」「写文件 ~/.config/…」）；空权限显示「无权限」灰徽章
fn perm_badges(perms: &[String]) -> GtkBox {
    let row = GtkBox::new(Orientation::Horizontal, 4);
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
