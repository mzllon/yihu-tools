//! 中心「主题切换」页：自动深浅色配置（2026-10-09 UI 重构）。
//!
//! 配置写入 `~/.config/yihu/autodark.conf`（旧版 `minitools` 路径自动迁移），启停 systemd 用户
//! 定时器；实际切换由无 UI 的 `autodark-agent` 每分钟执行。
//!
//! 视觉语言：主卡（开关 + 三列状态摘要 + 应用按钮）+ 双模式选中卡
//!（整卡可点，选中态强调色描边）；时间直接键入（等宽紧凑输入框，支持多种写法）。

use adw::prelude::*;
use gtk::{
    Align, Box as GtkBox, Button, CheckButton, Entry, Label, Orientation, Switch,
};
use gtk::glib;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::time::Duration;

use yihu_core::autodark::{self, Config, Mode};
use yihu_core::sun;

use crate::page_shell;

const SERVICE: &str = "yihu-autodark.service";
const TIMER: &str = "yihu-autodark.timer";

struct Ui {
    enable_switch: Switch,
    state_caption: Label,
    rb_custom: CheckButton,
    rb_sun: CheckButton,
    card_custom: GtkBox,
    card_sun: GtkBox,
    dark_start_entry: Entry,
    dark_end_entry: Entry,
    lat_entry: Entry,
    lon_entry: Entry,
    sun_info: Label,
    cur_theme: Label,
    next_switch: Label,
    timer_state: Label,
}

/// 解析时间输入，支持多种写法：`21:00`、`9:30`、`1730`、`730`、`21`、`7`。
/// 解析不出或越界（时 >23 / 分 >59）返回 None，此时不落盘、保留上次有效值。
fn parse_time(s: &str) -> Option<(u32, u32)> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (h, m) = match s.split_once(':') {
        Some((h, m)) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        None => match s.parse::<u32>() {
            // 纯数字：1-2 位视为小时，3 位 HMM，4 位 HHMM
            Ok(n) => match n {
                0..=99 => (n, 0),
                100..=959 => (n / 100, n % 100),
                1000..=2359 => (n / 100, n % 100),
                _ => return None,
            },
            Err(_) => return None,
        },
    };
    (h <= 23 && m <= 59).then_some((h, m))
}

fn fmt_tm(t: (u32, u32)) -> String {
    format!("{:02}:{:02}", t.0, t.1)
}

impl Ui {
    fn collect(&self) -> Config {
        // 输入未完成/非法时不采纳新值：回退到磁盘上的最后一次有效配置
        let disk = Config::load();
        let mut c = Config {
            enabled: self.enable_switch.is_active(),
            mode: if self.rb_sun.is_active() { Mode::Sun } else { Mode::Custom },
            dark_start: parse_time(&self.dark_start_entry.text()).unwrap_or(disk.dark_start),
            dark_end: parse_time(&self.dark_end_entry.text()).unwrap_or(disk.dark_end),
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
                        format!("{} {}（{}）", fmt_hm(t), th.name(), fmt_dur(mins))
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
            "自动切换已启用，每分钟核对一次"
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

    // ============ 主卡：开关 + 状态摘要 + 应用 ============
    let switch_card = card();
    let row = GtkBox::new(Orientation::Horizontal, 12);
    let title_col = GtkBox::new(Orientation::Vertical, 2);
    let t = Label::new(Some("自动主题切换"));
    t.add_css_class("title-3");
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
    enable_switch.set_margin_end(4);
    row.append(&title_col);
    row.append(&enable_switch);
    switch_card.append(&row);

    // 状态三列摘要（load-chip 风格）
    let chips = GtkBox::new(Orientation::Horizontal, 10);
    chips.set_homogeneous(true);
    let (c1, cur_theme) = summary_chip("当前主题");
    let (c2, next_switch) = summary_chip("下次切换");
    let (c3, timer_state) = summary_chip("定时器");
    chips.append(&c1);
    chips.append(&c2);
    chips.append(&c3);
    switch_card.append(&chips);

    let apply_row = GtkBox::new(Orientation::Horizontal, 8);
    let hint = Label::new(Some("配置修改即时保存，启用后立即生效"));
    hint.add_css_class("dim-label");
    hint.add_css_class("caption-sm");
    hint.set_hexpand(true);
    hint.set_halign(Align::Start);
    hint.set_valign(Align::Center);
    hint.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let apply_btn = Button::with_label("立即应用");
    apply_btn.add_css_class("suggested-action");
    apply_btn.add_css_class("pill");
    apply_row.append(&hint);
    apply_row.append(&apply_btn);
    switch_card.append(&apply_row);

    // ============ 双模式选中卡 ============
    let mode_row = GtkBox::new(Orientation::Horizontal, 14);
    mode_row.set_homogeneous(true);

    let rb_custom = CheckButton::with_label("自定义时间");
    let rb_sun = CheckButton::with_label("日出至日落");
    rb_sun.set_group(Some(&rb_custom));
    match cfg.mode {
        Mode::Custom => rb_custom.set_active(true),
        Mode::Sun => rb_sun.set_active(true),
    }

    // —— 自定义时间卡 ——
    let card_custom = mode_card(&rb_custom, "每天只在深色时段用深色主题，其余自动浅色");
    let time_grid = GtkBox::new(Orientation::Vertical, 8);
    let dark_start_entry = Entry::new();
    dark_start_entry.set_text(&fmt_tm(cfg.dark_start));
    dark_start_entry.add_css_class("hm-entry");
    let dark_end_entry = Entry::new();
    dark_end_entry.set_text(&fmt_tm(cfg.dark_end));
    dark_end_entry.add_css_class("hm-entry");
    let period_row = GtkBox::new(Orientation::Horizontal, 8);
    let pl = Label::new(Some("深色时段"));
    pl.set_valign(Align::Center);
    period_row.append(&pl);
    period_row.append(&dark_start_entry);
    let sep = |s: &str| {
        let l = Label::new(Some(s));
        l.set_valign(Align::Center);
        l
    };
    period_row.append(&sep("→"));
    period_row.append(&dark_end_entry);
    time_grid.append(&period_row);
    let period_hint = Label::new(Some("可直接键入，如 1730、9:30、21；结束早于开始表示跨零点"));
    period_hint.add_css_class("caption-sm");
    period_hint.add_css_class("dim-label");
    period_hint.set_halign(Align::Start);
    time_grid.append(&period_hint);
    time_grid.set_margin_top(10);
    time_grid.set_margin_start(26);
    card_custom.append(&time_grid);

    // —— 日出至日落卡 ——
    let card_sun = mode_card(&rb_sun, "按地理位置的日出日落自动切换");
    let sun_box = GtkBox::new(Orientation::Vertical, 8);
    let coord_row = GtkBox::new(Orientation::Horizontal, 8);
    let lat_entry = Entry::new();
    lat_entry.set_text(&format!("{}", cfg.latitude));
    lat_entry.add_css_class("coord");
    let lon_entry = Entry::new();
    lon_entry.set_text(&format!("{}", cfg.longitude));
    lon_entry.add_css_class("coord");
    let mk_label = |s: &str| {
        let l = Label::new(Some(s));
        l.set_valign(Align::Center);
        l
    };
    coord_row.append(&mk_label("纬度"));
    coord_row.append(&lat_entry);
    coord_row.append(&mk_label("经度"));
    coord_row.append(&lon_entry);
    coord_row.set_margin_top(10);
    coord_row.set_margin_start(26);
    sun_box.append(&coord_row);
    let sun_info = Label::new(Some("—"));
    sun_info.add_css_class("dim-label");
    sun_info.add_css_class("caption-sm");
    sun_info.set_margin_start(26);
    sun_info.set_halign(Align::Start);
    sun_box.append(&sun_info);
    card_sun.append(&sun_box);

    mode_row.append(&card_custom);
    mode_row.append(&card_sun);

    // —— 布局 ——
    let main_box = GtkBox::new(Orientation::Vertical, 14);
    main_box.set_margin_top(20);
    main_box.set_margin_bottom(28);
    main_box.set_margin_start(24);
    main_box.set_margin_end(24);
    main_box.set_valign(Align::Start);
    for c in [&switch_card, &mode_row] {
        c.set_hexpand(true);
        main_box.append(c);
    }

    let ui = Rc::new(Ui {
        enable_switch: enable_switch.clone(),
        state_caption,
        rb_custom: rb_custom.clone(),
        rb_sun: rb_sun.clone(),
        card_custom: card_custom.clone(),
        card_sun: card_sun.clone(),
        dark_start_entry,
        dark_end_entry,
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
            sync_mode_style(&ui_c);
            ui_c.save();
            ui_c.refresh();
        });
    }
    // 整卡可点：点击卡片任意处切到该模式。不能 claim 序列——按下即抢占会把
    // 卡片内 DropDown/Entry 尚在识别中的子手势一并取消，时间下拉从此点不开（BUG-003）。
    for (card, rb) in [(&ui.card_custom, &ui.rb_custom), (&ui.card_sun, &ui.rb_sun)] {
        let rb = rb.clone();
        let gesture = gtk::GestureClick::new();
        gesture.connect_pressed(move |_, _, _, _| {
            rb.set_active(true);
        });
        card.add_controller(gesture);
    }
    // 时间框：合法输入即时保存；失焦把任意写法归一成 HH:MM；进框全选便于直接键入
    for entry in [&ui.dark_start_entry, &ui.dark_end_entry] {
        let ui_c = ui.clone();
        entry.connect_changed(move |_| {
            ui_c.save();
            ui_c.refresh();
        });
    }
    let wire_time = |entry: &Entry, pick: Box<dyn Fn(&Config) -> (u32, u32)>| {
        let fc = gtk::EventControllerFocus::new();
        let ui_c = ui.clone();
        let e = entry.clone();
        fc.connect_leave(move |_| {
            let want = fmt_tm(pick(&ui_c.collect()));
            if e.text() != want {
                e.set_text(&want);
            }
        });
        let e2 = entry.clone();
        fc.connect_enter(move |_| {
            e2.select_region(0, -1);
        });
        entry.add_controller(fc);
    };
    wire_time(&ui.dark_start_entry, Box::new(|c| c.dark_start));
    wire_time(&ui.dark_end_entry, Box::new(|c| c.dark_end));
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

    sync_mode_style(&ui);
    ui.refresh();

    page_shell("主题切换", &crate::scroll_clamp(&main_box, 760))
}

fn sync_mode_style(ui: &Ui) {
    let sun = ui.rb_sun.is_active();
    ui.card_custom.remove_css_class("mode-selected");
    ui.card_sun.remove_css_class("mode-selected");
    if sun {
        ui.card_sun.add_css_class("mode-selected");
    } else {
        ui.card_custom.add_css_class("mode-selected");
    }
}

/// 模式卡：圆钮 + 标题 + 描述，选中态由 sync_mode_style 加 mode-selected 类
fn mode_card(rb: &CheckButton, desc: &str) -> GtkBox {
    let b = GtkBox::new(Orientation::Vertical, 4);
    b.add_css_class("card");
    b.add_css_class("mode-card");
    rb.set_halign(Align::Start);
    b.append(rb);
    let d = Label::new(Some(desc));
    d.add_css_class("caption-sm");
    d.add_css_class("dim-label");
    d.set_halign(Align::Start);
    d.set_margin_start(26);
    b.append(&d);
    b
}

/// 状态摘要 chip：上方小字标签 + 下方粗体值
fn summary_chip(label: &str) -> (GtkBox, Label) {
    let b = GtkBox::new(Orientation::Vertical, 2);
    b.add_css_class("load-chip");
    let l = Label::new(Some(label));
    l.add_css_class("caption-sm");
    l.add_css_class("dim-label");
    let v = Label::new(Some("–"));
    v.add_css_class("row-title");
    v.set_ellipsize(gtk::pango::EllipsizeMode::End);
    b.append(&l);
    b.append(&v);
    (b, v)
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

#[cfg(test)]
mod tests {
    use super::parse_time;

    #[test]
    fn parse_time_formats() {
        // 带冒号 / 纯数字 / 带空白等各种写法
        assert_eq!(parse_time("21:00"), Some((21, 0)));
        assert_eq!(parse_time("9:30"), Some((9, 30)));
        assert_eq!(parse_time("1730"), Some((17, 30)));
        assert_eq!(parse_time("730"), Some((7, 30)));
        assert_eq!(parse_time("21"), Some((21, 0)));
        assert_eq!(parse_time(" 7 "), Some((7, 0)));
        assert_eq!(parse_time("0"), Some((0, 0)));
        // 非法输入一律 None：越界、残缺、垃圾字符
        assert_eq!(parse_time("24:00"), None);
        assert_eq!(parse_time("7:60"), None);
        assert_eq!(parse_time("173"), None);
        assert_eq!(parse_time("17300"), None);
        assert_eq!(parse_time("abc"), None);
        assert_eq!(parse_time(""), None);
    }
}
