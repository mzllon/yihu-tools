//! 一呼 — Ubuntu 桌面工具箱的统一管理中心。
//!
//! 侧边栏导航 + 分页：主题切换 / 右键菜单 / 应用跟随 / 设置 / 关于。
//! 各功能的执行端保持独立（autodark-agent、Nautilus 扩展），
//! 中心只承担配置与状态管理；未来将扩展呼出面板与插件生态。

mod adapters;
mod cover_disc;
mod mpris;
mod nautilus;
mod page_apps;
mod page_autodark;
mod page_clipboard;
mod page_panel;
mod page_radio;
mod page_settings;
mod radio_api;
mod shell_ext;
mod radio_player;
mod tray;

use adw::prelude::*;
use adw::{Application, ApplicationWindow, NavigationPage, NavigationSplitView};
use gtk::{
    Align, Box as GtkBox, Image, Label, ListBox, ListBoxRow, Orientation,
    ScrolledWindow, Stack, StackTransitionType,
};
use gtk::glib;
use std::cell::RefCell;

const APP_ID: &str = "tools.yihu.desktop";
/// 侧边栏行序 = stack 页序（呼出面板 --page 深链也按这个名字寻址）
const PAGES: &[&str] = &["autodark", "clipboard", "apps", "radio", "panel", "settings", "about"];

thread_local! {
    /// 已构建界面的句柄：命令行转发（--page 深链 / 二次启动置前）需要它。
    /// GTK 主循环单线程，用 thread_local 而非 static。
    static UI: RefCell<Option<(Stack, ApplicationWindow)>> = const { RefCell::new(None) };
}

fn main() -> glib::ExitCode {
    // 全家统一约定：软件渲染压内存
    std::env::set_var("GSK_RENDERER", "cairo");
    let app = Application::builder()
        .application_id(APP_ID)
        .flags(adw::gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    app.add_main_option(
        "page",
        glib::Char::from(b'p'),
        glib::OptionFlags::NONE,
        glib::OptionArg::String,
        "打开指定页面",
        Some("页面名"),
    );
    app.connect_command_line(handle_command_line);
    // 防御：HANDLES_COMMAND_LINE 下正常不会触发 activate，真触发时兜底建 UI
    app.connect_activate(|app| {
        if !ui_built() {
            build_ui(app);
        }
    });
    app.run()
}

fn ui_built() -> bool {
    UI.with_borrow(|ui| ui.is_some())
}

/// 首次调用构建界面；之后转发 --page 深链并把窗口置前。
fn handle_command_line(app: &Application, cmd: &adw::gio::ApplicationCommandLine) -> i32 {
    let page = cmd.options_dict().lookup::<String>("page").ok().flatten();
    if !ui_built() {
        build_ui(app);
    }
    UI.with_borrow(|ui| {
        if let Some((stack, window)) = ui.as_ref() {
            if let Some(p) = page.as_deref() {
                if PAGES.contains(&p) {
                    stack.set_visible_child_name(p);
                }
            }
            window.present();
        }
    });
    0
}

fn build_ui(app: &Application) {
    if let Some(window) = app.windows().first() {
        window.present();
        return;
    }
    load_css();

    // —— 内容侧：各页面 ——
    let stack = Stack::new();
    stack.set_transition_type(StackTransitionType::SlideLeftRight);
    stack.add_named(&page_autodark::build_page(), Some("autodark"));
    stack.add_named(&page_clipboard::build_page(), Some("clipboard"));
    stack.add_named(&page_apps::build_page(), Some("apps"));
    stack.add_named(&page_radio::build_page(), Some("radio"));
    stack.add_named(&page_panel::build_page(), Some("panel"));
    stack.add_named(&page_settings::build_page(), Some("settings"));
    stack.add_named(&about_page(), Some("about"));

    let content = NavigationPage::new(&stack, "一呼");

    // —— 侧边栏 ——
    let sidebar_box = GtkBox::new(Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("一呼", "")));
    sidebar_box.append(&header);

    let list = ListBox::new();
    list.add_css_class("navigation-sidebar");
    let rows = [
        ("autodark", "night-light-symbolic", "主题切换"),
        ("clipboard", "edit-copy-symbolic", "右键菜单"),
        ("apps", "applications-system-symbolic", "应用跟随"),
        ("radio", "applications-multimedia-symbolic", "广播"),
        ("panel", "system-search-symbolic", "呼出面板"),
        ("settings", "emblem-system-symbolic", "设置"),
        ("about", "help-about-symbolic", "关于"),
    ];
    let mut first_row: Option<ListBoxRow> = None;
    for (_name, icon, title) in rows {
        let row_box = GtkBox::new(Orientation::Horizontal, 12);
        let img = Image::from_icon_name(icon);
        let lbl = Label::new(Some(title));
        lbl.set_halign(Align::Start);
        row_box.append(&img);
        row_box.append(&lbl);
        row_box.set_margin_start(6);
        row_box.set_margin_end(6);
        row_box.set_margin_top(4);
        row_box.set_margin_bottom(4);
        let row = ListBoxRow::new();
        row.set_child(Some(&row_box));
        list.append(&row);
        if first_row.is_none() {
            first_row = Some(row);
        }
    }
    sidebar_box.append(&list);
    sidebar_box.set_width_request(200);

    let sidebar = NavigationPage::new(&sidebar_box, "一呼");

    // —— 组合 ——
    let split = NavigationSplitView::new();
    split.set_sidebar(Some(&sidebar));
    split.set_content(Some(&content));

    // 页面切换（侧边栏行序 = stack 页序）
    {
        let stack = stack.clone();
        let names: &[&str] = PAGES;
        list.connect_row_selected(move |_, row| {
            if let Some(row) = row {
                if let Some(name) = names.get(row.index().max(0) as usize) {
                    stack.set_visible_child_name(name);
                }
            }
        });
    }

    let window = ApplicationWindow::builder()
        .application(app)
        .title("一呼 · 工具箱")
        .default_width(900)
        .default_height(640)
        .icon_name(APP_ID)
        .content(&split)
        .build();

    if let Some(row) = first_row {
        list.select_row(Some(&row));
    }
    tray::install(app, &window);
    // 托盘首行「打开广播页」:切到广播页并置前
    {
        let stack = stack.clone();
        let window = window.clone();
        tray::set_open_radio_page_hook(Box::new(move || {
            stack.set_visible_child_name("radio");
            window.present();
        }));
    }
    UI.with_borrow_mut(|ui| *ui = Some((stack.clone(), window.clone())));
    window.present();
}

fn about_page() -> gtk::Widget {
    let card = GtkBox::new(Orientation::Vertical, 8);
    card.add_css_class("card");
    card.add_css_class("card-pad");
    let title = Label::new(Some("一呼 · Yihu"));
    title.add_css_class("title-2");
    card.append(&title);
    for line in [
        "杂而全的 Ubuntu 桌面工具箱 · Rust + GTK4/libadwaita",
        "关闭窗口可驻留系统托盘；托盘菜单可重新打开或退出中心",
        "",
        "执行端架构：autodark-agent 由 systemd 用户定时器按需拉起；",
        "「复制绝对路径」为 Nautilus 扩展，点击时才执行。",
    ] {
        let l = Label::new(Some(line));
        l.add_css_class("dim-label");
        l.set_halign(Align::Start);
        l.set_wrap(true);
        card.append(&l);
    }
    scroll_clamp(&card, 720)
}

// 各页面共用的「滚动 + 居中限宽」容器
pub fn scroll_clamp(content: &impl IsA<gtk::Widget>, max: i32) -> gtk::Widget {
    let clamp = adw::Clamp::builder().maximum_size(max).build();
    clamp.set_child(Some(content));
    let scroll = ScrolledWindow::new();
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.set_child(Some(&clamp));
    scroll.set_vexpand(true);
    scroll.upcast()
}

// 页面外壳：顶部标题栏 + 内容
pub fn page_shell(title: &str, content: &impl IsA<gtk::Widget>) -> gtk::Widget {
    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(title, "")));
    view.add_top_bar(&header);
    view.set_content(Some(content));
    view.upcast()
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
