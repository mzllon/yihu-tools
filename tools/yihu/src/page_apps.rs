//! 中心「应用跟随」页：让不支持自动深浅切换的应用跟随系统主题。
//!
//! 首批：VS Code 系编辑器（CodeBuddy / ZCode，settings.json 热重载）。
//! 不可程序化适配的应用（如飞书）以指引卡片说明应用内设置路径。

use adw::prelude::*;
use gtk::{Align, Box as GtkBox, Button, Label, Orientation};
use std::cell::Cell;
use std::rc::Rc;

use crate::adapters::{self, AppAdapter};
use crate::{page_shell, scroll_clamp};

struct AdapterUi {
    busy: Cell<bool>,
    status_label: Label,
    btn: Button,
}

struct Ui {
    adapters: Vec<(&'static AppAdapter, Rc<AdapterUi>)>,
}

pub fn build_page() -> gtk::Widget {
    let main_box = GtkBox::new(Orientation::Vertical, 14);
    main_box.set_margin_top(20);
    main_box.set_margin_bottom(28);
    main_box.set_margin_start(24);
    main_box.set_margin_end(24);
    main_box.set_valign(Align::Start);

    let intro = Label::new(Some(
        "让「内置深浅主题但不跟随系统」的应用自动切换。适配通过改写应用自身配置实现，\
         不修改应用本体；VS Code 系编辑器改写后立即生效，无需重启。",
    ));
    intro.add_css_class("dim-label");
    intro.add_css_class("caption-sm");
    intro.set_wrap(true);
    intro.set_halign(Align::Start);
    main_box.append(&intro);

    // —— VS Code 系适配卡片 ——
    let vs_card = card();
    let vs_title = Label::new(Some("VS Code 系编辑器"));
    vs_title.add_css_class("sec-title");
    vs_title.set_halign(Align::Start);
    vs_card.append(&vs_title);

    let mut adapters_list: Vec<(&'static AppAdapter, Rc<AdapterUi>)> = Vec::new();
    for adapter in adapters::VSCODE_FAMILY {
        let (row, status_label) = status_row(adapter.name);
        let btn = Button::with_label("一键适配");
        btn.add_css_class("suggested-action");
        row.append(&btn);

        let ui_a = Rc::new(AdapterUi { busy: Cell::new(false), status_label, btn });
        vs_card.append(&row);
        adapters_list.push((adapter, ui_a));
    }

    let vs_note = Label::new(Some(
        "原理：写入 window.autoDetectColorScheme 与浅/深主题偏好（已有配置完整保留），\
         编辑器即时跟随系统；之后你也可以在编辑器里换成喜欢的主题对。",
    ));
    vs_note.add_css_class("dim-label");
    vs_note.add_css_class("caption-sm");
    vs_note.set_wrap(true);
    vs_note.set_halign(Align::Start);
    vs_card.append(&vs_note);
    main_box.append(&vs_card);

    // —— 暂不可适配：飞书 ——
    let feishu_card = card();
    let fs_title = Label::new(Some("飞书"));
    fs_title.add_css_class("sec-title");
    fs_title.set_halign(Align::Start);
    feishu_card.append(&fs_title);
    let fs_state_row = GtkBox::new(Orientation::Horizontal, 8);
    let fs_state_name = Label::new(Some("自动跟随"));
    fs_state_name.add_css_class("dim-label");
    fs_state_name.set_hexpand(true);
    fs_state_name.set_halign(Align::Start);
    let fs_state = Label::new(Some("不可适配"));
    fs_state.add_css_class("state-warn");
    fs_state_row.append(&fs_state_name);
    fs_state_row.append(&fs_state);
    feishu_card.append(&fs_state_row);
    let fs_hint = Label::new(Some(
        "飞书的主题偏好存储在 Chromium LevelDB 数据库中，运行时改写有损坏风险，\
         故不做程序化适配。请到飞书「设置 → 通用 → 外观」查看是否有「跟随系统」\
         选项（应用自身提供的能力最可靠）。",
    ));
    fs_hint.add_css_class("dim-label");
    fs_hint.add_css_class("caption-sm");
    fs_hint.set_wrap(true);
    fs_hint.set_halign(Align::Start);
    feishu_card.append(&fs_hint);
    main_box.append(&feishu_card);

    // —— 布局 ——
    let ui = Rc::new(Ui { adapters: adapters_list });

    {
        let ui_c = ui.clone();
        for (ad, ui_a) in &ui.adapters {
            let ui_c = ui_c.clone();
            let ad: &'static AppAdapter = ad;
            let ui_a = ui_a.clone();
            let btn = ui_a.btn.clone();
            btn.connect_clicked(move |_| {
                if ui_a.busy.get() {
                    return;
                }
                ui_a.busy.set(true);
                ui_a.btn.set_sensitive(false);
                let result = adapters::apply(ad);
                ui_a.busy.set(false);
                if let Err(e) = result {
                    ui_a.status_label.set_text(&format!("失败：{e}"));
                }
                refresh_all(&ui_c);
            });
        }
    }

    refresh_all(&ui);
    page_shell("应用跟随", &scroll_clamp(&main_box, 720))
}

fn refresh_all(ui: &Ui) {
    for (ad, ui_a) in &ui.adapters {
        refresh_adapter(ui_a, adapters::status(ad));
    }
}

/// 按钮的可用性与文案唯一由本函数决定。
fn refresh_adapter(ui_a: &AdapterUi, st: adapters::Status) {
    if ui_a.busy.get() {
        return;
    }
    if !st.app_present {
        ui_a.status_label.set_text("未检测到应用");
        ui_a.btn.set_visible(false);
        return;
    }
    ui_a.btn.set_visible(true);
    ui_a.btn.set_sensitive(!st.adapted);
    if st.adapted {
        ui_a.btn.set_label("已适配 ✓");
        ui_a.status_label.set_text("跟随系统中");
        ui_a.status_label.remove_css_class("state-off");
        ui_a.status_label.add_css_class("state-ok");
    } else {
        ui_a.btn.set_label("一键适配");
        ui_a
            .status_label
            .set_text("未适配（手动设置主题，不随系统切换）");
        ui_a.status_label.remove_css_class("state-ok");
        ui_a.status_label.add_css_class("state-off");
    }
}

// ---- UI 小工具 ----

fn card() -> GtkBox {
    let b = GtkBox::new(Orientation::Vertical, 10);
    b.add_css_class("card");
    b.add_css_class("card-pad");
    b
}

fn status_row(title: &str) -> (GtkBox, Label) {
    let row = GtkBox::new(Orientation::Horizontal, 8);
    let t = Label::new(Some(title));
    t.add_css_class("dim-label");
    t.set_hexpand(true);
    t.set_halign(Align::Start);
    let v = Label::new(Some("–"));
    v.add_css_class("row-title");
    row.append(&t);
    row.append(&v);
    (row, v)
}
