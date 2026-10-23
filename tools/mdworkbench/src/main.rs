//! Markdown 工作台 v2（M6）：**所见即所得编辑**（Typora 形态核心）。
//!
//! WebKitGTK 内嵌 TOAST UI Editor（开源 MIT，UMD 全量包随二进制内嵌发行）：
//! WYSIWYG / Markdown 双模式（编辑器内置切换）、表格/任务列表/GFM。
//! JS⇄Rust 桥：user content manager 脚本消息（changed/ready 节流回传）+
//! evaluate_javascript（getMarkdown 保存 / setMarkdown 载入）。
//! 文件管理保留 v1：文件夹 *.md 下拉切换、保存/另存（脏标记进标题）、
//! 导出 docx（pandoc，probe + apt 提示）、PDF = WebKit 打印对话框（本批
//! 新增，选「打印到文件」输出 PDF）。
//!
//! 独立进程原因同 v1：WebKitGTK 6.0 需 gtk4 0.10+，与面板 gtk4 0.9 因
//! `links = "gtk-4"` 不能同图（版本组合对齐 uic-poc）。

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::{
    Application, ApplicationWindow, Button, DropDown, Label, Orientation, ScrolledWindow,
    StringList,
};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use webkit6::prelude::*;
use webkit6::{PrintOperation, UserContentManager, WebView};

const APP_ID: &str = "tools.yihu.mdworkbench";
// 主编辑器 = Vditor IR（Typora 形态内核，BUG-013 方向 B）：index.min.js +
// 懒加载子集（lute 必需、highlight/mermaid/i18n/内容主题按需）。cdn 指向
// 落盘目录绝对地址，懒加载路径 ${cdn}/dist/... 必须保持此布局。
const VDITOR_JS: &[u8] = include_bytes!("../assets/vditor/index.min.js");
const VDITOR_CSS: &[u8] = include_bytes!("../assets/vditor/index.css");
const LUTE_JS: &[u8] = include_bytes!("../assets/dist/js/lute/lute.min.js");
const HLJS_JS: &[u8] = include_bytes!("../assets/dist/js/highlight.js/highlight.min.js");
const HLJS_GITHUB_CSS: &[u8] =
    include_bytes!("../assets/dist/js/highlight.js/styles/github.min.css");
const HLJS_GITHUB_DARK_CSS: &[u8] =
    include_bytes!("../assets/dist/js/highlight.js/styles/github-dark.min.css");
const MERMAID_JS: &[u8] = include_bytes!("../assets/dist/js/mermaid/mermaid.min.js");
const I18N_ZH_CN: &[u8] = include_bytes!("../assets/dist/js/i18n/zh_CN.js");
const ICONS_ANT_JS: &[u8] = include_bytes!("../assets/dist/js/icons/ant.js");
const CONTENT_LIGHT_CSS: &[u8] = include_bytes!("../assets/dist/css/content-theme/light.css");
const CONTENT_DARK_CSS: &[u8] = include_bytes!("../assets/dist/css/content-theme/dark.css");
// toastui 三件仅剩便签窗 sticky.html 在用
const EDITOR_JS: &[u8] = include_bytes!("../assets/toastui-editor-all.min.js");
const EDITOR_CSS: &[u8] = include_bytes!("../assets/toastui-editor.min.css");
const DARK_CSS: &[u8] = include_bytes!("../assets/toastui-editor-dark.css");
const STICKY_HTML: &str = include_str!("../assets/sticky.html");
const INDEX_HTML: &str = include_str!("../assets/index.html");

const WELCOME: &str = "# 欢迎使用 Markdown 工作台\n\n直接开始输入 —— 即时渲染（Typora 手感）。\n\n- Ctrl+/ 切源码；**Ctrl+T** 插表格；Ctrl+Shift+K 代码块\n- Ctrl+Shift+Q 引用；Ctrl+Shift+X 任务列表\n- 表格手打也行：表头回车后补一行 `| --- | --- |` 即成形\n- 顶部按钮：打开文件夹、保存、导出\n";

struct State {
    window: ApplicationWindow,
    dropdown: DropDown,
    title: Label,
    view: WebView,
    current: RefCell<Option<PathBuf>>,
    files: RefCell<Vec<PathBuf>>,
    dirty: Cell<bool>,
    /// 页面就绪后要灌入的初稿（ready 前 setMarkdown 会丢）
    pending_md: RefCell<Option<String>>,
    ready: Cell<bool>,
    /// 大纲（JS change 事件回传；ListBox 重建用）
    outline: RefCell<Vec<(u8, String)>>,
    outline_list: gtk::ListBox,
    /// 当前 md 文件目录（图片相对路径显示解析）
    current_dir: RefCell<String>,
    search_entry: gtk::Entry,
    replace_entry: gtk::Entry,
    search_info: gtk::Label,
}

fn main() {
    // 唯一实例：后续拉起（面板/便签能力/预览能力）经 GApplication 把参数
    // 转发到主进程的 ::command-line，由 dispatch 开窗——一个 WebKit 进程
    // 承载编辑窗/预览窗/全部便签窗。
    let app = Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    app.connect_command_line(move |app, cmd| {
        let args: Vec<String> = cmd
            .arguments()
            .iter()
            .map(|s| s.to_string_lossy().to_string())
            .skip(1)
            .collect();
        dispatch(app, &args);
        0
    });
    app.run();
}

/// 命令行分发（主实例）：--sticky / --sticky-close / --view-html / 编辑窗
fn dispatch(app: &Application, args: &[String]) {
    match args.first().map(|s| s.as_str()) {
        Some("--sticky") => {
            if let Some(p) = args.get(1) {
                open_sticky(app, PathBuf::from(p));
            }
        }
        Some("--sticky-close") => {
            if let Some(p) = args.get(1) {
                close_sticky(&PathBuf::from(p));
            }
        }
        Some("--view-html") => {
            if let Some(p) = args.get(1) {
                build_view_ui(app, Some(p.clone()));
            }
        }
        // 面板激活动作传 --open（native 约定，同 radio）：当无参开主窗
        _ => build_ui(app, args.first().filter(|a: &&String| a.as_str() != "--open").cloned()),
    }
}

// ---- 便签窗（Markdown 快贴，自动存回插件数据目录） ------------------------

thread_local! {
    static STICKIES: RefCell<Vec<(PathBuf, Rc<StickyWin>)>> = const { RefCell::new(Vec::new()) };
}

struct StickyWin {
    win: gtk::Window,
}

/// 打开（或置前）一张便签：id 寻址文件，内容随编辑自动存回
fn open_sticky(app: &Application, path: PathBuf) {
    let existing = STICKIES.with(|s| {
        s.borrow()
            .iter()
            .find(|(p, _)| *p == path)
            .map(|(_, w)| w.clone())
    });
    if let Some(w) = existing {
        w.win.present();
        return;
    }
    let content = std::fs::read_to_string(&path).unwrap_or_default();

    let win = gtk::Window::new();
    win.set_decorated(false);
    win.set_resizable(true);
    win.set_default_size(300, 340);
    win.set_title(Some("便签"));
    // 必须挂到 Application：未关联窗口不占用 use count，
    // GApplication 会在 use count 归零时直接退出（进程秒退的根因）
    win.set_application(Some(app));

    let root = gtk::Box::new(Orientation::Vertical, 0);

    // 拖动把手 + 关闭
    let handle = gtk::Box::new(Orientation::Horizontal, 4);
    let grip = Label::new(Some("≡"));
    grip.set_halign(gtk::Align::Start);
    grip.set_margin_start(8);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let title_lbl = Label::new(Some(&stem));
    title_lbl.set_hexpand(true);
    title_lbl.set_halign(gtk::Align::Start);
    handle.append(&grip);
    handle.append(&title_lbl);
    let close_btn = Button::with_label("✕");
    close_btn.add_css_class("flat");
    handle.append(&close_btn);
    handle.add_css_class("toolbar");
    root.append(&handle);

    let ucm = UserContentManager::new();
    let view = WebView::builder().user_content_manager(&ucm).build();
    view.set_vexpand(true);
    view.set_hexpand(true);
    root.append(&view);

    win.set_child(Some(&root));

    let st = Rc::new(StickyWin { win: win.clone() });

    // 拖动 = 原生移动（按住把手任意处拖）
    {
        let w2 = win.clone();
        let click = gtk::GestureClick::new();
        click.set_button(1);
        click.connect_pressed(move |_g, _n, x, y| {
            let seat = gtk::prelude::RootExt::display(&w2).default_seat();
            if let (Some(surface), Some(device)) =
                (w2.surface(), seat.and_then(|s| s.pointer()))
            {
                use gtk::glib::translate::{FromGlibPtrBorrow, ToGlibPtr};
                let raw = <gtk::gdk::Surface as gtk::glib::translate::ToGlibPtr<
                    *const gtk::gdk::ffi::GdkSurface,
                >>::to_glib_none(&surface)
                .0 as *mut gtk::gdk::ffi::GdkToplevel;
                let toplevel = unsafe { gtk::gdk::Toplevel::from_glib_borrow(raw) };
                toplevel.begin_move(&device, 1, x, y, gtk::gdk::CURRENT_TIME);
            }
        });
        handle.add_controller(click);
    }
    let st_close = st.clone();
    close_btn.connect_clicked(move |_| st_close.win.close());
    {
        let path2 = path.clone();
        win.connect_close_request(move |_w| {
            STICKIES.with(|s| s.borrow_mut().retain(|(p, _)| *p != path2));
            glib::Propagation::Proceed
        });
    }

    // 编辑 → 自动存回（防抖 500ms）
    let ucm2 = ucm.clone();
    {
        let path2 = path.clone();
        ucm2.register_script_message_handler("yihu", None);
        let save_timer: std::rc::Rc<RefCell<Option<glib::SourceId>>> =
            std::rc::Rc::new(RefCell::new(None));
        ucm2.connect_script_message_received(Some("yihu"), move |_ucm, value| {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&value.to_str()) else {
                return;
            };
            match v.get("type").and_then(|t| t.as_str()) {
                Some("changed") => {
                    let path3 = path2.clone();
                    let timer_cell = save_timer.clone();
                    if let Some(id) = timer_cell.borrow_mut().take() {
                        id.remove();
                    }
                    let view2 = view.clone();
                    let id = glib::timeout_add_local(
                        std::time::Duration::from_millis(500),
                        move || {
                            let path4 = path3.clone();
                            view2.evaluate_javascript(
                                "window.yihuGetMd()",
                                None,
                                None,
                                Option::<&gio::Cancellable>::None,
                                move |res| {
                                    if let Ok(v) = res {
                                        let md = v.to_str();
                                        let _ = std::fs::write(&path4, md.as_bytes());
                                    }
                                },
                            );
                            glib::ControlFlow::Break
                        },
                    );
                    *timer_cell.borrow_mut() = Some(id);
                }
                Some("ready") => {
                    let text = std::fs::read_to_string(&path2).unwrap_or_default();
                    let literal =
                        serde_json::to_string(&text).unwrap_or_else(|_| "\"\"".into());
                    let script = format!("window.yihuSetMd({literal});");
                    view.evaluate_javascript(
                        &script,
                        None,
                        None,
                        Option::<&gio::Cancellable>::None,
                        |_| {},
                    );
                }
                _ => {}
            }
        });
    }

    STICKIES.with(|s| s.borrow_mut().push((path, st)));
    win.present();
}

fn close_sticky(path: &std::path::Path) {
    STICKIES.with(|s| {
        let hit = s
            .borrow()
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, w)| w.clone());
        if let Some(w) = hit {
            w.win.close();
            s.borrow_mut().retain(|(p, _)| p != path);
        }
    });
}

/// 只读预览窗：消毒 HTML 直载，无编辑器无桥（md.view 能力的宿主侧渲染面）
fn build_view_ui(app: &Application, html: Option<String>) {
    let win = ApplicationWindow::builder()
        .application(app)
        .title("Markdown 预览")
        .default_width(1000)
        .default_height(760)
        .build();
    let view = WebView::new();
    // JS console 直落 stdout（YIHU_MD_DEBUG=1 时启用）：编辑器页初始化故障的现场
    if std::env::var_os("YIHU_MD_DEBUG").is_some() {
        if let Some(s) = webkit6::prelude::WebViewExt::settings(&view) {
            s.set_enable_write_console_messages_to_stdout(true);
        }
    }
    view.set_vexpand(true);
    view.set_hexpand(true);
    win.set_child(Some(&view));
    if let Some(p) = html {
        view.load_uri(&format!("file://{p}"));
    }
    win.present();
}

/// 资产落盘（幂等）：编辑器页 = include 内嵌，部署只带一个二进制
fn install_assets() -> Result<PathBuf, String> {
    let dir = glib::user_data_dir().join("yihu").join("md-editor");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建资产目录失败：{e}"))?;
    for (name, bytes) in [
        ("vditor/index.min.js", VDITOR_JS),
        ("vditor/index.css", VDITOR_CSS),
        ("dist/js/lute/lute.min.js", LUTE_JS),
        ("dist/js/highlight.js/highlight.min.js", HLJS_JS),
        (
            "dist/js/highlight.js/styles/github.min.css",
            HLJS_GITHUB_CSS,
        ),
        (
            "dist/js/highlight.js/styles/github-dark.min.css",
            HLJS_GITHUB_DARK_CSS,
        ),
        ("dist/js/mermaid/mermaid.min.js", MERMAID_JS),
        ("dist/js/i18n/zh_CN.js", I18N_ZH_CN),
        ("dist/js/icons/ant.js", ICONS_ANT_JS),
        ("dist/css/content-theme/light.css", CONTENT_LIGHT_CSS),
        ("dist/css/content-theme/dark.css", CONTENT_DARK_CSS),
        // toastui 三件：便签窗 sticky.html 专用
        ("toastui-editor-all.min.js", EDITOR_JS),
        ("toastui-editor.min.css", EDITOR_CSS),
        ("toastui-editor-dark.css", DARK_CSS),
        ("sticky.html", STICKY_HTML.as_bytes()),
    ] {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("创建资产目录失败：{e}"))?;
        }
        let same = std::fs::read(&path).map(|old| old == bytes).unwrap_or(false);
        if !same {
            std::fs::write(&path, bytes).map_err(|e| format!("写资产失败：{e}"))?;
        }
    }
    let html = dir.join("index.html");
    std::fs::write(&html, INDEX_HTML).map_err(|e| format!("写编辑器页失败：{e}"))?;
    Ok(html)
}

fn build_ui(app: &Application, initial: Option<String>) {
    let win = ApplicationWindow::builder()
        .application(app)
        .title("Markdown 工作台")
        .default_width(1240)
        .default_height(820)
        .build();

    let root = gtk::Box::new(Orientation::Vertical, 6);
    root.set_margin_top(8);
    root.set_margin_bottom(8);
    root.set_margin_start(8);
    root.set_margin_end(8);

    // —— 顶栏 ——
    let bar = gtk::Box::new(Orientation::Horizontal, 6);
    let mk = |label: &str| {
        let b = Button::with_label(label);
        bar.append(&b);
        b
    };
    let btn_folder = mk("📂 打开文件夹");
    let btn_open = mk("📄 打开文件");
    let btn_new = mk("➕ 新建");
    let btn_save = mk("💾 保存");
    let btn_saveas = mk("另存为");
    bar.append(&Label::new(Some("│")));
    let dropdown = DropDown::new(
        Some(StringList::new(&["（未打开文件夹）"])),
        None::<gtk::Expression>,
    );
    dropdown.set_enable_search(true);
    dropdown.set_hexpand(true);
    bar.append(&dropdown);
    bar.append(&Label::new(Some("│")));
    let btn_outline = mk("大纲");
    let btn_search = mk("🔍");
    let btn_docx = mk("docx");
    let btn_html = mk("HTML");
    let btn_pdf = mk("PDF");
    bar.append(&Label::new(Some("│")));
    let theme_dd = DropDown::from_strings(&["主题：跟随系统", "主题：浅色", "主题：深色"]);
    bar.append(&theme_dd);
    root.append(&bar);

    // —— 查找替换条（默认隐藏）——
    let search_row = gtk::Box::new(Orientation::Horizontal, 6);
    let search_entry = gtk::Entry::new();
    search_entry.set_placeholder_text(Some("查找"));
    let replace_entry = gtk::Entry::new();
    replace_entry.set_placeholder_text(Some("替换为"));
    let btn_find = Button::with_label("下一个");
    let btn_replall = Button::with_label("全部替换");
    let search_info = Label::new(Some(""));
    search_row.append(&search_entry);
    search_row.append(&replace_entry);
    search_row.append(&btn_find);
    search_row.append(&btn_replall);
    search_row.append(&search_info);
    search_row.set_visible(false);
    root.append(&search_row);

    // —— 编辑器（左：大纲侧栏；右：WebView；UCM 为 construct-only，须随构造挂载）——
    let ucm = UserContentManager::new();
    let view = WebView::builder()
        .user_content_manager(&ucm)
        .build();
    view.set_vexpand(true);
    view.set_hexpand(true);

    let outline_list = gtk::ListBox::new();
    outline_list.add_css_class("navigation-sidebar");
    let outline_scroll = ScrolledWindow::new();
    outline_scroll.set_child(Some(&outline_list));
    outline_scroll.set_width_request(190);
    outline_scroll.set_vexpand(true);
    outline_scroll.set_visible(false);

    let body = gtk::Box::new(Orientation::Horizontal, 0);
    body.append(&outline_scroll);
    body.append(&view);
    body.set_vexpand(true);
    body.set_hexpand(true);
    root.append(&body);

    let title = Label::new(Some("未命名"));
    title.add_css_class("dim-label");
    root.append(&title);

    win.set_child(Some(&root));

    let st = Rc::new(State {
        window: win.clone(),
        dropdown,
        title,
        view: view.clone(),
        current: RefCell::new(None),
        files: RefCell::new(Vec::new()),
        dirty: Cell::new(false),
        pending_md: RefCell::new(Some(WELCOME.into())),
        ready: Cell::new(false),
        outline: RefCell::new(Vec::new()),
        outline_list: outline_list.clone(),
        current_dir: RefCell::new(String::new()),
        search_entry: search_entry.clone(),
        replace_entry: replace_entry.clone(),
        search_info: search_info.clone(),
    });

    // —— JS⇄Rust 桥（load_uri 前接好信号）——
    ucm.register_script_message_handler("yihu", None);
    {
        let st = st.clone();
        ucm.connect_script_message_received(Some("yihu"), move |_ucm, value| {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&value.to_str()) else {
                return;
            };
            match v.get("type").and_then(|t| t.as_str()) {
                Some("changed") => {
                    st.dirty.set(true);
                    update_title(&st);
                }
                Some("ready") => {
                    st.ready.set(true);
                    if let Some(md) = st.pending_md.borrow_mut().take() {
                        set_md(&st, &md);
                    }
                }
                Some("jslog") => {
                    // 编辑器页诊断（YIHU_MD_DEBUG=1 时可见，排查通道）
                    if std::env::var_os("YIHU_MD_DEBUG").is_some() {
                        eprintln!(
                            "md-js: {}",
                            v.get("text").and_then(|x| x.as_str()).unwrap_or("")
                        );
                    }
                }
                Some("outline") => {
                    let items: Vec<(u8, String)> = v
                        .get("items")
                        .and_then(|a| a.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|it| {
                                    let lv = it.get(0)?.as_u64()? as u8;
                                    let txt = it.get(1)?.as_str()?.to_string();
                                    Some((lv.min(6), txt))
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    *st.outline.borrow_mut() = items.clone();
                    while let Some(row) = st.outline_list.first_child() {
                        st.outline_list.remove(&row);
                    }
                    for (lv, txt) in &items {
                        let lbl = gtk::Label::new(Some(txt));
                        lbl.set_xalign(0.0);
                        lbl.set_margin_start((lv - 1) as i32 * 12);
                        lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
                        lbl.set_width_chars(20);
                        let row = gtk::ListBoxRow::new();
                        row.set_child(Some(&lbl));
                        st.outline_list.append(&row);
                    }
                }
                Some("image") => {
                    // 粘贴图片自动入档：dataURL → base64 → 当前文件旁 images/
                    let id = v.get("id").and_then(|x| x.as_u64()).unwrap_or(0);
                    let name = v
                        .get("name")
                        .and_then(|x| x.as_str())
                        .unwrap_or("image.png")
                        .to_string();
                    let data = v
                        .get("data")
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string();
                    match save_pasted_image(&st, &name, &data) {
                        Ok(rel) => {
                            let script = format!(
                                "window.yihuImageSaved({id}, {});",
                                serde_json::to_string(&rel).unwrap_or_default()
                            );
                            st.view.evaluate_javascript(
                                &script,
                                None,
                                None,
                                Option::<&gio::Cancellable>::None,
                                |_| {},
                            );
                        }
                        Err(e) => notify(&format!("图片保存失败：{e}")),
                    }
                }
                _ => {}
            }
        });
    }
    // 资产 + 加载
    match install_assets() {
        Ok(html) => view.load_uri(&format!("file://{}", html.display())),
        Err(e) => {
            notify(&format!("编辑器资产部署失败：{e}"));
        }
    }
    // 页面加载完成兜底：ready 消息若未送达也灌初稿
    {
        let st = st.clone();
        view.connect_load_changed(move |_v, event| {
            if matches!(event, webkit6::LoadEvent::Finished)
                && !st.ready.get()
                && st.pending_md.borrow().is_some()
            {
                st.ready.set(true);
                if let Some(md) = st.pending_md.borrow_mut().take() {
                    set_md(&st, &md);
                }
            }
        });
    }

    // —— 文件操作 ——
    {
        let st = st.clone();
        let win2 = win.clone();
        btn_folder.connect_clicked(move |_| {
            let dialog = gtk::FileDialog::new();
            dialog.set_title("打开 Markdown 文件夹");
            let st2 = st.clone();
            dialog.select_folder(Some(&win2), gio::Cancellable::NONE, move |res| {
                if let Ok(file) = res {
                    if let Some(path) = file.path() {
                        open_folder(&st2, &path);
                    }
                }
            });
        });
    }
    {
        let st = st.clone();
        let win2 = win.clone();
        btn_open.connect_clicked(move |_| {
            let dialog = gtk::FileDialog::new();
            dialog.set_title("打开 Markdown 文件");
            let f = gtk::FileFilter::new();
            f.set_name(Some("Markdown"));
            f.add_pattern("*.md");
            dialog.set_default_filter(Some(&f));
            let st2 = st.clone();
            dialog.open(Some(&win2), gio::Cancellable::NONE, move |res| {
                if let Ok(file) = res {
                    if let Some(path) = file.path() {
                        load_file(&st2, &path);
                    }
                }
            });
        });
    }
    {
        let st = st.clone();
        btn_new.connect_clicked(move |_| {
            *st.current.borrow_mut() = None;
            st.dirty.set(false);
            set_md(&st, "");
            update_title(&st);
        });
    }
    {
        let st = st.clone();
        btn_save.connect_clicked(move |_| save_current(&st));
    }
    {
        let st = st.clone();
        let win2 = win.clone();
        btn_saveas.connect_clicked(move |_| {
            let st2 = st.clone();
            save_dialog(st2, &win2, |st3, path| {
                get_md(&st3, move |st4, md| write_file(&st4, &path, &md));
            });
        });
    }
    {
        let st = st.clone();
        let win2 = win.clone();
        btn_docx.connect_clicked(move |_| export_docx(&st, &win2));
    }
    // 大纲侧栏开关
    {
        let outline_scroll2 = outline_scroll.clone();
        btn_outline.connect_clicked(move |_| {
            outline_scroll2.set_visible(!outline_scroll2.is_visible());
        });
    }
    // 查找替换条开关 + Ctrl+F
    {
        let search_row2 = search_row.clone();
        let entry2 = search_entry.clone();
        btn_search.connect_clicked(move |_| {
            search_row2.set_visible(!search_row2.is_visible());
            if search_row2.is_visible() {
                entry2.grab_focus();
            }
        });
        let key = gtk::EventControllerKey::new();
        {
            let search_row2 = search_row.clone();
            let entry = search_entry.clone();
            key.connect_key_pressed(move |_k, key, _c, m| {
                if m.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                    && key.name().unwrap_or_default() == "f"
                {
                    search_row2.set_visible(true);
                    entry.grab_focus();
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
        }
        win.add_controller(key);
    }
    // 查找下一个
    {
        let st = st.clone();
        btn_find.connect_clicked(move |_| {
            let term = st.search_entry.text().to_string();
            if term.is_empty() {
                return;
            }
            let script = format!(
                "window.yihuFindNext({});",
                serde_json::to_string(&term).unwrap_or_default()
            );
            st.view
                .evaluate_javascript(&script, None, None, Option::<&gio::Cancellable>::None, |_| {});
        });
    }
    // 全部替换
    {
        let st = st.clone();
        btn_replall.connect_clicked(move |_| {
            let term = st.search_entry.text().to_string();
            let repl = st.replace_entry.text().to_string();
            if term.is_empty() {
                return;
            }
            let st2 = st.clone();
            let script = format!(
                "window.yihuReplaceAll({}, {});",
                serde_json::to_string(&term).unwrap_or_default(),
                serde_json::to_string(&repl).unwrap_or_default()
            );
            st.view
                .evaluate_javascript(&script, None, None, Option::<&gio::Cancellable>::None,
                    move |res| {
                        if let Ok(v) = res {
                            st2.search_info.set_text(&format!("替换 {} 处", v.to_str()));
                        }
                    });
        });
    }
    // 主题三态
    {
        let st = st.clone();
        let dd2 = theme_dd.clone();
        dd2.connect_selected_notify(move |dd| {
            let m = ["auto", "light", "dark"][dd.selected() as usize];
            let script =
                format!("window.yihuTheme({});", serde_json::to_string(m).unwrap_or_default());
            st.view
                .evaluate_javascript(&script, None, None, Option::<&gio::Cancellable>::None, |_| {});
        });
    }
    // 导出 HTML
    {
        let st = st.clone();
        let win2 = win.clone();
        btn_html.connect_clicked(move |_| export_html(&st));
    }
    {
        let st = st.clone();
        let win2 = win.clone();
        btn_pdf.connect_clicked(move |_| {
            // WebKit 打印对话框：打印机选「打印到文件」即输出 PDF
            let op = PrintOperation::new(&st.view);
            let _ = op.run_dialog(Some(&win2));
        });
    }
    {
        let st = st.clone();
        let dd_widget = st.dropdown.clone();
        dd_widget.connect_selected_notify(move |dd| {
            let files = st.files.borrow();
            let idx = dd.selected() as usize;
            if let Some(path) = files.get(idx) {
                load_file(&st, path);
            }
        });
    }

    // 大纲点击 → 滚动到对应标题
    {
        let st2 = st.clone();
        let list2 = st.outline_list.clone();
        list2.connect_row_activated(move |_list, row| {
            let idx = row.index().max(0) as usize;
            let script = format!("window.yihuScrollHeading({idx});");
            st2.view
                .evaluate_javascript(&script, None, None, Option::<&gio::Cancellable>::None, |_| {});
        });
    }

    if let Some(f) = initial {
        let path = PathBuf::from(f);
        if path.is_file() {
            if let Some(dir) = path.parent() {
                open_folder(&st, &dir.to_path_buf());
            }
            load_file(&st, &path);
        }
    }

    win.present();
}

// ---- JS 桥 -----------------------------------------------------------------

/// 灌入 Markdown（编辑器内容整体替换）
fn set_md(st: &State, md: &str) {
    let literal = serde_json::to_string(md).unwrap_or_else(|_| "\"\"".into());
    let script = format!("window.yihuSetMd({literal});");
    st.view
        .evaluate_javascript(&script, None, None, Option::<&gio::Cancellable>::None, |_| {});
    st.dirty.set(false);
    update_title(st);
}

/// 取当前 Markdown（异步回主线程回调）
fn get_md<F>(st: &Rc<State>, cb: F)
where
    F: FnOnce(Rc<State>, String) + 'static,
{
    let st2 = st.clone();
    st.view.evaluate_javascript(
        "window.yihuGetMd()",
        None,
        None,
        Option::<&gio::Cancellable>::None,
        move |res| {
            let md = match res {
                Ok(v) => {
                    let json = v.to_str();
                    serde_json::from_str::<String>(&json)
                        .unwrap_or_else(|_| json.trim_matches('"').to_string())
                }
                Err(e) => {
                    notify(&format!("读取编辑内容失败：{e}"));
                    return;
                }
            };
            cb(st2, md);
        },
    );
}

// ---- 文件 ------------------------------------------------------------------

fn buffer_current_name(st: &State) -> String {
    st.current
        .borrow()
        .as_ref()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().to_string()))
        .unwrap_or_else(|| "未命名".into())
}

fn update_title(st: &State) {
    let name = buffer_current_name(st);
    let dirty = if st.dirty.get() { " *" } else { "" };
    st.title.set_text(&format!("{name}{dirty}"));
    st.window
        .set_title(Some(&format!("Markdown 工作台 — {name}{dirty}")));
}

fn open_folder(st: &State, dir: &PathBuf) {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "md"))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    *st.files.borrow_mut() = files;
    let names: Vec<String> = st
        .files
        .borrow()
        .iter()
        .map(|p| p.file_name().unwrap_or_default().to_string_lossy().to_string())
        .collect();
    let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    st.dropdown.set_model(Some(&StringList::new(&refs)));
    if !names.is_empty() {
        st.dropdown.set_selected(0);
    }
}

fn load_file(st: &State, path: &std::path::Path) {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            *st.current.borrow_mut() = Some(path.to_path_buf());
            sync_dir(st, path);
            st.dirty.set(false);
            set_md(st, &text);
        }
        Err(e) => notify(&format!("打开失败：{e}")),
    }
}

/// 同步当前文件目录给 JS（相对图片显示解析用）
fn sync_dir(st: &State, path: &std::path::Path) {
    let dir = path
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    *st.current_dir.borrow_mut() = dir.clone();
    let script = format!(
        "window.yihuSetDir({});",
        serde_json::to_string(&dir).unwrap_or_default()
    );
    st.view
        .evaluate_javascript(&script, None, None, Option::<&gio::Cancellable>::None, |_| {});
}

/// 粘贴图片入档：dataURL → base64 → 当前文件旁 images/（无当前文件则应用数据目录）
fn save_pasted_image(st: &State, name: &str, data_url: &str) -> Result<String, String> {
    use base64::Engine as _;
    let (meta, b64) = data_url.split_once(',').ok_or("dataURL 格式非法")?;
    let ext = if meta.contains("jpeg") {
        "jpg"
    } else if meta.contains("gif") {
        "gif"
    } else if meta.contains("webp") {
        "webp"
    } else {
        "png"
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| format!("base64 解码失败：{e}"))?;
    let dir = match st.current.borrow().clone() {
        Some(p) if p.parent().is_some() => p.parent().unwrap().join("images"),
        _ => glib::user_data_dir().join("yihu").join("md-images"),
    };
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建图片目录失败：{e}"))?;
    let safe: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .collect();
    let fname = format!(
        "{}-{}.{ext}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        if safe.is_empty() { "image".to_string() } else { safe }
    );
    let path = dir.join(&fname);
    std::fs::write(&path, &bytes).map_err(|e| format!("写图片失败：{e}"))?;
    Ok(format!("images/{fname}"))
}

/// 导出 HTML 文件（pulldown-cmark 完整页）
fn export_html(st: &Rc<State>) {
    get_md(st, move |st2, md| {
        let html = yihu_core::mdrender::render_html(&md);
        let dialog = gtk::FileDialog::new();
        let initial = format!("{}.html", buffer_current_name(&st2));
        dialog.set_initial_name(Some(&initial));
        dialog.save(Some(&st2.window), gio::Cancellable::NONE, move |res| {
            if let Ok(file) = res {
                if let Some(path) = file.path() {
                    match std::fs::write(&path, html.as_bytes()) {
                        Ok(()) => notify(&format!("已导出 HTML：{}", path.display())),
                        Err(e) => notify(&format!("导出失败：{e}")),
                    }
                }
            }
        });
    });
}

fn save_current(st: &Rc<State>) {
    match st.current.borrow().clone() {
        Some(path) => get_md(st, move |st2, md| write_file(&st2, &path, &md)),
        None => {
            let win2 = st.window.clone();
            save_dialog(st.clone(), &win2, |st3, path| {
                get_md(&st3, move |st4, md| write_file(&st4, &path, &md));
            });
        }
    }
}

fn save_dialog<F>(st: Rc<State>, win: &ApplicationWindow, cont: F)
where
    F: FnOnce(Rc<State>, PathBuf) + 'static,
{
    let dialog = gtk::FileDialog::new();
    let initial = format!("{}.md", buffer_current_name(&st));
    dialog.set_initial_name(Some(&initial));
    dialog.save(Some(win), gio::Cancellable::NONE, move |res| {
        if let Ok(file) = res {
            if let Some(path) = file.path() {
                cont(st, path);
            }
        }
    });
}

fn write_file(st: &State, path: &std::path::Path, md: &str) {
    match std::fs::write(path, md.as_bytes()) {
        Ok(()) => {
            *st.current.borrow_mut() = Some(path.to_path_buf());
            sync_dir(st, path);
            st.dirty.set(false);
            update_title(st);
            notify(&format!("已保存：{}", path.display()));
        }
        Err(e) => notify(&format!("保存失败：{e}")),
    }
}

fn export_docx(st: &Rc<State>, _win: &ApplicationWindow) {
    if !std::path::Path::new("/usr/bin/pandoc").exists() {
        notify(&format!(
            "未安装 pandoc（{}）",
            yihu_core::distro::install_hint(&["pandoc"])
        ));
        return;
    }
    get_md(st, move |st2, md| {
        let src = std::env::temp_dir().join(format!(
            "yihu-md-export-{}.md",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        if let Err(e) = std::fs::write(&src, md.as_bytes()) {
            notify(&format!("写临时文件失败：{e}"));
            return;
        }
        let dialog = gtk::FileDialog::new();
        let initial = format!("{}.docx", buffer_current_name(&st2));
        dialog.set_initial_name(Some(&initial));
        let win3 = st2.window.clone();
        dialog.save(Some(&win3), gio::Cancellable::NONE, move |res| {
            if let Ok(file) = res {
                if let Some(dest) = file.path() {
                    let src2 = src.clone();
                    let dest2 = dest.clone();
                    std::thread::spawn(move || {
                        let out = std::process::Command::new("pandoc")
                            .arg(&src2)
                            .args(["-o"])
                            .arg(&dest2)
                            .output();
                        let msg = match out {
                            Ok(o) if o.status.success() => {
                                format!("已导出 docx：{}", dest2.display())
                            }
                            Ok(o) => format!(
                                "pandoc 失败：{}",
                                String::from_utf8_lossy(&o.stderr).lines().next().unwrap_or("")
                            ),
                            Err(e) => format!("pandoc 执行失败：{e}"),
                        };
                        notify(&msg);
                    });
                }
            }
        });
    });
}

fn notify(body: &str) {
    let _ = std::process::Command::new("notify-send")
        .args(["--app-name=一呼", body])
        .spawn();
}
