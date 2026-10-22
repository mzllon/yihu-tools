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
mod icons;
mod fdk;
mod legacy;
mod pulse;
mod store;
mod stream;
mod theme;
mod tray;
mod tsdemux;
mod ui;

use std::cell::RefCell;
use std::rc::Rc;

use gtk::gio;
use gtk::prelude::*;

fn main() {
    // glibc 每线程 malloc arena 上限压到 2：引擎每次换台起停线程组，
    // 默认「每线程一 arena」会让 touched 页随换台次数缓慢累积
    // （实测 20 轮换台 +11MB）。电台是低频分配场景，锁竞争无感。
    unsafe { libc::mallopt(-8 /* M_ARENA_MAX */, 2) };
    // 基座统一日志：文件落盘 + panic 钩子（GTK 回调 panic 即 abort，
    // 没有钩子闪退零现场——BUG-008）。要放最前，探测失败也要留痕。
    yihu_core::pluginlog::init("radio");
    // 电台图标装入用户 hicolor + desktop 文件（窗口/任务栏/通知按名
    // 取图与产品名）；纯装饰，失败只记日志。须在 UI 构建之前。
    icons::install_assets();
    // 渲染栈瘦身（2026-10-19 实测）：Wayland 会话上若落到 X11 后端
    // （如 shell 环境残留 GDK_BACKEND=x11），gtk_init 的
    // gdk_display_prepare_gl 会无条件初始化 EGL，拉进 NVIDIA/Mesa 全家
    // ≈ +28MB PSS 且无窗收听也驻留。检测到 Wayland 会话即强制 wayland
    // 后端（X11 会话机器无 WAYLAND_DISPLAY，不受影响）+ cairo 软渲染
    // （电台 UI 静态，软渲染足够，开窗也不引 GL）。
    if std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty()) {
        std::env::set_var("GDK_BACKEND", "wayland");
        std::env::set_var("GSK_RENDERER", "cairo");
    }
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
            // 播放环境探测提前到首帧：缺播放组件直接给出可安装提示并退出
            if let Err(e) = engine::Engine::new().probe() {
                yihu_core::perror!("播放环境探测失败：{e}");
                eprintln!("radio-player: {e}");
                let _ = std::process::Command::new("notify-send")
                    .args(["--app-name=一呼电台", "--icon=tools.yihu.radio", &e])
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
