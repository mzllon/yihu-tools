//! SysDash — 一呼的退役系统仪表盘（GTK4 + libadwaita 原生实现）。
//!
//! 结构约定：`yihu-core` 负责系统信息读取，UI 在主线程用
//! `glib::timeout_add_local` 每秒采样刷新。无 WebView、无 IPC。

use adw::prelude::*;
use adw::{Application, ApplicationWindow, Clamp, HeaderBar};
use gtk::{
    Align, Box as GtkBox, DrawingArea, FlowBox, FlowBoxChild, Label,
    Orientation, PolicyType, ProgressBar, ScrolledWindow,
};
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

const APP_ID: &str = "tools.yihu.sysdash";
const MAX_POINTS: usize = 90;

fn main() -> glib::ExitCode {
    // 仪表盘每秒才刷新一次，软件渲染足矣；GL 渲染器会把 Mesa/NVIDIA
    // 驱动栈（libLLVM、shader 编译器）整块映射进进程（实测 RSS 183MB → 79MB）。
    std::env::set_var("GSK_RENDERER", "cairo");
    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run()
}

// ---- 状态与刷新 ----

struct Dashboard {
    prev_cpu: RefCell<Option<yihu_core::CpuTimes>>,
    history: RefCell<Vec<f32>>,
    core_bars: RefCell<Vec<(ProgressBar, Label)>>,
    cores_box: FlowBox,
    cpu_pct: Label,
    spark: DrawingArea,
    mem_bar: ProgressBar,
    mem_text: Label,
    swap_bar: ProgressBar,
    swap_text: Label,
    disk_bar: ProgressBar,
    disk_text: Label,
    load_labels: Vec<Label>,
    uptime_label: Label,
}

impl Dashboard {
    fn refresh(&self) {
        // CPU：与上次采样差分出使用率
        let Ok(cur) = yihu_core::read_cpu_times() else {
            return;
        };
        let (pct, per_core) = match self.prev_cpu.borrow().as_ref() {
            Some(p) => yihu_core::cpu_usage(p, &cur),
            None => (0.0, vec![0.0; cur.cores.len()]),
        };
        *self.prev_cpu.borrow_mut() = Some(cur);

        self.cpu_pct.set_text(&format!("{}", pct.round() as u64));
        {
            let mut h = self.history.borrow_mut();
            h.push(pct);
            while h.len() > MAX_POINTS {
                h.remove(0);
            }
        }
        self.spark.queue_draw();
        self.ensure_core_bars(per_core.len());
        {
            let bars = self.core_bars.borrow();
            for ((bar, lbl), v) in bars.iter().zip(per_core.iter()) {
                bar.set_fraction((*v as f64 / 100.0).clamp(0.0, 1.0));
                lbl.set_text(&format!("{}%", v.round() as u64));
            }
        }

        if let Ok(m) = yihu_core::read_mem_info() {
            set_bar(&self.mem_bar, &self.mem_text, m.used_kb(), m.total_kb, fmt_kib);
            set_bar(&self.swap_bar, &self.swap_text, m.swap_used_kb, m.swap_total_kb, fmt_kib);
        }

        if let Ok(d) = yihu_core::read_disk_usage("/") {
            set_bar(&self.disk_bar, &self.disk_text, d.used_bytes(), d.total_bytes, fmt_bytes);
        }

        if let Ok(load) = yihu_core::read_loadavg() {
            for (lbl, v) in self.load_labels.iter().zip(load.iter()) {
                lbl.set_text(&format!("{v:.2}"));
            }
        }
        if let Ok(secs) = yihu_core::read_uptime_secs() {
            self.uptime_label.set_text(&fmt_uptime(secs as u64));
        }
    }

    /// 每核占用条：首次刷新时按核数构建。
    fn ensure_core_bars(&self, n: usize) {
        let mut bars = self.core_bars.borrow_mut();
        if bars.len() == n {
            return;
        }
        while let Some(child) = self.cores_box.child_at_index(0) {
            self.cores_box.remove(&child);
        }
        bars.clear();
        for _ in 0..n {
            let bar = ProgressBar::new();
            bar.add_css_class("core");
            bar.set_hexpand(true);
            bar.set_valign(Align::Center);
            let lbl = Label::new(Some("–"));
            lbl.add_css_class("dim-label");
            lbl.set_width_chars(4);
            let row = GtkBox::new(Orientation::Horizontal, 8);
            row.append(&bar);
            row.append(&lbl);
            let cell = FlowBoxChild::new();
            cell.set_child(Some(&row));
            self.cores_box.append(&cell);
            bars.push((bar, lbl));
        }
    }
}

fn set_bar(bar: &ProgressBar, text: &Label, used: u64, total: u64, fmt: fn(u64) -> String) {
    let f = if total > 0 { used as f64 / total as f64 } else { 0.0 };
    bar.set_fraction(f.clamp(0.0, 1.0));
    text.set_text(&format!(
        "{} / {}（{}%）",
        fmt(used),
        fmt(total),
        (f * 100.0).round() as u64
    ));
}

fn fmt_kib(kib: u64) -> String {
    let gib = kib as f64 / (1024.0 * 1024.0);
    if gib >= 1.0 { format!("{gib:.1} GB") } else { format!("{} MB", kib / 1024) }
}

fn fmt_bytes(bytes: u64) -> String {
    let gib = bytes as f64 / 1024.0f64.powi(3);
    if gib >= 1.0 { format!("{gib:.1} GB") } else { format!("{} MB", bytes / 1024 / 1024) }
}

fn fmt_uptime(secs: u64) -> String {
    let (d, h, m) = (secs / 86400, secs % 86400 / 3600, secs % 3600 / 60);
    if d > 0 {
        format!("{d} 天 {h} 小时")
    } else if h > 0 {
        format!("{h} 小时 {m} 分")
    } else {
        format!("{m} 分钟")
    }
}

// ---- UI 构建 ----

fn build_ui(app: &Application) {
    // 跟随系统深浅色模式（不在代码里强制深色）
    load_css();

    let host = yihu_core::read_host_info();

    // 头部：标题（主机名 · 内核）+ 右侧运行时长
    let header = HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(
        "SysDash",
        &format!("{} · Linux {}", host.hostname, host.kernel),
    )));
    let uptime_label = Label::new(Some("–"));
    uptime_label.add_css_class("title-4");
    let uptime_box = GtkBox::new(Orientation::Vertical, 0);
    uptime_box.set_valign(Align::Center);
    let uptime_cap = Label::new(Some("已运行"));
    uptime_cap.add_css_class("dim-label");
    uptime_cap.add_css_class("caption-sm");
    uptime_box.append(&uptime_cap);
    uptime_box.append(&uptime_label);
    header.pack_end(&uptime_box);

    // CPU 卡片
    let cpu_card = card();
    let (cpu_head, cpu_pct) = head_row("处理器");
    cpu_pct.remove_css_class("dim-label");
    cpu_pct.add_css_class("bignum");
    cpu_card.append(&cpu_head);

    let spark = DrawingArea::new();
    spark.set_content_height(76);
    spark.set_hexpand(true);
    cpu_card.append(&spark);

    let cores_box = FlowBox::new();
    cores_box.set_selection_mode(gtk::SelectionMode::None);
    cores_box.set_min_children_per_line(4);
    cores_box.set_max_children_per_line(8);
    cores_box.set_column_spacing(14);
    cores_box.set_row_spacing(8);
    cores_box.set_homogeneous(true);
    cores_box.set_hexpand(true);
    cpu_card.append(&cores_box);

    let cpu_model = Label::new(Some(&host.cpu_model));
    cpu_model.add_css_class("dim-label");
    cpu_model.add_css_class("caption-sm");
    cpu_model.set_ellipsize(gtk::pango::EllipsizeMode::End);
    cpu_card.append(&cpu_model);

    // 内存卡片
    let mem_card = card();
    let (mem_head, mem_text) = head_row("内存");
    mem_card.append(&mem_head);
    let mem_bar = ProgressBar::new();
    style_bar(&mem_bar, &["big", "mem"]);
    mem_card.append(&mem_bar);
    let (swap_head, swap_text) = head_row("交换分区");
    swap_head.add_css_class("sub-head");
    mem_card.append(&swap_head);
    let swap_bar = ProgressBar::new();
    style_bar(&swap_bar, &["slim", "swap"]);
    mem_card.append(&swap_bar);

    // 磁盘 + 负载卡片
    let sys_card = card();
    let (disk_head, disk_text) = head_row("磁盘（/）");
    sys_card.append(&disk_head);
    let disk_bar = ProgressBar::new();
    style_bar(&disk_bar, &["big", "disk"]);
    sys_card.append(&disk_bar);
    let (load_head, load_note) = head_row("负载均值");
    load_note.set_text("1 / 5 / 15 分钟");
    load_head.add_css_class("sub-head");
    sys_card.append(&load_head);
    let load_row = GtkBox::new(Orientation::Horizontal, 8);
    let load_labels: Vec<Label> = (0..3)
        .map(|_| {
            let chip = GtkBox::new(Orientation::Vertical, 0);
            chip.add_css_class("load-chip");
            chip.set_hexpand(true);
            let l = Label::new(Some("–"));
            l.set_halign(Align::Center);
            chip.append(&l);
            load_row.append(&chip);
            l
        })
        .collect();
    sys_card.append(&load_row);

    // 总布局
    let rows = GtkBox::new(Orientation::Horizontal, 14);
    mem_card.set_hexpand(true);
    sys_card.set_hexpand(true);
    rows.append(&mem_card);
    rows.append(&sys_card);

    cpu_card.set_hexpand(true);
    let main_box = GtkBox::new(Orientation::Vertical, 14);
    main_box.set_margin_top(20);
    main_box.set_margin_bottom(28);
    main_box.set_margin_start(24);
    main_box.set_margin_end(24);
    main_box.set_valign(Align::Start);
    main_box.append(&cpu_card);
    main_box.append(&rows);

    let clamp = Clamp::builder().maximum_size(1000).build();
    clamp.set_child(Some(&main_box));
    let scroll = ScrolledWindow::new();
    scroll.set_policy(PolicyType::Never, PolicyType::Automatic);
    scroll.set_child(Some(&clamp));
    scroll.set_vexpand(true);

    let content = GtkBox::new(Orientation::Vertical, 0);
    content.append(&header);
    content.append(&scroll);

    let window = ApplicationWindow::builder()
        .application(app)
        .title("SysDash · 系统仪表盘")
        .default_width(960)
        .default_height(680)
        .icon_name(APP_ID)
        .content(&content)
        .build();

    // 状态与每秒刷新循环
    let dash = Rc::new(Dashboard {
        prev_cpu: RefCell::new(None),
        history: RefCell::new(Vec::new()),
        core_bars: RefCell::new(Vec::new()),
        cores_box: cores_box.clone(),
        cpu_pct: cpu_pct.clone(),
        spark: spark.clone(),
        mem_bar,
        mem_text,
        swap_bar,
        swap_text,
        disk_bar,
        disk_text,
        load_labels,
        uptime_label: uptime_label.clone(),
    });
    dash.refresh();
    {
        let dash = dash.clone();
        glib::timeout_add_local(Duration::from_millis(1000), move || {
            dash.refresh();
            glib::ControlFlow::Continue
        });
    }
    {
        let dash = dash.clone();
        spark.set_draw_func(move |_, cr, w, h| draw_spark(&dash, cr, w as f64, h as f64));
    }

    window.present();
}

fn draw_spark(dash: &Dashboard, cr: &gtk::cairo::Context, w: f64, h: f64) {
    // 参考网格线：25% / 50% / 75%（颜色跟随系统深浅色模式）
    let dark = adw::StyleManager::default().is_dark();
    if dark {
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.06);
    } else {
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.10);
    }
    for f in [0.25, 0.5, 0.75] {
        cr.rectangle(0.0, h * f, w, 1.0);
        let _ = cr.fill();
    }

    let hist = dash.history.borrow();
    if hist.len() < 2 {
        return;
    }
    let step = w / (MAX_POINTS as f64 - 1.0);
    let x0 = w - (hist.len() as f64 - 1.0) * step;
    let y = |v: f64| h - (v / 100.0) * (h - 8.0) - 3.0;

    // 面积
    cr.set_source_rgba(233.0 / 255.0, 84.0 / 255.0, 32.0 / 255.0, 0.30);
    cr.move_to(x0, h);
    for (i, v) in hist.iter().enumerate() {
        cr.line_to(x0 + i as f64 * step, y(*v as f64));
    }
    cr.line_to(x0 + (hist.len() - 1) as f64 * step, h);
    cr.close_path();
    let _ = cr.fill();

    // 折线
    cr.set_source_rgba(233.0 / 255.0, 84.0 / 255.0, 32.0 / 255.0, 1.0);
    cr.set_line_width(2.0);
    cr.set_line_join(gtk::cairo::LineJoin::Round);
    cr.move_to(x0, y(hist[0] as f64));
    for (i, v) in hist.iter().enumerate() {
        cr.line_to(x0 + i as f64 * step, y(*v as f64));
    }
    let _ = cr.stroke();
}

// ---- 小工具函数 ----

fn card() -> GtkBox {
    let b = GtkBox::new(Orientation::Vertical, 10);
    b.add_css_class("card");
    b.add_css_class("card-pad");
    b
}

/// 卡片小节标题行：左侧标题，右侧数值文本（返回该文本标签）。
fn head_row(title: &str) -> (GtkBox, Label) {
    let row = GtkBox::new(Orientation::Horizontal, 8);
    let t = Label::new(Some(title));
    t.add_css_class("sec-title");
    t.set_hexpand(true);
    t.set_halign(Align::Start);
    let v = Label::new(Some("–"));
    v.add_css_class("dim-label");
    row.append(&t);
    row.append(&v);
    (row, v)
}

fn style_bar(bar: &ProgressBar, classes: &[&str]) {
    for c in classes {
        bar.add_css_class(c);
    }
    bar.set_hexpand(true);
}

fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(include_str!("style.css"));
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}
