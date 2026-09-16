//! 中心「主题切换」页：由独立版 autodark GUI 平移而来。
//!
//! 配置写入 `~/.config/minitools/autodark.conf`，启停 systemd 用户
//! 定时器；实际切换由无 UI 的 `autodark-agent` 每分钟执行。

use adw::prelude::*;
use gtk::{
    Align, Box as GtkBox, Button, CheckButton, Entry, Grid, Label, Orientation,
    SpinButton, Switch,
};
use gtk::glib;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::time::Duration;

use mt_core::autodark::{self, Config, Mode};
use mt_core::sun;

use crate::page_shell;

const SERVICE: &str = "yihu-autodark.service";
const TIMER: &str = "yihu-autodark.timer";

struct Ui {
    enable_switch: Switch,
    state_caption: Label,
    rb_custom: CheckButton,
    rb_sun: CheckButton,
    custom_box: GtkBox,
    sun_box: GtkBox,
    light_h: SpinButton,
    light_m: SpinButton,
    dark_h: SpinButton,
    dark_m: SpinButton,
    lat_entry: Entry,
    lon_entry: Entry,
    sun_info: Label,
    cur_theme: Label,
    next_switch: Label,
    timer_state: Label,
}

impl Ui {
    fn collect(&self) -> Config {
        let mut c = Config {
            enabled: self.enable_switch.is_active(),
            mode: if self.rb_sun.is_active() { Mode::Sun } else { Mode::Custom },
            light_time: (self.light_h.value() as u32, self.light_m.value() as u32),
            dark_time: (self.dark_h.value() as u32, self.dark_m.value() as u32),
            latitude: self.lat_entry.text().parse().unwrap_or(30.66),
            longitude: self.lon_entry.text().parse().unwrap_or(104.06),
        };
        c.latitude = c.latitude.clamp(-89.0, 89.0);
        c.longitude = c.longitude.clamp(-180.0, 180.0);
        c
    }

    fn save(&self) -> Config {
        let cfg = self.collect();
        cfg.save().ok();
        cfg
    }

    fn refresh(&self) {
        let cfg = self.collect();
        let Ok(now) = autodark::LocalTime::now() else {
            return;
        };

        self.cur_theme
            .set_text(autodark::current_scheme().map(|t| t.name()).unwrap_or("未知"));

        if cfg.enabled {
            self.next_switch.set_text(
                &autodark::next_transition(&cfg, &now)
                    .map(|(t, th)| {
                        let mins = ((t - now.hours()) * 60.0).round().max(0.0) as i64;
                        format!("{} → {}（{}）", fmt_hm(t), th.name(), fmt_dur(mins))
                    })
                    .unwrap_or_else(|| "无法计算".into()),
            );
        } else {
            self.next_switch.set_text("未启用");
        }

        self.timer_state
            .set_text(match systemctl(&["is-active", TIMER]).as_deref() {
                Ok("active") => "运行中",
                Ok(_) => "已停用",
                Err(_) => "不可用",
            });

        self.sun_info.set_text(
            &if cfg.mode == Mode::Sun {
                sun::sun_times(now.year, now.month, now.day, cfg.latitude, cfg.longitude, now.utc_offset_hours)
                    .map(|(r, s)| format!("今日日出 {} · 日落 {}", fmt_hm(r), fmt_hm(s)))
                    .unwrap_or_else(|| "今日无日出/日落（极昼或极夜）".into())
            } else {
                "—".into()
            },
        );

        self.state_caption.set_text(if cfg.enabled {
            "自动切换已启用，定时器每分钟核对一次主题"
        } else {
            "自动切换未启用"
        });
        self.state_caption.remove_css_class("state-ok");
        self.state_caption.remove_css_class("state-off");
        self.state_caption
            .add_css_class(if cfg.enabled { "state-ok" } else { "state-off" });
    }
}

pub fn build_page() -> gtk::Widget {
    let cfg = Config::load();

    // —— 卡片：切换 ——
    let switch_card = card();
    let row = GtkBox::new(Orientation::Horizontal, 12);
    let title_col = GtkBox::new(Orientation::Vertical, 2);
    let t = Label::new(Some("启用自动主题切换"));
    t.add_css_class("row-title");
    let state_caption = Label::new(Some("自动切换未启用"));
    state_caption.add_css_class("dim-label");
    state_caption.add_css_class("caption-sm");
    title_col.append(&t);
    title_col.append(&state_caption);
    title_col.set_hexpand(true);
    title_col.set_valign(Align::Center);
    let enable_switch = Switch::new();
    enable_switch.set_active(cfg.enabled);
    enable_switch.set_valign(Align::Center);
    row.append(&title_col);
    row.append(&enable_switch);
    switch_card.append(&row);

    // —— 卡片：模式 ——
    let mode_card = card();
    let rb_custom = CheckButton::with_label("自定义时间");
    let rb_sun = CheckButton::with_label("日出至日落（根据地理坐标）");
    rb_sun.set_group(Some(&rb_custom));
    match cfg.mode {
        Mode::Custom => rb_custom.set_active(true),
        Mode::Sun => rb_sun.set_active(true),
    }
    mode_card.append(&rb_custom);

    let custom_box = GtkBox::new(Orientation::Vertical, 6);
    let grid = Grid::new();
    grid.set_column_spacing(8);
    grid.set_row_spacing(6);
    grid.set_margin_start(28);
    let mk_spin = |v: u32| {
        let s = SpinButton::with_range(0.0, 59.0, 1.0);
        s.set_value(v as f64);
        s.add_css_class("spin-hm");
        s
    };
    let light_h = mk_spin(cfg.light_time.0);
    let light_m = mk_spin(cfg.light_time.1);
    let dark_h = mk_spin(cfg.dark_time.0);
    let dark_m = mk_spin(cfg.dark_time.1);
    let mk_label = |s: &str| {
        let l = Label::new(Some(s));
        l.set_valign(Align::Center);
        l
    };
    grid.attach(&mk_label("浅色"), 0, 0, 1, 1);
    grid.attach(&light_h, 1, 0, 1, 1);
    grid.attach(&mk_label(":"), 2, 0, 1, 1);
    grid.attach(&light_m, 3, 0, 1, 1);
    grid.attach(&mk_label("深色"), 0, 1, 1, 1);
    grid.attach(&dark_h, 1, 1, 1, 1);
    grid.attach(&mk_label(":"), 2, 1, 1, 1);
    grid.attach(&dark_m, 3, 1, 1, 1);
    custom_box.append(&grid);
    mode_card.append(&custom_box);

    mode_card.append(&rb_sun);
    let sun_box = GtkBox::new(Orientation::Vertical, 6);
    let coord_row = GtkBox::new(Orientation::Horizontal, 8);
    let lat_entry = Entry::new();
    lat_entry.set_text(&format!("{}", cfg.latitude));
    lat_entry.add_css_class("coord");
    let lon_entry = Entry::new();
    lon_entry.set_text(&format!("{}", cfg.longitude));
    lon_entry.add_css_class("coord");
    coord_row.append(&mk_label("纬度"));
    coord_row.append(&lat_entry);
    coord_row.append(&mk_label("经度"));
    coord_row.append(&lon_entry);
    coord_row.set_margin_start(28);
    sun_box.append(&coord_row);
    let sun_info = Label::new(Some("—"));
    sun_info.add_css_class("dim-label");
    sun_info.set_margin_start(28);
    sun_box.append(&sun_info);
    mode_card.append(&sun_box);

    // —— 卡片：状态 ——
    let st_card = card();
    let (cur_row, cur_theme) = status_row("当前主题");
    let (next_row, next_switch) = status_row("下次切换");
    let (timer_row, timer_state) = status_row("定时器");
    st_card.append(&cur_row);
    st_card.append(&next_row);
    st_card.append(&timer_row);
    let apply_row = GtkBox::new(Orientation::Horizontal, 8);
    let hint = Label::new(Some("配置修改即时保存；启用后立即生效，此后每分钟核对。"));
    hint.add_css_class("dim-label");
    hint.add_css_class("caption-sm");
    hint.set_hexpand(true);
    hint.set_halign(Align::Start);
    hint.set_valign(Align::Center);
    let apply_btn = Button::with_label("立即应用");
    apply_btn.add_css_class("suggested-action");
    apply_row.append(&hint);
    apply_row.append(&apply_btn);
    st_card.append(&apply_row);

    // —— 布局 ——
    let main_box = GtkBox::new(Orientation::Vertical, 14);
    main_box.set_margin_top(20);
    main_box.set_margin_bottom(28);
    main_box.set_margin_start(24);
    main_box.set_margin_end(24);
    main_box.set_valign(Align::Start);
    for c in [&switch_card, &mode_card, &st_card] {
        c.set_hexpand(true);
        main_box.append(c);
    }

    let ui = Rc::new(Ui {
        enable_switch: enable_switch.clone(),
        state_caption,
        rb_custom: rb_custom.clone(),
        rb_sun: rb_sun.clone(),
        custom_box: custom_box.clone(),
        sun_box: sun_box.clone(),
        light_h,
        light_m,
        dark_h,
        dark_m,
        lat_entry,
        lon_entry,
        sun_info,
        cur_theme,
        next_switch,
        timer_state,
    });

    // —— 信号 ——
    {
        let ui_c = ui.clone();
        ui.enable_switch.connect_active_notify(move |sw| {
            let mut cfg = ui_c.collect();
            cfg.enabled = sw.is_active();
            cfg.save().ok();
            let result = if cfg.enabled {
                enable_schedule()
            } else {
                disable_schedule()
            };
            if let Err(e) = result {
                ui_c.timer_state.set_text(&format!("systemctl 出错：{e}"));
            }
            if cfg.enabled {
                run_apply(&cfg);
            }
            ui_c.refresh();
        });
    }
    for rb in [&ui.rb_custom, &ui.rb_sun] {
        let ui_c = ui.clone();
        rb.connect_toggled(move |_| {
            sync_mode_visibility(&ui_c);
            ui_c.save();
            ui_c.refresh();
        });
    }
    for sp in [&ui.light_h, &ui.light_m, &ui.dark_h, &ui.dark_m] {
        let ui_c = ui.clone();
        sp.connect_value_changed(move |_| {
            ui_c.save();
            ui_c.refresh();
        });
    }
    for entry in [&ui.lat_entry, &ui.lon_entry] {
        let ui_c = ui.clone();
        entry.connect_changed(move |_| {
            ui_c.save();
            ui_c.refresh();
        });
    }
    {
        let ui_c = ui.clone();
        apply_btn.connect_clicked(move |_| {
            let cfg = ui_c.save();
            run_apply(&cfg);
            ui_c.refresh();
        });
    }
    {
        let ui_c = ui.clone();
        glib::timeout_add_local(Duration::from_secs(10), move || {
            ui_c.refresh();
            glib::ControlFlow::Continue
        });
    }

    sync_mode_visibility(&ui);
    ui.refresh();

    page_shell("主题切换", &crate::scroll_clamp(&main_box, 760))
}

fn sync_mode_visibility(ui: &Ui) {
    let sun = ui.rb_sun.is_active();
    ui.custom_box.set_visible(!sun);
    ui.sun_box.set_visible(sun);
}

fn run_apply(cfg: &Config) {
    cfg.save().ok();
    if let Ok(agent) = agent_path() {
        Command::new(agent).arg("apply").status().ok();
    }
}

fn enable_schedule() -> io::Result<()> {
    write_units()?;
    run("systemctl", &["--user", "daemon-reload"])?;
    run("systemctl", &["--user", "enable", "--now", TIMER])
}

fn disable_schedule() -> io::Result<()> {
    run("systemctl", &["--user", "disable", "--now", TIMER])
}

fn write_units() -> io::Result<()> {
    let agent = agent_path()?;
    let dir = config_home().join("systemd/user");
    fs::create_dir_all(&dir)?;
    let service = format!(
        "[Unit]\nDescription=一呼 AutoDark 应用主题切换\n\n[Service]\nType=oneshot\nExecStart=\"{}\" apply\n",
        agent.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%").replace('$', "$$")
    );
    let timer = format!(
        "[Unit]\nDescription=一呼 AutoDark 定时核对\n\n[Timer]\nOnCalendar=*-*-* *:*:00\nAccuracySec=15s\nPersistent=true\nUnit={SERVICE}\n\n[Install]\nWantedBy=timers.target\n"
    );
    fs::write(dir.join(SERVICE), service)?;
    fs::write(dir.join(TIMER), timer)
}

fn agent_path() -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe.parent().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "无上级目录"))?;
    let p = dir.join("autodark-agent");
    if !p.exists() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "未找到 autodark-agent，请先 cargo build --release"));
    }
    Ok(p)
}

fn config_home() -> PathBuf {
    std::env::var("XDG_CONFIG_HOME").ok().filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into())).join(".config"))
}

fn run(prog: &str, args: &[&str]) -> io::Result<()> {
    let st = Command::new(prog).args(args).status()?;
    if st.success() {
        Ok(())
    } else {
        Err(io::Error::new(io::ErrorKind::Other, format!("{prog} {args:?} 失败")))
    }
}

fn systemctl(args: &[&str]) -> io::Result<String> {
    let out = Command::new("systemctl").arg("--user").args(args).output()?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
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

fn fmt_hm(h: f64) -> String {
    let total = (h * 60.0).round() as i64;
    format!("{:02}:{:02}", (total / 60).rem_euclid(24), total.rem_euclid(60))
}

fn fmt_dur(mins: i64) -> String {
    let (h, m) = (mins / 60, mins % 60);
    match (h, m) {
        (0, m) => format!("{m} 分钟后"),
        (h, 0) => format!("{h} 小时后"),
        (h, m) => format!("{h} 小时 {m} 分后"),
    }
}
