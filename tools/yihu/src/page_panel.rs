//! 中心「呼出面板」页：进程状态与资源、全局热键注册、开机自启、Wayland 说明。
//!
//! 中心只负责配置与状态：热键注册写 gsettings（合并、不覆盖用户已有键）、
//! 启停与自启针对独立执行端 `yihu-panel`；中心关闭后面板照常工作。

use adw::prelude::*;
use gtk::glib;
use gtk::{Align, Box as GtkBox, Button, DropDown, Label, Orientation, Switch};
use std::cell::Cell;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use yihu_core::panel;

use crate::shell_ext;
use crate::{page_shell, scroll_clamp};

const AUTOSTART_NAME: &str = "yihu-panel.desktop";

/// 预设快捷键（显示名, gsettings binding 值）
const PRESETS: &[(&str, &str)] = &[
    ("Alt+Space", "<Alt>space"),
    ("Alt+Z", "<Alt>Z"),
    ("Alt+R", "<Alt>R"),
    ("Ctrl+Space（输入法冲突风险）", "<Control>space"),
];

struct Ui {
    autostart_sync: Cell<bool>,
    hotkey_sync: Cell<bool>,
    busy: Cell<bool>,
    autostart_switch: Switch,
    autostart_state: Label,
    panel_res: Label,
    hotkey_state: Label,
    hotkey_result: Label,
    preset: DropDown,
    pos_state: Label,
}

/// 后台任务结果槽（阻塞调用不入 UI 主线程的既有惯例）
type Slot = Arc<Mutex<Option<String>>>;

pub fn build_page() -> gtk::Widget {
    // —— 卡片：面板进程 ——
    let proc_card = card();
    let proc_title = Label::new(Some("面板进程"));
    proc_title.add_css_class("sec-title");
    proc_title.set_halign(Align::Start);
    proc_card.append(&proc_title);
    let (p_row, panel_res) = status_row("yihu-panel（隐藏待命）");
    proc_card.append(&p_row);
    let btn_row = GtkBox::new(Orientation::Horizontal, 8);
    let start_btn = Button::with_label("启动面板");
    let quit_btn = Button::with_label("退出面板");
    btn_row.append(&start_btn);
    btn_row.append(&quit_btn);
    btn_row.set_halign(Align::Start);
    proc_card.append(&btn_row);

    // —— 卡片：全局快捷键 ——
    let hk_card = card();
    let hk_title = Label::new(Some("全局快捷键"));
    hk_title.add_css_class("sec-title");
    hk_title.set_halign(Align::Start);
    hk_card.append(&hk_title);
    let preset_row = GtkBox::new(Orientation::Horizontal, 8);
    let preset_label = Label::new(Some("快捷键"));
    preset_label.set_halign(Align::Start);
    preset_label.set_hexpand(true);
    let names: Vec<&str> = PRESETS.iter().map(|(n, _)| *n).collect();
    let preset = DropDown::from_strings(&names);
    preset.set_valign(Align::Center);
    preset_row.append(&preset_label);
    preset_row.append(&preset);
    hk_card.append(&preset_row);
    let (cur_row, hotkey_state) = status_row("当前注册");
    hk_card.append(&cur_row);
    let hk_btn_row = GtkBox::new(Orientation::Horizontal, 8);
    let reg_btn = Button::with_label("注册到系统");
    let unreg_btn = Button::with_label("从系统移除");
    hk_btn_row.append(&reg_btn);
    hk_btn_row.append(&unreg_btn);
    hk_btn_row.set_halign(Align::Start);
    hk_card.append(&hk_btn_row);
    let hotkey_result = Label::new(None);
    hotkey_result.add_css_class("dim-label");
    hotkey_result.add_css_class("caption-sm");
    hotkey_result.set_wrap(true);
    hotkey_result.set_halign(Align::Start);
    hk_card.append(&hotkey_result);
    let hk_hint = Label::new(Some(
        "注册会写入 GNOME「设置 → 键盘 → 自定义快捷键」：追加本面板一条记录，\
         不改动其他自定义快捷键；Wayland 下由系统层触发 yihu-panel toggle。\
         若快捷键与 GNOME 内置键冲突（如默认的 Alt+Space「窗口菜单」），\
         注册时会自动解除内置占用。",
    ));
    hk_hint.add_css_class("dim-label");
    hk_hint.add_css_class("caption-sm");
    hk_hint.set_wrap(true);
    hk_hint.set_halign(Align::Start);
    hk_card.append(&hk_hint);

    // —— 卡片：开机自启 ——
    let as_card = card();
    let as_row = GtkBox::new(Orientation::Horizontal, 12);
    let as_col = GtkBox::new(Orientation::Vertical, 2);
    let as_t = Label::new(Some("开机自启"));
    as_t.add_css_class("row-title");
    let autostart_state = Label::new(Some("未启用"));
    autostart_state.add_css_class("dim-label");
    autostart_state.add_css_class("caption-sm");
    as_col.append(&as_t);
    as_col.append(&autostart_state);
    as_col.set_hexpand(true);
    as_col.set_valign(Align::Center);
    let autostart_switch = Switch::new();
    autostart_switch.set_valign(Align::Center);
    as_row.append(&as_col);
    as_row.append(&autostart_switch);
    as_card.append(&as_row);
    let as_hint = Label::new(Some(
        "面板随登录自启并隐藏待命（实测约 25 MB 物理占用）；中心关闭后仍可呼出。",
    ));
    as_hint.add_css_class("dim-label");
    as_hint.add_css_class("caption-sm");
    as_hint.set_wrap(true);
    as_hint.set_halign(Align::Start);
    as_card.append(&as_hint);

    // —— 卡片：屏幕位置（Shell 扩展）——
    let pos_card = card();
    let pos_title = Label::new(Some("屏幕位置"));
    pos_title.add_css_class("sec-title");
    pos_title.set_halign(Align::Start);
    pos_card.append(&pos_title);
    let (pl_row, pos_state) = status_row("面板定位扩展");
    pos_card.append(&pl_row);
    let pos_btn_row = GtkBox::new(Orientation::Horizontal, 8);
    let pos_install_btn = Button::with_label("安装扩展");
    let pos_remove_btn = Button::with_label("移除扩展");
    pos_btn_row.append(&pos_install_btn);
    pos_btn_row.append(&pos_remove_btn);
    pos_btn_row.set_halign(Align::Start);
    pos_card.append(&pos_btn_row);
    let pos_hint = Label::new(Some(
        "Wayland 下应用无法决定自己的位置：默认由系统摆放（位置不固定）。\n\
         安装定位扩展后（GNOME Shell 扩展，约 2KB，仅本用户），面板每次呼出\n\
         自动摆到「水平居中、垂直上 1/4 处」并置顶。新装扩展需注销重新登录一次生效。",
    ));
    pos_hint.add_css_class("dim-label");
    pos_hint.add_css_class("caption-sm");
    pos_hint.set_wrap(true);
    pos_hint.set_halign(Align::Start);
    pos_card.append(&pos_hint);

    // —— 卡片：说明 ——
    let note_card = card();
    let note_title = Label::new(Some("形态说明（M1 骨架）"));
    note_title.add_css_class("sec-title");
    note_title.set_halign(Align::Start);
    note_card.append(&note_title);
    for line in [
        "面板出现在屏幕中央：Wayland 不允许应用自选窗口位置；",
        "呼出时面板默认在顶层（Wayland 无客户端置顶接口，故不做置顶）；",
        "当前列表为性能验证用的演示数据，应用与能力搜索在 M2 接入。",
    ] {
        let l = Label::new(Some(line));
        l.add_css_class("dim-label");
        l.add_css_class("caption-sm");
        l.set_halign(Align::Start);
        l.set_wrap(true);
        note_card.append(&l);
    }

    // —— 布局 ——
    let main_box = GtkBox::new(Orientation::Vertical, 14);
    main_box.set_margin_top(20);
    main_box.set_margin_bottom(28);
    main_box.set_margin_start(24);
    main_box.set_margin_end(24);
    main_box.set_valign(Align::Start);
    for c in [&proc_card, &hk_card, &pos_card, &as_card, &note_card] {
        c.set_hexpand(true);
        main_box.append(c);
    }

    let ui = Rc::new(Ui {
        autostart_sync: Cell::new(false),
        hotkey_sync: Cell::new(false),
        busy: Cell::new(false),
        autostart_switch,
        autostart_state,
        panel_res,
        hotkey_state,
        hotkey_result,
        preset,
        pos_state,
    });

    // 后台任务结果槽：注册 / 移除 / 退出 / 定位扩展各一个
    let reg_slot: Slot = Arc::new(Mutex::new(None));
    let unreg_slot: Slot = Arc::new(Mutex::new(None));
    let quit_slot: Slot = Arc::new(Mutex::new(None));
    let pos_slot: Slot = Arc::new(Mutex::new(None));

    // —— 信号：安装 / 移除定位扩展 ——
    {
        let ui = ui.clone();
        let slot = pos_slot.clone();
        pos_install_btn.connect_clicked(move |_| {
            run_bg(&ui, &slot, || {
                shell_ext::install()
                    .map(|_| "扩展已安装".to_string())
                    .map_err(|e| e.to_string())
            });
        });
    }
    {
        let ui = ui.clone();
        let slot = pos_slot.clone();
        pos_remove_btn.connect_clicked(move |_| {
            run_bg(&ui, &slot, || {
                shell_ext::remove()
                    .map(|_| "扩展已移除（重新登录后完全卸载）".to_string())
                    .map_err(|e| e.to_string())
            });
        });
    }

    // —— 信号：启动 / 退出面板 ——
    {
        let ui = ui.clone();
        start_btn.connect_clicked(move |_| {
            if ui.busy.get() {
                return;
            }
            match panel_path() {
                Ok(p) => {
                    // fire-and-forget：spawn 即返回，不阻塞 UI
                    if let Err(e) = Command::new(&p).stdin(fs_null()).spawn() {
                        ui.hotkey_result.set_text(&format!("启动失败：{e}"));
                    }
                }
                Err(e) => ui.hotkey_result.set_text(&format!("启动失败：{e}")),
            }
            ui.refresh();
        });
    }
    {
        let ui = ui.clone();
        let slot = quit_slot.clone();
        quit_btn.connect_clicked(move |_| {
            run_bg(&ui, &slot, || {
                cli("quit")
                    .map(|_| "面板已退出".to_string())
                    .map_err(|e| e.to_string())
            });
        });
    }

    // —— 信号：注册 / 移除快捷键 ——
    {
        let ui = ui.clone();
        let slot = reg_slot.clone();
        reg_btn.connect_clicked(move |_| {
            let binding = ui.selected_binding();
            run_bg(&ui, &slot, move || {
                // load-modify-save：保留 place_offset_up 等已有配置
                let mut cfg = panel::Config::load();
                cfg.hotkey = binding.clone();
                cfg.save().ok();
                match panel_command().and_then(|cmd| panel::register_hotkey(&binding, &cmd)) {
                    Ok(()) => Ok(format!("已注册 {binding}")),
                    Err(e) => Err(e.to_string()),
                }
            });
        });
    }
    {
        let ui = ui.clone();
        let slot = unreg_slot.clone();
        unreg_btn.connect_clicked(move |_| {
            run_bg(&ui, &slot, || {
                panel::remove_hotkey()
                    .map(|_| "已从系统移除".to_string())
                    .map_err(|e| e.to_string())
            });
        });
    }

    // —— 信号：下拉切换即保存配置 ——
    {
        let ui = ui.clone();
        let preset = ui.preset.clone();
        preset.connect_selected_notify(move |d| {
            if ui.hotkey_sync.get() {
                return;
            }
            if let Some((_, binding)) = PRESETS.get(d.selected() as usize) {
                let mut cfg = panel::Config::load();
                cfg.hotkey = binding.to_string();
                cfg.save().ok();
            }
        });
    }

    // —— 信号：自启开关 ——
    {
        let ui = ui.clone();
        let sw = ui.autostart_switch.clone();
        sw.connect_active_notify(move |sw| {
            if ui.autostart_sync.get() {
                return;
            }
            if let Err(e) = set_autostart(sw.is_active()) {
                ui.autostart_state.set_text(&format!("操作失败：{e}"));
            }
            ui.refresh();
        });
    }

    // —— 轮询：后台任务结果（150ms）+ 周期刷新（5s）——
    {
        let ui = ui.clone();
        let slots = [reg_slot, unreg_slot, quit_slot, pos_slot];
        let mut idx = 0usize;
        glib::timeout_add_local(Duration::from_millis(150), move || {
            let slot = &slots[idx % slots.len()];
            idx += 1;
            if let Some(res) = slot.lock().unwrap().take() {
                ui.busy.set(false);
                ui.hotkey_result.set_text(&res);
                ui.refresh();
            }
            glib::ControlFlow::Continue
        });
    }
    {
        let ui = ui.clone();
        glib::timeout_add_local(Duration::from_secs(5), move || {
            ui.refresh();
            glib::ControlFlow::Continue
        });
    }

    ui.refresh();
    page_shell("呼出面板", &scroll_clamp(&main_box, 720))
}

impl Ui {
    fn selected_binding(&self) -> String {
        PRESETS
            .get(self.preset.selected() as usize)
            .map(|(_, b)| b.to_string())
            .unwrap_or_else(|| panel::DEFAULT_HOTKEY.to_string())
    }

    fn refresh(&self) {
        // 面板进程资源
        self.panel_res.set_text(&match yihu_core::find_process_stats("yihu-panel") {
            Some((rss, pss)) => format!(
                "运行中 · RSS {:.0} MB · 实际 {:.0} MB",
                mb(rss),
                mb(pss)
            ),
            None => "未运行（呼出快捷键或「启动面板」会自动拉起）".into(),
        });

        // 当前注册的快捷键
        let registered = panel::registered_hotkey();
        let text = match &registered {
            Some(b) => format!("已注册（{b}）"),
            None => "未注册".to_string(),
        };
        self.hotkey_state.set_text(&text);
        // 下拉框同步：已注册项优先，其次本地配置（防回环）
        let current = registered.or_else(|| Some(panel::Config::load().hotkey));
        if let Some(b) = current {
            if let Some(pos) = PRESETS.iter().position(|(_, pb)| pb == &b) {
                self.hotkey_sync.set(true);
                self.preset.set_selected(pos as u32);
                self.hotkey_sync.set(false);
            }
        }

        // 开机自启
        let enabled = autostart_enabled();
        self.autostart_sync.set(true);
        self.autostart_switch.set_active(enabled);
        self.autostart_sync.set(false);
        self.autostart_state.set_text(if enabled {
            "已启用：登录后面板隐藏待命"
        } else {
            "未启用"
        });
        if !panel_path().is_ok_and(|p| p.exists()) {
            self.autostart_state.set_text("未找到 yihu-panel（请先构建或安装发布包）");
        }

        // 定位扩展
        self.pos_state.set_text(if shell_ext::installed() {
            "已安装（重新登录后生效）"
        } else {
            "未安装（位置由系统摆放，不固定）"
        });
    }
}

// ---- 执行端定位与调用 ----

fn panel_path() -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    exe.parent()
        .ok_or_else(|| io::Error::other("无法定位安装目录"))
        .map(|d| d.join("yihu-panel"))
}

/// 注册到系统的命令：绝对路径 + toggle 子命令
fn panel_command() -> io::Result<String> {
    Ok(format!("{} toggle", panel_path()?.display()))
}

/// 调用面板 CLI（quit 等短命令），阻塞式——仅限后台线程使用
fn cli(sub: &str) -> io::Result<()> {
    let st = Command::new(panel_path()?).arg(sub).status()?;
    if st.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("yihu-panel {sub} 失败：{st}")))
    }
}

fn run_bg<F>(ui: &Rc<Ui>, slot: &Slot, f: F)
where
    F: FnOnce() -> Result<String, String> + Send + 'static,
{
    if ui.busy.get() {
        return;
    }
    ui.busy.set(true);
    ui.hotkey_result.set_text("处理中…");
    let slot = slot.clone();
    std::thread::spawn(move || {
        let res = f();
        *slot.lock().unwrap() = Some(match res {
            Ok(msg) => msg,
            Err(e) => format!("失败：{e}"),
        });
    });
}

fn fs_null() -> std::process::Stdio {
    std::process::Stdio::null()
}

// ---- 自启动（yihu-panel.desktop，指向面板守护进程）----

fn autostart_path() -> PathBuf {
    let legacy = home().join(".config/autostart").join(AUTOSTART_NAME);
    if legacy.exists() {
        return legacy;
    }
    std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|v| v.starts_with('/'))
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
        .join("autostart")
        .join(AUTOSTART_NAME)
}

fn autostart_enabled() -> bool {
    autostart_path().exists()
}

fn set_autostart(on: bool) -> io::Result<()> {
    if on {
        let exe = panel_path()?;
        let content = format!(
            "[Desktop Entry]\nType=Application\nName=一呼面板\n\
             Comment=一呼即出的工具面板\nExec=\"{}\"\n\
             Icon=tools.yihu.desktop\nTerminal=false\n\
             X-GNOME-Autostart-enabled=true\n",
            exe.display()
        );
        let p = autostart_path();
        fs::create_dir_all(p.parent().expect("自启动路径必有父目录"))?;
        fs::write(&p, content)?;
        // 显式 0644：umask 002 的系统上 fs::write 会得到 664（组可写），
        // 会被发布包安装器的安全检查拒绝
        fs::set_permissions(&p, fs::Permissions::from_mode(0o644))?;
    } else {
        match fs::remove_file(autostart_path()) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

// ---- UI 小工具（同 page_settings 惯例）----

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
    v.set_halign(Align::Start);
    row.append(&t);
    row.append(&v);
    (row, v)
}

fn mb(kb: u64) -> f64 {
    kb as f64 / 1024.0
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
}
