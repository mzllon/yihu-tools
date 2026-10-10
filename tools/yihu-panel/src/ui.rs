//! 原生模板 UI 渲染器（M5 UI 插件层 v1）：插件声明式描述表单，
//! 宿主渲染原生 GTK 窗口，用户交互事件回传插件。
//!
//! 协议（api:1 加法扩展）：
//!   插件→宿主 {"type":"ui.show","ui_id":N,"spec":{...}}
//!   宿主→插件 {"type":"ui.event","ui_id":N,"event":"submit","values":{...}}
//!            {"type":"ui.event","ui_id":N,"event":"cancel"}
//!
//! spec（v1 控件集，全部纯声明）：
//!   {"title":"…","fields":[
//!      {"key":"name","kind":"text","label":"名称","placeholder":"…","value":"预填"},
//!      {"key":"binding","kind":"hotkey","label":"按下快捷键","value":"<Super>e"},
//!      {"key":"app","kind":"app_select","label":"选择应用"},          ← 宿主应用列表
//!      {"key":"command","kind":"text","label":"或输入命令"}
//!    ],"submit":"保存"}
//!
//! 「绑定什么动作我不知道」的用户问题由 app_select 解决：从已安装应用
//! 里点选（值 = desktop id，宿主映射为 gtk-launch 命令），不必知道
//! nautilus 是什么。hotkey 控件 = EventControllerKey 按键捕获：按下
//! 组合即录入（<Super>e），不需要学语法。
//!
//! 生命周期：ui.show 的会话在 UI 打开期间被 pinned（面板收起不杀，
//! 见 sessions::pinned），窗口关闭时回 cancel/submit 并解除 pin。

use gtk::prelude::*;
use gtk::glib;
use gtk::{
    Align, Box as GtkBox, Button, Entry, EventControllerKey, Label, Orientation, Window,
};
use std::cell::RefCell;
use std::rc::Rc;

/// 一条声明字段解析后的控件句柄（取值用）
enum Field {
    Text(Entry),
    /// 快捷键捕获：Entry 只读展示 + 按键录入
    Hotkey(Entry),
    /// 应用选择：DropDown（desktop id 列表），值转命令
    AppSelect(gtk::DropDown, Rc<Vec<(String, String)>>), // (desktop_id, command)
}

struct UiWindow {
    win: Window,
    ui_id: u64,
    fields: Vec<(String, Field)>,
}

thread_local! {
    /// 打开中的 UI 窗口（ui_id → 窗口）。同 ui_id 重复 show = 替换。
    static OPEN: RefCell<Vec<Rc<UiWindow>>> = const { RefCell::new(Vec::new()) };
}

/// window.plugin_id 附件（同 app.rs Row 的 data 惯例）
trait PluginId {
    fn set_plugin_id(&self, id: &str);
    fn plugin_id(&self) -> Option<String>;
}
impl PluginId for Window {
    fn set_plugin_id(&self, id: &str) {
        unsafe { self.set_data("yihu-ui-plugin", id.to_string()) };
    }
    fn plugin_id(&self) -> Option<String> {
        unsafe { self.data::<String>("yihu-ui-plugin").map(|p| p.as_ref().clone()) }
    }
}

/// 关闭指定 ui_id 的窗口（会话死亡时兜底；不发 cancel——插件已死）
pub fn close(ui_id: u64) {
    OPEN.with(|o| {
        o.borrow_mut().retain(|w| {
            if w.ui_id == ui_id {
                w.win.close();
                false
            } else {
                true
            }
        })
    });
}

/// app_select 的候选项：宿主应用列表（desktop id → 启动命令）。
/// gtk-launch <id> 是 GNOME 官方的按 desktop id 启动方式。
fn app_choices() -> Rc<Vec<(String, String)>> {
    let mut v: Vec<(String, String)> = gtk::gio::AppInfo::all()
        .into_iter()
        .filter(|a| a.should_show())
        .filter_map(|a| {
            let id = a.id()?.to_string();
            let name = a.display_name().to_string();
            if id.ends_with(".desktop") && !name.is_empty() {
                let cmd = format!("gtk-launch {}", id.trim_end_matches(".desktop"));
                Some((id, cmd))
            } else {
                None
            }
        })
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    Rc::new(v)
}

/// 渲染并打开一个声明式表单。`on_event` 在主线程回调：
/// (ui_id, "submit", values_json) 或 (ui_id, "cancel", "")。
pub fn show(
    ui_id: u64,
    plugin: &str,
    spec: &serde_json::Value,
    on_event: Rc<dyn Fn(u64, &str, String)>,
) -> Result<(), String> {
    let title = spec
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("一呼插件")
        .to_string();
    let submit_label = spec
        .get("submit")
        .and_then(|v| v.as_str())
        .unwrap_or("保存")
        .to_string();
    let fields_spec = spec
        .get("fields")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "ui.show spec 缺 fields".to_string())?;

    // 同 id 已开则替换
    close(ui_id);

    let win = Window::new();
    win.set_title(Some(&title));
    win.set_resizable(false);
    win.set_modal(false);
    win.set_hide_on_close(false);

    let root = GtkBox::new(Orientation::Vertical, 12);
    root.set_margin_top(18);
    root.set_margin_bottom(16);
    root.set_margin_start(20);
    root.set_margin_end(20);

    let title_lbl = Label::new(Some(&title));
    title_lbl.add_css_class("title-3");
    title_lbl.set_halign(Align::Start);
    root.append(&title_lbl);

    let choices = app_choices();
    let mut fields: Vec<(String, Field)> = Vec::new();

    for f in fields_spec {
        let key = f.get("key").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if key.is_empty() {
            return Err("ui.show 字段缺 key".into());
        }
        let kind = f.get("kind").and_then(|v| v.as_str()).unwrap_or("text");
        let label_text = f.get("label").and_then(|v| v.as_str()).unwrap_or(&key);
        let prefill = f.get("value").and_then(|v| v.as_str()).unwrap_or("");

        let row = GtkBox::new(Orientation::Vertical, 4);
        let lbl = Label::new(Some(label_text));
        lbl.add_css_class("caption-sm");
        lbl.set_halign(Align::Start);
        row.append(&lbl);

        match kind {
            "hotkey" => {
                let entry = Entry::new();
                entry.set_width_chars(24);
                entry.set_placeholder_text(Some("点击此处后按下组合键"));
                entry.set_text(prefill);
                entry.set_editable(false);
                entry.add_css_class("hotkey-entry");
                let controller = EventControllerKey::new();
                {
                    let entry = entry.clone();
                    controller.connect_key_pressed(
                        move |_c, key, _code, state| {
                            let (binding, display) = binding_of(key, state);
                            if !binding.is_empty() {
                                entry.set_text(&display);
                                // SAFETY：键 "binding" 只在本文件 setup/取值中使用
                                unsafe { entry.set_data::<String>("binding", binding) };
                            }
                            glib::Propagation::Proceed
                        },
                    );
                }
                entry.add_controller(controller);
                row.append(&entry);
                fields.push((key, Field::Hotkey(entry)));
            }
            "app_select" => {
                let names: Vec<&str> = choices
                    .iter()
                    .map(|(id, _)| id.trim_end_matches(".desktop"))
                    .collect();
                let dd = gtk::DropDown::from_strings(&names);
                dd.set_enable_search(true);
                dd.set_width_request(260);
                // 预填按 desktop id 定位
                if let Some(pos) = choices
                    .iter()
                    .position(|(id, _)| id == prefill.trim_end_matches(".desktop") || prefill == *id)
                {
                    dd.set_selected(pos as u32);
                }
                row.append(&dd);
                fields.push((key, Field::AppSelect(dd, choices.clone())));
            }
            _ => {
                // text
                let entry = Entry::new();
                entry.set_width_chars(28);
                entry.set_placeholder_text(
                    Some(f.get("placeholder").and_then(|v| v.as_str()).unwrap_or("")),
                );
                entry.set_text(prefill);
                row.append(&entry);
                fields.push((key, Field::Text(entry)));
            }
        }
        root.append(&row);
    }

    // 按钮行
    let btn_row = GtkBox::new(Orientation::Horizontal, 8);
    btn_row.set_halign(Align::End);
    let cancel = Button::with_label("取消");
    let submit = Button::with_label(&submit_label);
    submit.add_css_class("suggested-action");
    btn_row.append(&cancel);
    btn_row.append(&submit);
    root.append(&btn_row);

    win.set_child(Some(&root));

    let w = Rc::new(UiWindow { win, ui_id, fields });

    // 取消：回 cancel 并关窗
    {
        let w = w.clone();
        let on_event = on_event.clone();
        cancel.connect_clicked(move |_| {
            on_event(w.ui_id, "cancel", String::new());
            close(w.ui_id);
        });
    }
    // 提交：收集字段值 → values JSON → 回 submit
    {
        let w = w.clone();
        let on_event = on_event.clone();
        submit.connect_clicked(move |_| {
            let mut values = serde_json::Map::new();
            for (key, field) in &w.fields {
                match field {
                    Field::Text(e) => {
                        values.insert(key.clone(), serde_json::Value::String(e.text().to_string()));
                    }
                    Field::Hotkey(e) => {
                        // SAFETY：键 "binding" 只在本文件写入/读取
                        let binding = unsafe {
                            e.data::<String>("binding")
                                .map(|s| s.as_ref().clone())
                                .unwrap_or_default()
                        };
                        values.insert(key.clone(), serde_json::Value::String(binding));
                    }
                    Field::AppSelect(dd, choices) => {
                        let idx = dd.selected() as usize;
                        let cmd = choices
                            .get(idx)
                            .map(|(_, c)| c.clone())
                            .unwrap_or_default();
                        values.insert(key.clone(), serde_json::Value::String(cmd));
                    }
                }
            }
            let json = serde_json::to_string(&serde_json::Value::Object(values))
                .unwrap_or_else(|_| "{}".into());
            on_event(w.ui_id, "submit", json);
            close(w.ui_id);
        });
    }
    // 窗口关闭（X/收起兜底）= cancel
    {
        let wc = w.clone();
        let wc_win = wc.win.clone();
        let on_event = on_event.clone();
        wc_win.connect_close_request(move |_| {
            on_event(wc.ui_id, "cancel", String::new());
            OPEN.with(|o| o.borrow_mut().retain(|x| x.ui_id != wc.ui_id));
            glib::Propagation::Proceed
        });
    }
    w.win.set_plugin_id(plugin);
    OPEN.with(|o| o.borrow_mut().push(w.clone()));
    w.win.present();
    Ok(())
}

/// 会话 pin：UI 打开期间该插件会话不随面板收起被杀（BUG-001 同源的
/// 跨生命周期约定：豁免按插件 id，UI 全关后自动解除）。
pub fn open_plugin_ids() -> Vec<String> {
    OPEN.with(|o| {
        o.borrow()
            .iter()
            .filter_map(|w| w.win.plugin_id())
            .collect()
    })
}

/// GDK Key + 修饰键 → (gsettings binding, 展示文本)
fn binding_of(key: gtk::gdk::Key, state: gtk::gdk::ModifierType) -> (String, String) {
    use gtk::gdk::ModifierType as M;
    let mut s = String::new();
    let mut display = String::new();
    fn push(s: &mut String, d: &mut String, tag: &str, name: &str) {
        s.push_str(tag);
        d.push_str(name);
    }
    if state.contains(M::SUPER_MASK) {
        push(&mut s, &mut display, "<Super>", "Win+");
    }
    if state.contains(M::CONTROL_MASK) {
        push(&mut s, &mut display, "<Control>", "Ctrl+");
    }
    if state.contains(M::ALT_MASK) {
        push(&mut s, &mut display, "<Alt>", "Alt+");
    }
    if state.contains(M::SHIFT_MASK) {
        push(&mut s, &mut display, "<Shift>", "Shift+");
    }
    let name = key.name().unwrap_or_default();
    if name.is_empty() {
        return (String::new(), String::new());
    }
    // 单字母/数字转小写；F 键与特殊键原样（F1、Return→Enter…）
    let (part, disp) = match name.as_str() {
        "space" => ("space".into(), "Space".into()),
        "Return" | "KP_Enter" => ("Return".into(), "Enter".into()),
        "Escape" => ("Escape".into(), "Esc".into()),
        other => {
            let lower = other.to_ascii_lowercase();
            if other.len() == 1 {
                (lower, other.to_ascii_uppercase())
            } else {
                (other.to_string(), other.to_string())
            }
        }
    };
    s.push_str(&part);
    display.push_str(&disp);
    (s, display)
}
