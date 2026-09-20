//! 中心「插件」页：已装插件列表、本地目录安装、启停与权限明示。
//!
//! 插件 = 独立进程（协议 v0：stdio 行式 JSON）。v0 权限为声明 + 明示
//!（不做运行时强制隔离；进程隔离 + bwrap 白名单兜底列入 M4）。

use adw::prelude::*;
use gtk::glib;
use gtk::{Align, Box as GtkBox, Button, Entry, Label, Orientation, Switch};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use yihu_core::plugins;

use crate::{page_shell, scroll_clamp};

struct Ui {
    busy: Cell<bool>,
    list_box: GtkBox,
    state_hint: Label,
}

/// 后台任务结果槽（阻塞调用不入 UI 主线程的既有惯例）
type Slot = Arc<Mutex<Option<String>>>;

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
        "v0 权限为声明 + 明示（安装时与权限列可见）；进程隔离与白名单",
        "沙箱兜底已内建，强制隔离列入 M4。",
    ] {
        let l = Label::new(Some(line));
        l.add_css_class("dim-label");
        l.add_css_class("caption-sm");
        l.set_halign(Align::Start);
        l.set_wrap(true);
        help_card.append(&l);
    }

    // —— 卡片：从目录安装 ——
    let install_card = card();
    let install_title = Label::new(Some("从目录安装"));
    install_title.add_css_class("sec-title");
    install_title.set_halign(Align::Start);
    install_card.append(&install_title);
    let row = GtkBox::new(Orientation::Horizontal, 8);
    let path_entry = Entry::new();
    path_entry.set_placeholder_text(Some("/路径/插件目录（内含 manifest.toml）"));
    path_entry.set_hexpand(true);
    let install_btn = Button::with_label("安装");
    row.append(&path_entry);
    row.append(&install_btn);
    install_card.append(&row);
    let install_hint = Label::new(None);
    install_hint.add_css_class("dim-label");
    install_hint.add_css_class("caption-sm");
    install_hint.set_halign(Align::Start);
    install_hint.set_wrap(true);
    install_card.append(&install_hint);

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
    for c in [&help_card, &install_card, &list_card] {
        c.set_hexpand(true);
        main_box.append(c);
    }

    let ui = Rc::new(Ui {
        busy: Cell::new(false),
        list_box,
        state_hint: install_hint,
    });

    // —— 信号：安装（后台线程 + 轮询回填）——
    {
        let ui = ui.clone();
        let path = path_entry.clone();
        let slot: Slot = Arc::new(Mutex::new(None));
        let slot2 = slot.clone();
        install_btn.connect_clicked(move |_| {
            let src = path.text().to_string();
            if src.trim().is_empty() || ui.busy.get() {
                return;
            }
            ui.busy.set(true);
            ui.state_hint.set_text("安装中…");
            let slot3 = slot2.clone();
            std::thread::spawn(move || {
                let res = plugins::install_from_dir(std::path::Path::new(src.trim()))
                    .map(|m| format!("已安装：{}（{}）", m.name, m.id))
                    .map_err(|e| e.to_string());
                *slot3.lock().unwrap() = Some(match res {
                    Ok(msg) => msg,
                    Err(e) => format!("失败：{e}"),
                });
            });
            let ui = ui.clone();
            let slot = slot2.clone();
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

    ui.refresh_list();
    page_shell("插件", &scroll_clamp(&main_box, 720))
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
                format!("权限：{}", inst.manifest.permissions.join("、"))
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
}

fn card() -> GtkBox {
    let b = GtkBox::new(Orientation::Vertical, 10);
    b.add_css_class("card");
    b.add_css_class("card-pad");
    b
}
