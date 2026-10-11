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
    Application, ApplicationWindow, Button, DropDown, Label, Orientation, StringList,
};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use webkit6::prelude::*;
use webkit6::{PrintOperation, UserContentManager, WebView};

const APP_ID: &str = "tools.yihu.mdworkbench";
const EDITOR_JS: &[u8] = include_bytes!("../assets/toastui-editor-all.min.js");
const EDITOR_CSS: &[u8] = include_bytes!("../assets/toastui-editor.min.css");
const DARK_CSS: &[u8] = include_bytes!("../assets/toastui-editor-dark.css");
const INDEX_HTML: &str = include_str!("../assets/index.html");

const WELCOME: &str = "# 欢迎使用 Markdown 工作台\n\n直接开始输入 —— 这是**所见即所得**编辑器。\n\n- 工具栏可切换 Markdown / 所见即所得模式\n- 表格、任务列表、删除线都支持\n- 顶部按钮：打开文件夹、保存、导出\n";

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
}

fn main() {
    let app = Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let file = std::env::args().nth(1);
    app.connect_activate(move |app| build_ui(app, file.clone()));
    app.run();
}

/// 资产落盘（幂等）：编辑器页 = include 内嵌，部署只带一个二进制
fn install_assets() -> Result<PathBuf, String> {
    let dir = glib::user_data_dir().join("yihu").join("md-editor");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建资产目录失败：{e}"))?;
    for (name, bytes) in [
        ("toastui-editor-all.min.js", EDITOR_JS),
        ("toastui-editor.min.css", EDITOR_CSS),
        ("toastui-editor-dark.css", DARK_CSS),
    ] {
        let path = dir.join(name);
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
    let btn_docx = mk("导出 docx");
    let btn_pdf = mk("导出 PDF");
    root.append(&bar);

    // —— 编辑器（全窗 WebView；UCM 为 construct-only，须随构造挂载）——
    let ucm = UserContentManager::new();
    let view = WebView::builder()
        .user_content_manager(&ucm)
        .build();
    view.set_vexpand(true);
    view.set_hexpand(true);
    root.append(&view);

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
    });

    // —— JS⇄Rust 桥（load_uri 前接好信号）——
    ucm.register_script_message_handler("yihu", None);
    {
        let st = st.clone();
        ucm.connect_script_message_received(Some("yihu"), move |_ucm, value| {
            let msg = value.to_str();
            if msg.contains("\"changed\"") {
                st.dirty.set(true);
                update_title(&st);
            } else if msg.contains("\"ready\"") {
                st.ready.set(true);
                if let Some(md) = st.pending_md.borrow_mut().take() {
                    set_md(&st, &md);
                }
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
            st.dirty.set(false);
            set_md(st, &text);
        }
        Err(e) => notify(&format!("打开失败：{e}")),
    }
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
            st.dirty.set(false);
            update_title(st);
            notify(&format!("已保存：{}", path.display()));
        }
        Err(e) => notify(&format!("保存失败：{e}")),
    }
}

fn export_docx(st: &Rc<State>, _win: &ApplicationWindow) {
    if !std::path::Path::new("/usr/bin/pandoc").exists() {
        notify("未安装 pandoc（sudo apt install pandoc）");
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
