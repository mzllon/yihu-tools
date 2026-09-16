//! 中心「右键菜单」页：「复制绝对路径」的启用、依赖安装与状态管理。
//!
//! 依赖 python3-nautilus 时通过 pkexec 弹出系统授权对话框完成安装，
//! 用户无需打开终端。安装为阻塞调用，放后台线程执行，主循环轮询；
//! 取消授权 / 安装失败 / 超时都会恢复到可重试状态。

use adw::prelude::*;
use gtk::glib;
use gtk::{Align, Box as GtkBox, Button, Label, Orientation};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::{nautilus, page_shell};

struct Ui {
    installing: Cell<bool>,
    action_btn: Button,
    install_btn: Button,
    state_caption: Label,
    legacy_note: Label,
    dep_state: Label,
}

pub fn build_page() -> gtk::Widget {
    // —— 卡片：功能 ——
    let switch_card = card();
    let row = GtkBox::new(Orientation::Horizontal, 12);
    let title_col = GtkBox::new(Orientation::Vertical, 2);
    let t = Label::new(Some("复制绝对路径"));
    t.add_css_class("row-title");
    let state_caption = Label::new(Some("未启用"));
    state_caption.add_css_class("dim-label");
    state_caption.add_css_class("caption-sm");
    title_col.append(&t);
    title_col.append(&state_caption);
    title_col.set_hexpand(true);
    title_col.set_valign(Align::Center);
    let action_btn = Button::new();
    action_btn.set_valign(Align::Center);
    row.append(&title_col);
    row.append(&action_btn);
    switch_card.append(&row);
    let loc_hint = Label::new(Some(
        "启用后在文件管理器中右键任意文件即可看到「复制绝对路径」（顶级菜单项，支持多选）",
    ));
    loc_hint.add_css_class("dim-label");
    loc_hint.add_css_class("caption-sm");
    loc_hint.set_wrap(true);
    switch_card.append(&loc_hint);
    let legacy_note = Label::new(Some(""));
    legacy_note.add_css_class("caption-sm");
    legacy_note.set_wrap(true);
    legacy_note.set_halign(Align::Start);
    switch_card.append(&legacy_note);

    // —— 卡片：依赖 ——
    let dep_card = card();
    let dep_title = Label::new(Some("依赖"));
    dep_title.add_css_class("sec-title");
    dep_title.set_halign(Align::Start);
    dep_card.append(&dep_title);
    let (dep_row, dep_state) = status_row("python3-nautilus");
    dep_card.append(&dep_row);
    let install_btn = Button::with_label("安装");
    install_btn.add_css_class("suggested-action");
    dep_row.append(&install_btn);
    let dep_hint = Label::new(Some(
        "点击「安装」会弹出系统授权对话框，输入一次密码即可自动完成；\
         安装完成后扩展自动启用。取消授权可随时重试。",
    ));
    dep_hint.add_css_class("dim-label");
    dep_hint.add_css_class("caption-sm");
    dep_hint.set_wrap(true);
    dep_hint.set_halign(Align::Start);
    dep_card.append(&dep_hint);

    let btn_row = GtkBox::new(Orientation::Horizontal, 8);
    let hint = Label::new(Some("启用/移除会自动重载 Nautilus（nautilus -q）。"));
    hint.add_css_class("dim-label");
    hint.add_css_class("caption-sm");
    hint.set_hexpand(true);
    hint.set_halign(Align::Start);
    hint.set_valign(Align::Center);
    let refresh_btn = Button::with_label("重新检测");
    btn_row.append(&hint);
    btn_row.append(&refresh_btn);
    dep_card.append(&btn_row);

    // —— 布局 ——
    let main_box = GtkBox::new(Orientation::Vertical, 14);
    main_box.set_margin_top(20);
    main_box.set_margin_bottom(28);
    main_box.set_margin_start(24);
    main_box.set_margin_end(24);
    main_box.set_valign(Align::Start);
    for c in [&switch_card, &dep_card] {
        c.set_hexpand(true);
        main_box.append(c);
    }

    let ui = Rc::new(Ui {
        installing: Cell::new(false),
        action_btn: action_btn.clone(),
        install_btn: install_btn.clone(),
        state_caption,
        legacy_note,
        dep_state,
    });

    // —— 信号 ——
    {
        let ui_c = ui.clone();
        action_btn.connect_clicked(move |_| {
            if ui_c.installing.get() {
                return;
            }
            let st = nautilus::status();
            let result = if st.ext_installed {
                nautilus::disable()
            } else {
                nautilus::enable()
            };
            if let Err(e) = result {
                ui_c.state_caption.set_text(&format!("操作失败：{e}"));
            }
            ui_c.refresh();
        });
    }
    {
        let ui_c = ui.clone();
        install_btn.connect_clicked(move |_| {
            if ui_c.installing.get() {
                return;
            }
            ui_c.installing.set(true);
            ui_c.refresh();
            ui_c
                .dep_state
                .set_text("正在安装…（请在弹出的授权对话框中输入密码）");

            // 后台线程执行阻塞式安装；主循环每 150ms 轮询结果，
            // 5 分钟无结果按超时处理，状态始终可恢复。
            let slot = Arc::new(Mutex::new(None));
            {
                let slot = slot.clone();
                std::thread::spawn(move || {
                    let r = nautilus::pkexec_install("python3-nautilus");
                    *slot.lock().unwrap() = Some(r);
                });
            }
            let ui_p = ui_c.clone();
            let slot = slot.clone();
            let mut started: Option<Instant> = None;
            glib::timeout_add_local(Duration::from_millis(150), move || {
                if started.is_none() {
                    started = Some(Instant::now());
                }
                if started.is_some_and(|t| t.elapsed() > Duration::from_secs(300)) {
                    ui_p.installing.set(false);
                    ui_p.refresh();
                    ui_p.dep_state.set_text("安装超时，请重试");
                    return glib::ControlFlow::Break;
                }
                let done = slot.lock().unwrap().take();
                match done {
                    None => glib::ControlFlow::Continue,
                    Some(result) => {
                        ui_p.installing.set(false);
                        match result {
                            Ok(true) => {
                                if let Err(e) = nautilus::enable() {
                                    ui_p.refresh();
                                    ui_p.state_caption
                                        .set_text(&format!("依赖已装好，但启用失败：{e}"));
                                    return glib::ControlFlow::Break;
                                }
                            }
                            Ok(false) => {
                                // 授权被取消或安装未成功
                            }
                            Err(e) => {
                                ui_p.refresh();
                                ui_p.dep_state.set_text(&format!("安装失败：{e}"));
                                return glib::ControlFlow::Break;
                            }
                        }
                        ui_p.refresh();
                        if let Ok(false) = result {
                            ui_p.dep_state.set_text("未完成安装（授权已取消），可重试");
                        }
                        glib::ControlFlow::Break
                    }
                }
            });
        });
    }
    {
        let ui_c = ui.clone();
        refresh_btn.connect_clicked(move |_| ui_c.refresh());
    }

    ui.refresh();
    page_shell("右键菜单", &crate::scroll_clamp(&main_box, 720))
}

impl Ui {
    /// 所有按钮的可用性与文案均以此为准（状态单一来源）。
    fn refresh(&self) {
        let st = nautilus::status();
        let installing = self.installing.get();

        // 依赖安装按钮：仅「未安装且不在安装中」可点
        self.install_btn
            .set_sensitive(!st.python_nautilus_available && !installing);
        self.install_btn.set_visible(!st.python_nautilus_available);
        if !installing {
            self.dep_state.set_text(if st.python_nautilus_available {
                "已安装"
            } else {
                "未安装"
            });
            self.dep_state.remove_css_class("state-ok");
            self.dep_state.remove_css_class("state-warn");
            self.dep_state.add_css_class(if st.python_nautilus_available {
                "state-ok"
            } else {
                "state-warn"
            });
        }

        // 主操作按钮
        if installing {
            self.action_btn.set_label("安装中…");
            self.action_btn.set_sensitive(false);
        } else {
            match (st.python_nautilus_available, st.ext_installed) {
                (true, true) => {
                    self.action_btn.set_label("移除");
                    self.action_btn.set_sensitive(true);
                }
                (true, false) => {
                    self.action_btn.set_label("一键启用");
                    self.action_btn.set_sensitive(true);
                }
                (false, _) => {
                    self.action_btn.set_label("需先安装依赖");
                    self.action_btn.set_sensitive(false);
                }
            }
        }

        self.state_caption.set_text(if st.ext_installed {
            "已启用：在文件管理器中右键任意文件 → 复制绝对路径"
        } else {
            "未启用"
        });
        self.state_caption.remove_css_class("state-ok");
        self.state_caption.remove_css_class("state-off");
        self.state_caption
            .add_css_class(if st.ext_installed { "state-ok" } else { "state-off" });

        self.legacy_note.set_text(if st.legacy_script && !st.ext_installed {
            "检测到旧版「脚本」方案残留；启用扩展后会自动清理。"
        } else {
            ""
        });
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
