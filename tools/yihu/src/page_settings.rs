//! 中心「设置」页：开机启动与软件自身资源占用。

use adw::prelude::*;
use gtk::glib;
use gtk::{Align, Box as GtkBox, Label, Orientation, Switch};
use std::cell::Cell;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::time::Duration;

use crate::{page_shell, scroll_clamp};

const AUTOSTART_NAME: &str = "yihu.desktop";
const TIMER: &str = "yihu-autodark.timer";

struct Ui {
    autostart_sync: Cell<bool>,
    autostart_switch: Switch,
    autostart_state: Label,
    center_res: Label,
    agent_res: Label,
    ext_res: Label,
}

pub fn build_page() -> gtk::Widget {
    // —— 卡片：开机启动 ——
    let as_card = card();
    let row = GtkBox::new(Orientation::Horizontal, 12);
    let title_col = GtkBox::new(Orientation::Vertical, 2);
    let t = Label::new(Some("开机启动"));
    t.add_css_class("row-title");
    let autostart_state = Label::new(Some("未启用"));
    autostart_state.add_css_class("dim-label");
    autostart_state.add_css_class("caption-sm");
    title_col.append(&t);
    title_col.append(&autostart_state);
    title_col.set_hexpand(true);
    title_col.set_valign(Align::Center);
    let autostart_switch = Switch::new();
    autostart_switch.set_valign(Align::Center);
    row.append(&title_col);
    row.append(&autostart_switch);
    as_card.append(&row);
    let as_hint = Label::new(Some(
        "此开关只影响中心界面是否随登录自动打开；主题切换、右键菜单、\
         应用跟随等能力均由后台执行端驱动，与本界面是否运行无关。",
    ));
    as_hint.add_css_class("dim-label");
    as_hint.add_css_class("caption-sm");
    as_hint.set_wrap(true);
    as_hint.set_halign(Align::Start);
    as_card.append(&as_hint);

    // —— 卡片：资源占用 ——
    let res_card = card();
    let res_title = Label::new(Some("资源占用"));
    res_title.add_css_class("sec-title");
    res_title.set_halign(Align::Start);
    res_card.append(&res_title);
    let (c_row, center_res) = status_row("一呼中心");
    let (a_row, agent_res) = status_row("主题切换 agent");
    let (e_row, ext_res) = status_row("Nautilus 扩展");
    res_card.append(&c_row);
    res_card.append(&a_row);
    res_card.append(&e_row);
    let res_hint = Label::new(Some(
        "RSS 为系统监视器口径（含共享库），实际物理占用约为其一半（PSS 口径）。",
    ));
    res_hint.add_css_class("dim-label");
    res_hint.add_css_class("caption-sm");
    res_hint.set_wrap(true);
    res_hint.set_halign(Align::Start);
    res_card.append(&res_hint);

    // —— 布局 ——
    let main_box = GtkBox::new(Orientation::Vertical, 14);
    main_box.set_margin_top(20);
    main_box.set_margin_bottom(28);
    main_box.set_margin_start(24);
    main_box.set_margin_end(24);
    main_box.set_valign(Align::Start);
    for c in [&as_card, &res_card] {
        c.set_hexpand(true);
        main_box.append(c);
    }

    let ui = Rc::new(Ui {
        autostart_sync: Cell::new(false),
        autostart_switch: autostart_switch.clone(),
        autostart_state,
        center_res,
        agent_res,
        ext_res,
    });

    // —— 信号 ——
    {
        let ui_c = ui.clone();
        autostart_switch.connect_active_notify(move |sw| {
            if ui_c.autostart_sync.get() {
                return;
            }
            if let Err(e) = set_autostart(sw.is_active()) {
                ui_c.autostart_state.set_text(&format!("操作失败：{e}"));
            }
            ui_c.refresh();
        });
    }
    {
        let ui_c = ui.clone();
        glib::timeout_add_local(Duration::from_secs(5), move || {
            ui_c.refresh();
            glib::ControlFlow::Continue
        });
    }

    ui.refresh();
    page_shell("设置", &scroll_clamp(&main_box, 720))
}

impl Ui {
    fn refresh(&self) {
        // 开机启动
        let enabled = autostart_enabled();
        self.autostart_sync.set(true);
        self.autostart_switch.set_active(enabled);
        self.autostart_sync.set(false);
        self.autostart_state.set_text(if enabled {
            "已启用：登录后自动打开中心"
        } else {
            "未启用"
        });

        // 中心进程资源
        self.center_res.set_text(&match mt_core::find_process_stats("yihu") {
            Some((rss, pss)) => {
                format!("运行中 · RSS {:.0} MB · 实际 {:.0} MB", mb(rss), mb(pss))
            }
            None => "未运行".into(),
        });

        // agent：零常驻，只报定时器状态
        let timer_active = systemctl(&["is-active", TIMER]).as_deref() == Some("active");
        self.agent_res.set_text(if timer_active {
            "零常驻（定时器每分钟拉起，单次存活约 50 ms）"
        } else {
            "未启用（在「主题切换」页开启）"
        });

        // Nautilus 扩展
        self.ext_res.set_text(if nautilus_ext_installed() {
            "寄生于 Nautilus 进程，无独立占用"
        } else {
            "未启用（在「右键菜单」页开启）"
        });
    }
}

// ---- 自启动 ----

fn autostart_path() -> PathBuf {
    // Keep an existing legacy entry manageable after an XDG directory change.
    let legacy = home().join(".config/autostart").join(AUTOSTART_NAME);
    if legacy.exists() { return legacy; }
    std::env::var("XDG_CONFIG_HOME").ok().filter(|v| v.starts_with('/'))
        .map(PathBuf::from).unwrap_or_else(|| home().join(".config"))
        .join("autostart").join(AUTOSTART_NAME)
}

pub(crate) fn autostart_enabled() -> bool {
    autostart_path().exists()
}

pub(crate) fn set_autostart(on: bool) -> io::Result<()> {
    if on {
        let exe = std::env::current_exe()?;
        let content = format!(
            "[Desktop Entry]\nType=Application\nName=一呼\n\
             Comment=杂而全的 Ubuntu 桌面工具箱\nExec=\"{}\"\n\
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

// ---- 进程资源读取 ----

fn mb(kb: u64) -> f64 {
    kb as f64 / 1024.0
}

fn nautilus_ext_installed() -> bool {
    home().join(".local/share/nautilus-python/extensions/copy_absolute_path.py").exists()
}

fn systemctl(args: &[&str]) -> Option<String> {
    let out = Command::new("systemctl").arg("--user").args(args).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
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
