//! 中心「插件」页：已装插件列表、本地目录/zip 安装、市场 v0、
//! 启停与权限明示。
//!
//! M4 起宿主强制安全：插件经 bwrap 沙箱拉起（fail-closed），系统能力
//! 须经宿主能力代理并受 manifest 权限约束。市场/网络只在中心应用
//! 的后台线程发生，呼出路径零 IO 底线不变。

use adw::prelude::*;
use gtk::glib;
use gtk::{Align, Box as GtkBox, Button, Entry, Label, Orientation, Switch};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use yihu_core::plugins;

use crate::market::{self, MarketPlugin};
use crate::{page_shell, scroll_clamp};

struct Ui {
    busy: Cell<bool>,
    list_box: GtkBox,
    state_hint: Label,
    market_box: GtkBox,
    market_hint: Label,
    market_plugins: Rc<std::cell::RefCell<Vec<MarketPlugin>>>,
}

/// 后台任务结果槽（阻塞调用不入 UI 主线程的既有惯例）
type Slot = Arc<Mutex<Option<String>>>;
/// 市场刷新结果槽
type MarketSlot = Arc<Mutex<Option<Result<Vec<MarketPlugin>, String>>>>;

pub fn build_page() -> gtk::Widget {
    // —— 卡片：说明 ——
    let help_card = card();
    let help_title = Label::new(Some("什么是插件"));
    help_title.add_css_class("sec-title");
    help_title.set_halign(Align::Start);
    help_card.append(&help_title);
    for line in [
        "插件是带 manifest.toml 的独立程序：呼出面板把搜索词交给它，",
        "把结果并入候选列表；面板收起时插件进程随即结束，不占资源。",
        "M4 起宿主强制安全：插件经 bwrap 沙箱拉起（白名单外访问失败、",
        "fail-closed），系统能力须经宿主代理并受 manifest 权限约束。",
    ] {
        let l = Label::new(Some(line));
        l.add_css_class("dim-label");
        l.add_css_class("caption-sm");
        l.set_halign(Align::Start);
        l.set_wrap(true);
        help_card.append(&l);
    }

    // —— 卡片：从目录安装 ——
    let (install_card, path_entry, install_btn, install_hint) = install_card_common(
        "从目录安装",
        "/路径/插件目录（内含 manifest.toml）",
    );

    // —— 卡片：从 zip 安装 ——
    let (zip_card, zip_entry, zip_btn, _zip_hint) =
        install_card_common("从 zip 安装", "/路径/插件包.zip（64 MiB 内，安全校验后原子安装）");

    // —— 卡片：市场 v0 ——
    let market_card = card();
    let market_title = Label::new(Some("插件市场（v0 实验性）"));
    market_title.add_css_class("sec-title");
    market_title.set_halign(Align::Start);
    market_card.append(&market_title);
    let mrow = GtkBox::new(Orientation::Horizontal, 8);
    let url_entry = Entry::new();
    url_entry.set_placeholder_text(Some("registry.json 地址（GitHub PR 审核维护）"));
    url_entry.set_hexpand(true);
    let refresh_btn = Button::with_label("刷新");
    mrow.append(&url_entry);
    mrow.append(&refresh_btn);
    market_card.append(&mrow);
    let market_hint = Label::new(None);
    market_hint.add_css_class("dim-label");
    market_hint.add_css_class("caption-sm");
    market_hint.set_halign(Align::Start);
    market_hint.set_wrap(true);
    market_card.append(&market_hint);
    let market_box = GtkBox::new(Orientation::Vertical, 8);
    market_card.append(&market_box);
    if let Ok(saved) = yihu_core::paths::read_compatible("market_url") {
        let saved = saved.trim().to_string();
        if !saved.is_empty() {
            url_entry.set_text(&saved);
        }
    }

    // —— 卡片：已装插件 ——
    let list_card = card();
    let list_title = Label::new(Some("已安装插件"));
    list_title.add_css_class("sec-title");
    list_title.set_halign(Align::Start);
    list_card.append(&list_title);
    let list_box = GtkBox::new(Orientation::Vertical, 8);
    list_card.append(&list_box);
    let list_hint = Label::new(Some(
        "启停立即生效（下次呼出按新状态拉起）；移除会删除插件目录。",
    ));
    list_hint.add_css_class("dim-label");
    list_hint.add_css_class("caption-sm");
    list_hint.set_wrap(true);
    list_hint.set_halign(Align::Start);
    list_card.append(&list_hint);

    // —— 布局 ——
    let main_box = GtkBox::new(Orientation::Vertical, 14);
    main_box.set_margin_top(20);
    main_box.set_margin_bottom(28);
    main_box.set_margin_start(24);
    main_box.set_margin_end(24);
    main_box.set_valign(Align::Start);
    for c in [&help_card, &install_card, &zip_card, &market_card, &list_card] {
        c.set_hexpand(true);
        main_box.append(c);
    }

    let ui = Rc::new(Ui {
        busy: Cell::new(false),
        list_box,
        state_hint: install_hint.clone(),
        market_box,
        market_hint,
        market_plugins: Rc::new(std::cell::RefCell::new(Vec::new())),
    });

    // —— 信号：目录安装（后台线程 + 轮询回填）——
    spawn_on_click(install_btn, path_entry, ui.clone(), move |src| {
        plugins::install_from_dir(std::path::Path::new(&src))
            .map(|m| format!("已安装：{}（{}）", m.name, m.id))
            .map_err(|e| e.to_string())
    });

    // —— 信号：zip 安装 ——
    spawn_on_click(zip_btn, zip_entry, ui.clone(), move |src| {
        yihu_core::zipfile::install_from_zip(std::path::Path::new(&src))
            .map(|m| format!("已安装：{}（{}）· sha256 已记入收据", m.name, m.id))
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
                        ui.market_hint.set_text(&format!("市场条目 {} 个", list.len()));
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
    page_shell("插件", &scroll_clamp(&main_box, 720))
}

/// 目录/zip 两张安装卡的公共结构
fn install_card_common(
    title: &str,
    placeholder: &str,
) -> (GtkBox, Entry, Button, Label) {
    let card_box = card();
    let t = Label::new(Some(title));
    t.add_css_class("sec-title");
    t.set_halign(Align::Start);
    card_box.append(&t);
    let row = GtkBox::new(Orientation::Horizontal, 8);
    let entry = Entry::new();
    entry.set_placeholder_text(Some(placeholder));
    entry.set_hexpand(true);
    let btn = Button::with_label("安装");
    row.append(&entry);
    row.append(&btn);
    card_box.append(&row);
    let hint = Label::new(None);
    hint.add_css_class("dim-label");
    hint.add_css_class("caption-sm");
    hint.set_halign(Align::Start);
    hint.set_wrap(true);
    card_box.append(&hint);
    (card_box, entry, btn, hint)
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

impl Ui {
    /// 重建已装插件列表（数量小，直接重建；容器内旧行全部移除）。
    fn refresh_list(&self) {
        while let Some(child) = self.list_box.first_child() {
            self.list_box.remove(&child);
        }

        let (installed, errors) = plugins::list_installed();
        let state = plugins::PluginsState::load();

        for e in &errors {
            let l = Label::new(Some(&format!("⚠ {e}")));
            l.add_css_class("caption-sm");
            l.add_css_class("error");
            l.set_halign(Align::Start);
            l.set_wrap(true);
            self.list_box.append(&l);
        }

        if installed.is_empty() {
            let empty = Label::new(Some(
                "暂无插件。构建示例插件后（scripts/install-example-plugins.sh）",
            ));
            empty.add_css_class("dim-label");
            empty.add_css_class("caption-sm");
            empty.set_halign(Align::Start);
            empty.set_wrap(true);
            self.list_box.append(&empty);
            return;
        }

        for inst in &installed {
            let id = inst.manifest.id.clone();
            let enabled = !state.is_disabled(&id);

            let row = GtkBox::new(Orientation::Horizontal, 8);
            let title_col = GtkBox::new(Orientation::Vertical, 2);
            let t = Label::new(Some(&format!(
                "{} {}",
                inst.manifest.name, inst.manifest.version
            )));
            t.add_css_class("row-title");
            t.set_halign(Align::Start);
            let perms = if inst.manifest.permissions.is_empty() {
                "权限：无".to_string()
            } else {
                format!(
                    "权限：{}",
                    inst.manifest
                        .permissions
                        .iter()
                        .map(|p| format!("{p}（{}）", yihu_core::permissions::label(p)))
                        .collect::<Vec<_>>()
                        .join("、")
                )
            };
            let sub = Label::new(Some(&format!("{perms} · 入口 {}", inst.manifest.entry)));
            sub.add_css_class("caption-sm");
            sub.add_css_class("dim-label");
            sub.set_halign(Align::Start);
            title_col.append(&t);
            title_col.append(&sub);
            title_col.set_hexpand(true);
            title_col.set_valign(Align::Center);
            row.append(&title_col);

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
            row.append(&sw);

            let rm = Button::with_label("移除");
            rm.set_valign(Align::Center);
            {
                let id = id.clone();
                rm.connect_clicked(move |_| {
                    let _ = plugins::remove_plugin(&id);
                    let mut st = plugins::PluginsState::load();
                    st.set_disabled(&id, false);
                    let _ = st.save_to(&plugins::state_path());
                });
            }
            row.append(&rm);

            self.list_box.append(&row);
        }
    }

    /// 重建市场条目列表（&Rc<Self> 接收者：行内闭包需要克隆 Rc）
    fn refresh_market(self: &Rc<Self>) {
        let ui = self.clone();
        while let Some(child) = self.market_box.first_child() {
            self.market_box.remove(&child);
        }
        let list = self.market_plugins.borrow().clone();
        if list.is_empty() {
            let empty = Label::new(Some("（空）"));
            empty.add_css_class("dim-label");
            empty.add_css_class("caption-sm");
            empty.set_halign(Align::Start);
            self.market_box.append(&empty);
            return;
        }
        for p in &list {
            let row = GtkBox::new(Orientation::Horizontal, 8);
            let col = GtkBox::new(Orientation::Vertical, 2);
            let t = Label::new(Some(&format!("{} {}", p.name, p.version)));
            t.add_css_class("row-title");
            t.set_halign(Align::Start);
            let perms = if p.permissions.is_empty() {
                "权限：无".to_string()
            } else {
                format!(
                    "权限：{}",
                    p.permissions
                        .iter()
                        .map(|x| format!("{x}（{}）", yihu_core::permissions::label(x)))
                        .collect::<Vec<_>>()
                        .join("、")
                )
            };
            let sub = Label::new(Some(&format!(
                "{} · {} · {perms} · sha256 前 8 位 {}",
                p.desc,
                p.id,
                &p.sha256[..8.min(p.sha256.len())]
            )));
            sub.add_css_class("caption-sm");
            sub.add_css_class("dim-label");
            sub.set_halign(Align::Start);
            sub.set_wrap(true);
            col.append(&t);
            col.append(&sub);
            col.set_hexpand(true);
            col.set_valign(Align::Center);
            row.append(&col);

            let install = Button::with_label("安装");
            install.set_valign(Align::Center);
            {
                let p = p.clone();
                let ui_row = ui.clone();
                install.connect_clicked(move |_| {
                    install_market_plugin(&ui_row, p.clone());
                });
            }
            row.append(&install);
            self.market_box.append(&row);
        }
    }
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
        let tmp = std::env::temp_dir().join(format!("yihu-market-{}-{}.zip", std::process::id(), p.id));
        let res = (|| -> Result<String, String> {
            let n = market::download_to(&p.url, &tmp)?;
            if p.size > 0 && n != p.size {
                return Err(format!("体积不符：期望 {} 字节，实际 {n}", p.size));
            }
            let m = yihu_core::zipfile::install_from_zip_verified(&tmp, &p.sha256, &p.url)?;
            // 授权时点闭环（审查 I-3）：包内实际权限必须与市场条目展示
            // 权限一致，否则回滚已安装内容并拒装
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

fn card() -> GtkBox {
    let b = GtkBox::new(Orientation::Vertical, 10);
    b.add_css_class("card");
    b.add_css_class("card-pad");
    b
}
