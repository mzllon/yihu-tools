//! 原生模板 UI 渲染器（M5 UI 插件层）：插件声明式描述界面，
//! 宿主渲染原生 GTK 窗口，用户交互事件回传插件。
//!
//! 协议（api:1 加法扩展）：
//!   插件→宿主 {"type":"ui.show","ui_id":N,"spec":{...}}
//!   宿主→插件 {"type":"ui.event","ui_id":N,"event":"submit","values":{...}}
//!            {"type":"ui.event","ui_id":N,"event":"cancel"}
//!            {"type":"ui.event","ui_id":N,"event":"action:<key>","values":{...}}
//!
//! spec（v2 控件集，全部纯声明）：
//!   {"title":"…",
//!    "fields":[
//!      {"key":"name","kind":"text","label":"名称","placeholder":"…","value":"预填"},
//!      {"key":"binding","kind":"hotkey","label":"按下快捷键","value":"<Super>e"},
//!      {"key":"app","kind":"app_select","label":"选择应用"},                ← 宿主应用列表
//!      {"key":"sql","kind":"textarea","label":"SQL","value":"…","lines":10}, ← v2 多行
//!      {"key":"op","kind":"select","label":"操作",                          ← v2 通用下拉
//!       "choices":[{"label":"Base64 编码","value":"b64enc"},…],"value":"b64enc"},
//!      {"key":"sym","kind":"switch","label":"符号","value":true},           ← v2 开关
//!      {"key":"len","kind":"number","label":"长度","min":6,"max":64,"value":16}, ← v2 数字
//!    ],
//!    "actions":[{"key":"run","label":"转换"}],      ← v2 动作按钮：不关窗，
//!                                                     点击回 action:<key> + 当前 values
//!    "output":{"rows":[                             ← v2 输出区：结果行 + 行内复制
//!      {"title":"MD5","subtitle":"32 位","copy":"<完整值>","preview":"<多行预览>"},
//!      {"title":"杭州交通91.8","subtitle":"正在播：…",          ← v2.1 行内动作按钮
//!       "actions":[{"key":"play:1133","label":"▶ 播放"}]}
//!    ]},
//!    "submit":"保存"}
//!
//! 复制语义：output 行内「复制」由宿主直写剪贴板（与列表激活复制 payload
//! 同一宿主语义），插件无需 clipboard.write 声明。
//!
//! 生命周期：ui.show 的会话在 UI 打开期间被 pinned（面板收起不杀，
//! 见 sessions::pinned），窗口关闭时回 cancel/submit 并解除 pin。
//! 同 ui_id 重复 show = 原地重建（生成→看结果→再生成，不闪窗不丢焦点）。

use gtk::prelude::*;
use gtk::glib;
use gtk::{
    Align, Box as GtkBox, Button, Entry, EventControllerKey, Label, Orientation, PolicyType,
    ScrolledWindow, SpinButton, Switch, TextView, Window, WrapMode,
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
    /// 多行文本（v2）
    TextArea(TextView),
    /// 通用下拉（v2）：候选项值列表，选中序号取值
    Select(gtk::DropDown, Rc<Vec<String>>),
    /// 开关（v2）
    Switch(Switch),
    /// 整数字段（v2）
    Number(SpinButton),
}

struct UiWindow {
    win: Window,
    /// 归属插件 id：去重 key（协议 ui_id 是各插件自计数，跨插件会撞号，
    /// 不能做全局 key——BUG-005 连带发现）
    plugin: String,
    /// 当前 ui_id（原地重建会更新；Cell 让既有按钮闭包免改）
    ui_id: std::cell::Cell<u64>,
    /// 字段控件（原地重建时整体替换；闭包持 Rc 读取最新值）
    fields: RefCell<Vec<(String, Field)>>,
}

/// build_content 的产物：fresh 创建与原地重建共用
struct Built {
    title: String,
    root: GtkBox,
    fields: Vec<(String, Field)>,
    /// (key, button)：点击不关窗，回 action:<key>
    actions: Vec<(String, Button)>,
    cancel: Option<Button>,
    submit: Option<Button>,
}

thread_local! {
    /// 打开中的 UI 窗口（ui_id → 窗口）
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

/// 按窗口身份移除出打开表（关闭语义由调用方决定；不发 cancel）
fn remove_open(w: &Rc<UiWindow>) {
    OPEN.with(|o| {
        o.borrow_mut().retain(|x| !Rc::ptr_eq(x, w));
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

/// 收集全部字段当前值（submit 与 action 共用）
fn collect_values(fields: &[(String, Field)]) -> serde_json::Map<String, serde_json::Value> {
    let mut values = serde_json::Map::new();
    for (key, field) in fields {
        match field {
            Field::Text(e) => {
                values.insert(key.clone(), serde_json::Value::String(e.text().to_string()));
            }
            Field::Hotkey(e) => {
                // SAFETY：键 "binding" 只在本文件 setup/取值中使用
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
            Field::TextArea(tv) => {
                let buf = tv.buffer();
                let (a, b) = buf.bounds();
                values.insert(
                    key.clone(),
                    serde_json::Value::String(buf.text(&a, &b, false).to_string()),
                );
            }
            Field::Select(dd, values_list) => {
                let idx = dd.selected() as usize;
                let v = values_list.get(idx).cloned().unwrap_or_default();
                values.insert(key.clone(), serde_json::Value::String(v));
            }
            Field::Switch(sw) => {
                values.insert(key.clone(), serde_json::Value::Bool(sw.is_active()));
            }
            Field::Number(sp) => {
                values.insert(
                    key.clone(),
                    serde_json::Value::Number(serde_json::Number::from(sp.value() as i64)),
                );
            }
        }
    }
    values
}

/// 构建一份 spec 的完整内容（不建窗口）。fresh 与原地重建共用。
fn build_content(spec: &serde_json::Value) -> Result<Built, String> {
    let title = spec
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("一呼插件")
        .to_string();
    let submit_label = spec
        .get("submit")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let fields_spec = spec
        .get("fields")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "ui.show spec 缺 fields".to_string())?;

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
            "textarea" => {
                let tv = TextView::new();
                tv.set_wrap_mode(WrapMode::WordChar);
                tv.buffer().set_text(prefill);
                let lines = f.get("lines").and_then(|v| v.as_u64()).unwrap_or(5).clamp(3, 14) as i32;
                let sw = ScrolledWindow::new();
                sw.set_child(Some(&tv));
                sw.set_height_request(lines * 24 + 8);
                sw.set_policy(PolicyType::Never, PolicyType::Automatic);
                row.append(&sw);
                fields.push((key, Field::TextArea(tv)));
            }
            "select" => {
                let ch = f
                    .get("choices")
                    .and_then(|v| v.as_array())
                    .ok_or_else(|| format!("select 字段 {key} 缺 choices"))?;
                let mut labels: Vec<String> = Vec::new();
                let mut values: Vec<String> = Vec::new();
                for c in ch {
                    match c {
                        serde_json::Value::Object(o) => {
                            let l = o.get("label").and_then(|v| v.as_str()).unwrap_or("");
                            let v = o.get("value").and_then(|v| v.as_str()).unwrap_or(l);
                            labels.push(l.to_string());
                            values.push(v.to_string());
                        }
                        serde_json::Value::String(s) => {
                            labels.push(s.clone());
                            values.push(s.clone());
                        }
                        _ => return Err(format!("select 字段 {key} 的 choices 项非法")),
                    }
                }
                if labels.is_empty() {
                    return Err(format!("select 字段 {key} 的 choices 为空"));
                }
                let strs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
                let dd = gtk::DropDown::from_strings(&strs);
                dd.set_width_request(260);
                if let Some(pos) = values.iter().position(|v| v == prefill) {
                    dd.set_selected(pos as u32);
                }
                row.append(&dd);
                fields.push((key, Field::Select(dd, Rc::new(values))));
            }
            "switch" => {
                let sw = Switch::new();
                sw.set_active(f.get("value").and_then(|v| v.as_bool()).unwrap_or(false));
                sw.set_halign(Align::Start);
                row.append(&sw);
                fields.push((key, Field::Switch(sw)));
            }
            "number" => {
                let min = f.get("min").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let max = f.get("max").and_then(|v| v.as_f64()).unwrap_or(65535.0);
                let step = f.get("step").and_then(|v| v.as_f64()).unwrap_or(1.0);
                let sp = SpinButton::with_range(min, max, step);
                sp.set_value(f.get("value").and_then(|v| v.as_f64()).unwrap_or(min));
                sp.set_width_request(120);
                sp.set_halign(Align::Start);
                row.append(&sp);
                fields.push((key, Field::Number(sp)));
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

    // —— v2 动作按钮行：点击不关窗，回 action:<key> + 当前 values ——
    let mut actions: Vec<(String, Button)> = Vec::new();
    if let Some(list) = spec.get("actions").and_then(|v| v.as_array()) {
        let btn_row = GtkBox::new(Orientation::Horizontal, 8);
        btn_row.set_halign(Align::End);
        for (i, a) in list.iter().enumerate() {
            let key = a
                .get("key")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if key.is_empty() {
                return Err("actions 项缺 key".into());
            }
            let label = a.get("label").and_then(|v| v.as_str()).unwrap_or(&key);
            let b = Button::with_label(label);
            if i == 0 {
                b.add_css_class("suggested-action");
            }
            btn_row.append(&b);
            actions.push((key, b));
        }
        root.append(&btn_row);
    }

    // —— v2 输出区：结果行 + 行内复制（宿主直写剪贴板，插件零声明）——
    let has_output = spec.get("output").is_some();
    if let Some(rows) = spec
        .get("output")
        .and_then(|o| o.get("rows"))
        .and_then(|v| v.as_array())
    {
        let list = GtkBox::new(Orientation::Vertical, 8);
        for r in rows.iter().take(64) {
            let rbox = GtkBox::new(Orientation::Vertical, 3);

            let head = GtkBox::new(Orientation::Horizontal, 8);
            let txt_col = GtkBox::new(Orientation::Vertical, 2);
            let t = r.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let tl = Label::new(Some(t));
            tl.add_css_class("body");
            tl.set_halign(Align::Start);
            tl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            tl.set_max_width_chars(48);
            tl.set_hexpand(true);
            txt_col.append(&tl);
            if let Some(sub) = r
                .get("subtitle")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                let sl = Label::new(Some(sub));
                sl.add_css_class("caption-sm");
                sl.set_halign(Align::Start);
                sl.set_ellipsize(gtk::pango::EllipsizeMode::End);
                txt_col.append(&sl);
            }
            head.append(&txt_col);
            if let Some(list) = r.get("actions").and_then(|v| v.as_array()) {
                // v2.1 行内动作按钮：点击回 action:<key>（与窗级 actions
                // 同一事件语义），供「列表里直接 ▶ 播放」类交互（电台等）
                for a in list.iter().take(3) {
                    let key = a
                        .get("key")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if key.is_empty() {
                        return Err("output 行 actions 项缺 key".into());
                    }
                    let label = a.get("label").and_then(|v| v.as_str()).unwrap_or(&key);
                    let b = Button::with_label(label);
                    b.set_valign(Align::Center);
                    head.append(&b);
                    actions.push((key, b));
                }
            } else if let Some(val) = r
                .get("copy")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                let val = val.to_string();
                let btn = Button::with_label("复制");
                btn.set_valign(Align::Center);
                {
                    let b_click = btn.clone();
                    let b_val = btn.clone();
                    b_click.connect_clicked(move |_| {
                        b_val.clipboard().set_text(&val);
                        b_val.set_label("已复制 ✓");
                    });
                }
                head.append(&btn);
            }
            rbox.append(&head);

            if let Some(prev) = r
                .get("preview")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                let tv = TextView::new();
                tv.set_wrap_mode(WrapMode::WordChar);
                tv.set_editable(false);
                tv.set_cursor_visible(false);
                tv.set_left_margin(8);
                tv.set_right_margin(8);
                tv.set_top_margin(6);
                tv.set_bottom_margin(6);
                tv.add_css_class("monospace");
                tv.buffer().set_text(prev);
                let nlines = prev.lines().count().max(1) as i32;
                let sw = ScrolledWindow::new();
                sw.set_child(Some(&tv));
                sw.set_height_request((nlines * 20 + 16).clamp(40, 240));
                sw.set_policy(PolicyType::Never, PolicyType::Automatic);
                rbox.append(&sw);
            }
            list.append(&rbox);
        }
        root.append(&list);
    }

    // 提交/取消行：spec 带 submit，或（无 actions 且无 output 的纯表单）时渲染；
    // 工具型窗口（只有 actions + output）不再出现多余的「保存」。
    let show_btns = !submit_label.is_empty() || (actions.is_empty() && !has_output);
    let mut cancel_btn = None;
    let mut submit_btn = None;
    if show_btns {
        let label = if submit_label.is_empty() { "保存" } else { &submit_label };
        let btn_row = GtkBox::new(Orientation::Horizontal, 8);
        btn_row.set_halign(Align::End);
        let cancel = Button::with_label("取消");
        let submit = Button::with_label(label);
        submit.add_css_class("suggested-action");
        btn_row.append(&cancel);
        btn_row.append(&submit);
        root.append(&btn_row);
        cancel_btn = Some(cancel);
        submit_btn = Some(submit);
    }

    Ok(Built {
        title,
        root,
        fields,
        actions,
        cancel: cancel_btn,
        submit: submit_btn,
    })
}

/// 渲染并打开一个声明式界面。`on_event` 在主线程回调：
/// (ui_id, "submit"|"cancel"|"action:<key>", values_json)。
/// 同 ui_id 已开时原地重建内容（不闪窗）。
pub fn show(
    ui_id: u64,
    plugin: &str,
    spec: &serde_json::Value,
    on_event: Rc<dyn Fn(u64, &str, String)>,
) -> Result<(), String> {
    let built = build_content(spec)?;

    // 每插件一窗：同插件重复 show = 原地重建（协议 ui_id 是插件自计数，
    // 每次弹窗都会递增，按它去重永远不命中——一查一窗的根源，BUG-005）
    let existing = OPEN.with(|o| {
        o.borrow()
            .iter()
            .find(|w| w.plugin == plugin)
            .map(Rc::clone)
    });
    if let Some(w) = existing {
        // 原地重建：标题/内容/字段整体替换，闭包持同一 Rc 读最新字段
        w.ui_id.set(ui_id);
        w.win.set_title(Some(&built.title));
        w.win.set_child(Some(&built.root));
        *w.fields.borrow_mut() = built.fields;
        wire_actions(&w, built.actions, &on_event);
        if let Some(cancel) = built.cancel {
            let w = w.clone();
            let on_event = on_event.clone();
            cancel.connect_clicked(move |_| {
                on_event(w.ui_id.get(), "cancel", String::new());
                remove_open(&w);
            });
        }
        if let Some(submit) = built.submit {
            let w = w.clone();
            let on_event = on_event.clone();
            submit.connect_clicked(move |_| {
                let values = collect_values(&w.fields.borrow());
                let json = serde_json::to_string(&serde_json::Value::Object(values))
                    .unwrap_or_else(|_| "{}".into());
                on_event(w.ui_id.get(), "submit", json);
                remove_open(&w);
            });
        }
        return Ok(());
    }

    let win = Window::new();
    win.set_title(Some(&built.title));
    win.set_resizable(false);
    win.set_modal(false);
    win.set_hide_on_close(false);
    win.set_child(Some(&built.root));

    let w = Rc::new(UiWindow {
        win,
        plugin: plugin.to_string(),
        ui_id: std::cell::Cell::new(ui_id),
        fields: RefCell::new(built.fields),
    });

    if let Some(cancel) = built.cancel {
        let w = w.clone();
        let on_event = on_event.clone();
        cancel.connect_clicked(move |_| {
            on_event(w.ui_id.get(), "cancel", String::new());
            remove_open(&w);
        });
    }
    if let Some(submit) = built.submit {
        let w = w.clone();
        let on_event = on_event.clone();
        submit.connect_clicked(move |_| {
            let values = collect_values(&w.fields.borrow());
            let json = serde_json::to_string(&serde_json::Value::Object(values))
                .unwrap_or_else(|_| "{}".into());
            on_event(w.ui_id.get(), "submit", json);
            remove_open(&w);
        });
    }
    wire_actions(&w, built.actions, &on_event);

    // 窗口关闭（X/收起兜底）= cancel
    {
        let wc = w.clone();
        let wc_win = wc.win.clone();
        let on_event = on_event.clone();
        wc_win.connect_close_request(move |_| {
            on_event(wc.ui_id.get(), "cancel", String::new());
            remove_open(&wc);
            glib::Propagation::Proceed
        });
    }
    w.win.set_plugin_id(plugin);
    OPEN.with(|o| o.borrow_mut().push(w.clone()));
    w.win.present();
    Ok(())
}

/// 接线动作按钮：点击收集当前字段值 → 回 action:<key>（不关窗）
fn wire_actions(
    w: &Rc<UiWindow>,
    actions: Vec<(String, Button)>,
    on_event: &Rc<dyn Fn(u64, &str, String)>,
) {
    for (key, btn) in actions {
        let w = w.clone();
        let on_event = on_event.clone();
        btn.connect_clicked(move |_| {
            let values = collect_values(&w.fields.borrow());
            let json = serde_json::to_string(&serde_json::Value::Object(values))
                .unwrap_or_else(|_| "{}".into());
            on_event(w.ui_id.get(), &format!("action:{key}"), json);
        });
    }
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
