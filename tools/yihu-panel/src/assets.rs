//! 面板图标与 desktop 文件自装。
//!
//! GNOME Wayland 的 Shell 按 application id（tools.yihu.Panel）找同名
//! desktop 文件取产品名与图标；面板此前只有 autostart 文件（id =
//! yihu-panel，不匹配），alt-tab/任务栏会露原始 id + 默认齿轮图标。
//! 启动时自装 hicolor 图标与 applications desktop 文件（幂等：内容
//! 一致即跳过）。纯装饰，失败只记日志。

use std::path::PathBuf;

use yihu_core::pwarn;

/// 图标主题名：与 application_id（service::BUS_NAME）完全一致。
pub const ICON_NAME: &str = "tools.yihu.Panel";

const ICON_32: &[u8] = include_bytes!("../icons/32x32.png");
const ICON_128: &[u8] = include_bytes!("../icons/128x128.png");
const ICON_256: &[u8] = include_bytes!("../icons/256x256.png");
const ICON_512: &[u8] = include_bytes!("../icons/512x512.png");

/// 安装 hicolor 图标 + applications desktop 文件。main 最前调用
/// （纯文件操作，不依赖 GTK）。
pub fn install_assets() {
    for (size, png) in [(32, ICON_32), (128, ICON_128), (256, ICON_256), (512, ICON_512)] {
        let dir = hicolor_dir(size).join("apps");
        let path = dir.join(format!("{ICON_NAME}.png"));
        if std::fs::read(&path).is_ok_and(|b| b == png) {
            continue;
        }
        if std::fs::create_dir_all(&dir).is_err() {
            pwarn!("图标目录创建失败：{}", dir.display());
            continue;
        }
        if let Err(e) = std::fs::write(&path, png) {
            pwarn!("面板图标写入失败 {}：{e}", path.display());
        }
    }
    install_desktop();
}

fn install_desktop() {
    let Ok(exe) = std::env::current_exe() else {
        pwarn!("无法定位自身路径，面板 desktop 文件跳过");
        return;
    };
    let dir = data_home().join("applications");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("{ICON_NAME}.desktop"));
    let content = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=一呼面板\n\
         Comment=一呼即出的工具面板\n\
         Exec=\"{}\"\n\
         Icon={ICON_NAME}\n\
         Terminal=false\n\
         Categories=Utility;\n\
         StartupNotify=true\n",
        exe.display()
    );
    if std::fs::read_to_string(&path).is_ok_and(|b| b == content) {
        return;
    }
    if let Err(e) = std::fs::write(&path, &content) {
        pwarn!("面板 desktop 写入失败 {}：{e}", path.display());
    }
}

fn hicolor_dir(size: u32) -> PathBuf {
    data_home().join(format!("icons/hicolor/{size}x{size}"))
}

fn data_home() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_default()
}
