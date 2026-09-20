//! AutoDark 执行器：读取配置并应用主题，无任何 UI / GTK 依赖。
//!
//! 由 systemd 用户定时器每分钟调用一次（`autodark-agent apply`），
//! 进程按需启动、即退即走，常驻资源为零。

use yihu_core::autodark;

fn main() {
    let cmd = std::env::args().nth(1);
    if cmd.as_deref() != Some("apply") {
        eprintln!("用法: autodark-agent apply");
        std::process::exit(2);
    }
    let cfg = autodark::Config::load();
    if !cfg.enabled {
        return; // 未启用：静默退出，定时器保留但无事可做
    }
    match autodark::apply(&cfg) {
        Ok(theme) => println!("autodark: 已应用 {}", theme.name()),
        Err(e) => {
            eprintln!("autodark-agent: {e}");
            std::process::exit(1);
        }
    }
}
