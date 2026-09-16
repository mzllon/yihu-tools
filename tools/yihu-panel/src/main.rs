//! 一呼 · 呼出面板：全局热键唤起的搜索式命令面板（M1 骨架）。
//!
//! 单二进制双角色：无参数 = 常驻守护进程（GTK4 隐藏待命窗口 + zbus 服务）；
//! `toggle/show/hide/quit/status` = 薄 CLI（连会话总线调用后即退）。
//! 性能底线：窗口与演示数据启动时建好、隐藏待命，呼出路径零 IO。

mod app;
mod calc;
mod service;

fn main() {
    // 全家统一约定：软件渲染压内存（见 README 实测教训）
    std::env::set_var("GSK_RENDERER", "cairo");
    if std::env::var_os("YIHU_PANEL_BENCH").is_some() {
        app::run_bench();
        return;
    }
    match std::env::args().nth(1).as_deref() {
        None => app::run_daemon(),
        Some("toggle") => std::process::exit(service::run_cli(service::Cmd::Toggle)),
        Some("show") => std::process::exit(service::run_cli(service::Cmd::Show)),
        Some("hide") => std::process::exit(service::run_cli(service::Cmd::Hide)),
        Some("quit") => std::process::exit(service::run_cli(service::Cmd::Quit)),
        Some("status") => std::process::exit(service::run_status()),
        Some(other) => {
            eprintln!("未知子命令 {other:?}\n用法: yihu-panel [toggle|show|hide|quit|status]");
            std::process::exit(2);
        }
    }
}
