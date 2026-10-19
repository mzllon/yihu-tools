//! 一呼电台（native 官方插件）：独立 GTK4 进程，播放器优先窗体。
//!
//! GApplication 单实例（mdworkbench 惯例）：面板行/插件中心/命令行重复
//! 拉起都经 command-line 转发到运行中实例——
//! `--open` 开窗置前 / `--play <id> [标题]` 后台开播（不弹窗）/
//! `--stop` 停播 / `--quit` 退出（停播）。无参 = 开窗。
//!
//! 关窗 = 隐藏（收听不中断，设计稿 §6.2）；进程独立于面板生命周期。

mod api;
mod engine;
mod store;
mod theme;
mod ui;

use std::cell::RefCell;
use std::rc::Rc;

use gtk::gio;
use gtk::prelude::*;

fn main() {
    // 基座统一日志：文件落盘 + panic 钩子（GTK 回调 panic 即 abort，
    // 没有钩子闪退零现场——BUG-008）。要放最前，探测失败也要留痕。
    yihu_core::pluginlog::init("radio");
    let app = gtk::Application::builder()
        .application_id("tools.yihu.radio")
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    // 单实例：主进程在首个 command-line 时构建整窗，后续转发分发
    let state: Rc<RefCell<Option<Rc<ui::RadioApp>>>> = Rc::new(RefCell::new(None));
    let s = state.clone();
    app.connect_command_line(move |app, cmd| {
        let args: Vec<String> = cmd
            .arguments()
            .iter()
            .map(|a| a.to_string_lossy().to_string())
            .skip(1)
            .collect();
        if s.borrow().is_none() {
            // 主实例首次建窗才记启动行（CLI 转发进程也会进本函数，别刷屏）
            yihu_core::pinfo!(
                "radio-player 主实例启动 pid={} args={:?}",
                std::process::id(),
                args
            );
            // 播放环境探测提前到首帧：缺 gst 组件直接给出可安装提示并退出
            if let Err(e) = engine::Engine::new().probe() {
                yihu_core::perror!("播放环境探测失败：{e}");
                eprintln!("radio-player: {e}");
                let _ = std::process::Command::new("notify-send")
                    .args(["--app-name=一呼电台", &e])
                    .status();
                std::process::exit(1);
            }
            *s.borrow_mut() = Some(ui::RadioApp::build(app));
        }
        if let Some(ra) = s.borrow().as_ref() {
            ra.dispatch(&args);
        }
        0
    });
    app.run();
}
