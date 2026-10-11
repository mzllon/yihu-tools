//! Markdown 工作台（M6）：Typora 形态第一步。
//!
//! 分栏 = 左 GtkTextView 编辑 + 右 WebKitGTK 6.0 实时预览（防抖 300ms），
//! 文件夹级文件管理（FileDialog 选目录 → 下拉切换 *.md）、保存/另存、
//! 导出 docx（pandoc，probe + apt 提示）、浏览器打印出 PDF。
//!
//! 为什么独立进程：WebKitGTK 6.0 走 gtk4 0.10+，与面板（gtk4 0.9）因
//! `links = "gtk-4"` 不能共存于一张依赖图——进程级外置（同中心应用惯例）。
//! 面板宫格/搜索「MD 工作台」拉起本进程，可多开。

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::{
    Application, ApplicationWindow, Button, DropDown, Label, Orientation, Paned, ScrolledWindow,
    StringList, TextView, WrapMode,
};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use webkit6::prelude::*;
use webkit6::WebView;

const APP_ID: &str = "tools.yihu.mdworkbench";
/// 编辑→预览防抖
const RENDER_DEBOUNCE_MS: u64 = 300;

struct State {
    buffer: gtk::TextBuffer,
    view: WebView,
    dropdown: DropDown,
    title: Label,
    window: ApplicationWindow,
    current: RefCell<Option<PathBuf>>,
    files: RefCell<Vec<PathBuf>>,
    dirty: Cell<bool>,
    debounce: RefCell<Option<glib::SourceId>>,
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

fn build_ui(app: &Application, initial: Option<String>) {
    let win = ApplicationWindow::builder()
        .application(app)
        .title("Markdown 工作台")
        .default_width(1280)
        .default_height(820)
        .build();

    let root = gtk::Box::new(Orientation::Vertical, 6);
    root.set_margin_top(8);
    root.set_margin_bottom(8);
    root.set_margin_start(8);
    root.set_margin_end(8);

    // —— 顶栏：文件操作 + 文件切换 + 导出 ——
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

    // —— 分栏：编辑 | 预览 ——
    let paned = Paned::new(Orientation::Horizontal);
    paned.set_position(620);
    paned.set_vexpand(true);

    let editor = TextView::new();
    editor.set_wrap_mode(WrapMode::Word);
    editor.add_css_class("monospace");
    editor.set_top_margin(8);
    editor.set_bottom_margin(8);
    editor.set_left_margin(8);
    editor.set_right_margin(8);
    let left = ScrolledWindow::new();
    left.set_child(Some(&editor));
    left.set_hexpand(true);
    left.set_vexpand(true);
    paned.set_start_child(Some(&left));

    let view = WebView::new();
    let right = ScrolledWindow::new();
    right.set_child(Some(&view));
    right.set_hexpand(true);
    right.set_vexpand(true);
    paned.set_end_child(Some(&right));
    root.append(&paned);

    let title = Label::new(Some("未命名"));
    title.add_css_class("dim-label");
    root.append(&title);

    win.set_child(Some(&root));

    let st = Rc::new(State {
        buffer: editor.buffer(),
        view,
        dropdown,
        title,
        window: win.clone(),
        current: RefCell::new(None),
        files: RefCell::new(Vec::new()),
        dirty: Cell::new(false),
        debounce: RefCell::new(None),
    });

    // 初始渲染占位
    render_now(&st);

    // 编辑 → dirty + 防抖渲染
    {
        let st = st.clone();
        let buf = st.buffer.clone();
        buf.connect_changed(move |_| {
            st.dirty.set(true);
            update_title(&st);
            if let Some(id) = st.debounce.borrow_mut().take() {
                id.remove();
            }
            let st2 = st.clone();
            let id = glib::timeout_add_local(std::time::Duration::from_millis(RENDER_DEBOUNCE_MS), move || {
                render_now(&st2);
                glib::ControlFlow::Break
            });
            *st.debounce.borrow_mut() = Some(id);
        });
    }

    // 打开文件夹：FileDialog 选目录 → 列 *.md → 下拉切换
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
    // 打开单文件
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
    // 新建
    {
        let st = st.clone();
        btn_new.connect_clicked(move |_| {
            st.buffer.set_text("");
            *st.current.borrow_mut() = None;
            st.dirty.set(false);
            update_title(&st);
        });
    }
    // 保存 / 另存为
    {
        let st = st.clone();
        btn_save.connect_clicked(move |_| save_current(&st));
    }
    {
        let st = st.clone();
        let win2 = win.clone();
        btn_saveas.connect_clicked(move |_| {
            let st2 = st.clone();
            save_dialog(st2, &win2, move |st3, path| {
                write_file(&st3, &path);
            });
        });
    }
    // 导出 docx（pandoc）
    {
        let st = st.clone();
        let win2 = win.clone();
        btn_docx.connect_clicked(move |_| export_docx(&st, &win2));
    }
    // 导出 PDF：浏览器打开预览页，Ctrl+P 打印为 PDF
    {
        let st = st.clone();
        btn_pdf.connect_clicked(move |_| {
            let html = yihu_core::mdrender::render_html(&buffer_text(&st));
            match yihu_core::mdrender::write_preview(&html) {
                Ok(path) => {
                    let _ = std::process::Command::new("gio")
                        .args(["open", &path.display().to_string()])
                        .spawn();
                    notify("已在浏览器打开（Ctrl+P 可打印/另存为 PDF）");
                }
                Err(e) => notify(&format!("导出失败：{e}")),
            }
            let _ = &st;
        });
    }
    // 下拉切换文件
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

    // 初始文件（argv）
    if let Some(f) = initial {
        let path = PathBuf::from(f);
        if path.is_file() {
            if let Some(dir) = path.parent() {
                open_folder(&st, dir);
            }
            load_file(&st, &path);
        }
    }

    win.present();
}

fn buffer_text(st: &State) -> String {
    let (s, e) = st.buffer.bounds();
    st.buffer.text(&s, &e, false).to_string()
}

fn update_title(st: &State) {
    let name = st
        .current
        .borrow()
        .as_ref()
        .map(|p| p.file_name().unwrap_or_default().to_string_lossy().to_string())
        .unwrap_or_else(|| "未命名".into());
    let dirty = if st.dirty.get() { " *" } else { "" };
    st.title.set_text(&format!("{name}{dirty}"));
    st.window.set_title(Some(&format!("Markdown 工作台 — {name}{dirty}")));
}

fn render_now(st: &State) {
    let html = yihu_core::mdrender::render_html(&buffer_text(st));
    st.view.load_html(&html, None);
}

fn open_folder(st: &State, dir: &std::path::Path) {
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
        .map(|p| {
            p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        })
        .collect();
    let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    st.dropdown.set_model(Some(&StringList::new(&refs)));
    if !names.is_empty() {
        st.dropdown.set_selected(0); // 触发 selected_notify → load_file
    }
}

fn load_file(st: &State, path: &std::path::Path) {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            st.buffer.set_text(&text);
            *st.current.borrow_mut() = Some(path.to_path_buf());
            st.dirty.set(false);
            update_title(st);
        }
        Err(e) => notify(&format!("打开失败：{e}")),
    }
}

fn save_current(st: &State) {
    match st.current.borrow().clone() {
        Some(path) => write_file(st, &path),
        None => {
            let st = Rc::new(State {
                buffer: st.buffer.clone(),
                view: st.view.clone(),
                dropdown: st.dropdown.clone(),
                title: st.title.clone(),
                window: st.window.clone(),
                current: RefCell::new(st.current.borrow().clone()),
                files: RefCell::new(st.files.borrow().clone()),
                dirty: Cell::new(st.dirty.get()),
                debounce: RefCell::new(None),
            });
            let win2 = st.window.clone();
            save_dialog(st, &win2, |st3, path| write_file(&st3, &path));
        }
    }
}

fn save_dialog<F>(st: Rc<State>, win: &ApplicationWindow, cont: F)
where
    F: FnOnce(Rc<State>, PathBuf) + 'static,
{
    let dialog = gtk::FileDialog::new();
    let stem = st
        .current
        .borrow()
        .as_ref()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().to_string()))
        .unwrap_or_else(|| "未命名".into());
    let initial = format!("{stem}.md");
    dialog.set_initial_name(Some(&initial));
    let win2 = win.clone();
    dialog.save(Some(win), gio::Cancellable::NONE, move |res| {
        if let Ok(file) = res {
            if let Some(path) = file.path() {
                cont(st, path);
            }
        }
        let _ = win2;
    });
}

fn write_file(st: &State, path: &std::path::Path) {
    let text = buffer_text(st);
    match std::fs::write(path, text.as_bytes()) {
        Ok(()) => {
            *st.current.borrow_mut() = Some(path.to_path_buf());
            st.dirty.set(false);
            update_title(st);
            notify(&format!("已保存：{}", path.display()));
        }
        Err(e) => notify(&format!("保存失败：{e}")),
    }
}

fn export_docx(st: &Rc<State>, win: &ApplicationWindow) {
    if !std::path::Path::new("/usr/bin/pandoc").exists() {
        notify("未安装 pandoc（sudo apt install pandoc）");
        return;
    }
    // 先落一个临时 md（当前编辑内容，无论是否保存过）
    let src = std::env::temp_dir().join(format!(
        "yihu-md-export-{}.md",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    if let Err(e) = std::fs::write(&src, buffer_text(st).as_bytes()) {
        notify(&format!("写临时文件失败：{e}"));
        return;
    }
    let dialog = gtk::FileDialog::new();
    let stem = st
        .current
        .borrow()
        .as_ref()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().to_string()))
        .unwrap_or_else(|| "文档".into());
    let initial = format!("{stem}.docx");
    dialog.set_initial_name(Some(&initial));
    dialog.save(Some(win), gio::Cancellable::NONE, move |res| {
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
}

fn notify(body: &str) {
    let _ = std::process::Command::new("notify-send")
        .args(["--app-name=一呼", body])
        .spawn();
}
