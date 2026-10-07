//! UI 插件形态 PoC（M4 二期③）：用实测数据敲定 UI 插件进程形态。
//!
//! 对照两组（M4 计划验收门槛：空载 PSS / 冷启动 / 呼出延迟）：
//! - `uic-poc native`  原生模板：宿主 GTK 渲染声明式行（零额外语义进程）；
//! - `uic-poc web`     受控 WebView：webkitgtk 按需进程（收起杀灭）；
//! - `uic-poc bench`   依次拉起两种模式，汇总输出对比表。
//!
//! 口径说明：主进程 PSS 取 /proc/self/smaps_rollup；webkit 模式额外
//! 汇总 WebKitWebProcess / WebKitNetworkProcess 子进程 PSS（多进程
//! 架构的真实占用 = 两者之和）。map_ms = 进程启动到窗口首帧映射的
//! 墙钟，即「冷启动 + 首帧」的近似。
//!
//! 决策记录（实施前冻结，先立此存照）：跑 `bench` 把三组数字填进
//! docs/插件基座M4计划.md 的「UI 形态 PoC」小节。红线：空载 PSS 与
//! 呼出延迟不得明显劣化面板整体指标；超线直接取原生模板，不折衷。

use gtk::prelude::*;
use gtk::{Align, Box as GtkBox, Image, Label, Orientation, Window};
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn main() {
    std::env::set_var("GSK_RENDERER", "cairo");
    match std::env::args().nth(1).as_deref() {
        Some("native") => run_mode(Mode::Native),
        Some("web") => run_mode(Mode::Web),
        Some("bench") => bench(),
        _ => {
            eprintln!("用法: uic-poc [native|web|bench]");
            std::process::exit(2);
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Native,
    Web,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Native => "native",
            Mode::Web => "web",
        }
    }
}

/// 模拟插件 UI 的 10 行数据（title/subtitle/icon 与 results 协议同构）
fn rows() -> Vec<(&'static str, &'static str, &'static str)> {
    (0..10)
        .map(|i| {
            (
                match i % 3 {
                    0 => "转换时间戳",
                    1 => "生成密码",
                    _ => "翻译选中文本",
                },
                "一呼 · 示例插件",
                "applications-system-symbolic",
            )
        })
        .collect()
}

#[cfg_attr(not(feature = "web"), allow(dead_code))]
fn html_page() -> String {
    let items: String = rows()
        .iter()
        .map(|(t, s, _)| {
            format!(
                "<div class='row'><div><div class='t'>{t}</div><div class='s'>{s}</div></div></div>"
            )
        })
        .collect();
    format!(
        "<html><head><meta charset='utf-8'><style>
        body {{ margin:0; background:#1e1e1e; color:#eee; font-family:sans-serif; }}
        .row {{ padding:8px 14px; border-bottom:1px solid #333; }}
        .t {{ font-size:15px; }} .s {{ font-size:12px; color:#999; }}
        </style></head><body>{items}</body></html>"
    )
}

fn run_mode(mode: Mode) {
    let t0 = Instant::now();
    let app = gtk::Application::builder()
        .application_id("tools.yihu.uic-poc")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_activate(move |app| {
        let win = Window::new();
        win.set_default_size(640, 420);
        match mode {
            Mode::Native => {
                let v = GtkBox::new(Orientation::Vertical, 0);
                for (t, s, icon) in rows() {
                    let row = GtkBox::new(Orientation::Horizontal, 10);
                    row.set_margin_top(8);
                    row.set_margin_bottom(8);
                    row.set_margin_start(14);
                    row.set_margin_end(14);
                    let img = Image::from_icon_name(icon);
                    img.set_valign(Align::Center);
                    row.append(&img);
                    let col = GtkBox::new(Orientation::Vertical, 2);
                    let tl = Label::new(Some(t));
                    tl.set_halign(Align::Start);
                    tl.set_xalign(0.0);
                    let sl = Label::new(Some(s));
                    sl.set_halign(Align::Start);
                    sl.set_xalign(0.0);
                    sl.add_css_class("caption");
                    col.append(&tl);
                    col.append(&sl);
                    row.append(&col);
                    v.append(&row);
                }
                win.set_child(Some(&v));
            }
            Mode::Web => {
                #[cfg(feature = "web")]
                {
                    let view = webkit6::WebView::new();
                    view.load_html(&html_page(), None);
                    win.set_child(Some(&view));
                }
                #[cfg(not(feature = "web"))]
                {
                    let l = Label::new(Some(
                        "web 模式需 --features web 构建（需 libwebkitgtk-6.0-dev）",
                    ));
                    win.set_child(Some(&l));
                    eprintln!("uic-poc: web 模式需 --features web 构建（需 libwebkitgtk-6.0-dev）");
                }
            }
        }
        {
            let mode = mode;
            win.connect_map(move |_| {
                println!("{} map_ms={}", mode.name(), t0.elapsed().as_millis());
            });
        }
        app.add_window(&win); // 裸 Window 不会自动挂到 app，主循环会提前退出
        win.present();
        let mode = mode;
        let app_handle = app.clone();
        gtk::glib::timeout_add_local_once(Duration::from_millis(2000), move || {
            println!("{} main_pss_kb={}", mode.name(), pss_kb(std::path::Path::new("/proc/self")));
            println!(
                "{} webkit_children_pss_kb={}",
                mode.name(),
                webkit_children_pss_kb(std::process::id())
            );
            app_handle.quit();
        });
    });
    app.run_with_args(&[] as &[&str]);
}

/// 依次拉起两种模式，汇总对比
fn bench() {
    let exe = std::env::current_exe().expect("当前可执行文件");
    let mut summary = Vec::new();
    for mode in [Mode::Native, Mode::Web] {
        let out = std::process::Command::new(&exe)
            .arg(mode.name())
            .env_remove("GSK_RENDERER") // webkit 自行决定渲染路径
            .output()
            .expect("拉起子进程");
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let get = |key: &str| -> u64 {
            text.lines()
                .find(|l| l.contains(key))
                .and_then(|l| l.split('=').next_back())
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0)
        };
        let map_ms = get("map_ms=");
        let main_pss = get("main_pss_kb=");
        let web_pss = get("webkit_children_pss_kb=");
        println!(
            "[{:^6}] map_ms={map_ms:<5} main_pss={main_pss:<6} KiB helper_pss={web_pss:<6} KiB total_pss={} KiB",
            mode.name(),
            main_pss + web_pss
        );
        summary.push((mode.name(), map_ms, main_pss + web_pss));
    }
    println!("---- 决策口径 ----");
    println!("红线：空载 PSS 与冷启动/呼出延迟不得明显劣化面板整体指标（待命 PSS ~20MB、暖 toggle <100ms）。");
    println!("native 总 PSS={} KiB map={} ms；web 总 PSS={} KiB map={} ms",
        summary[0].2, summary[0].1, summary[1].2, summary[1].1);
}

fn pss_kb(proc_dir: &std::path::Path) -> u64 {
    let Ok(text) = std::fs::read_to_string(proc_dir.join("smaps_rollup")) else {
        return 0;
    };
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("Pss:") {
            return v.trim().trim_end_matches(" kB").parse().unwrap_or(0);
        }
    }
    0
}

/// 汇总本进程直接子代里 WebKit 辅助进程的 PSS（多进程架构真实占用）
fn webkit_children_pss_kb(ppid: u32) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return 0;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let dir = PathBuf::from(format!("/proc/{pid}"));
        let Ok(comm) = std::fs::read_to_string(dir.join("comm")) else {
            continue;
        };
        if !comm.trim().starts_with("WebKit") {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(dir.join("stat")) else {
            continue;
        };
        // stat: pid (comm) state ppid …（comm 可含空格，取最后一个 ')' 之后）
        let Some(rest) = stat.rsplit_once(')').map(|(_, r)| r) else {
            continue;
        };
        let ppid_field = rest.split_whitespace().nth(1).unwrap_or("0");
        if ppid_field == ppid.to_string() {
            total += pss_kb(&dir);
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pss_parses_self() {
        // 本测试进程必有非零 PSS
        assert!(pss_kb(&PathBuf::from("/proc/self")) > 0);
    }

    #[test]
    fn webkit_children_tolerates_absence() {
        // 无 webkit 子进程时返回 0（正常环境）
        assert_eq!(webkit_children_pss_kb(u32::MAX - 1), 0);
    }

    #[test]
    fn rows_and_page_are_wellformed() {
        assert_eq!(rows().len(), 10);
        let page = html_page();
        assert!(page.contains("转换时间戳"));
        assert!(page.contains("</html>"));
    }
}
