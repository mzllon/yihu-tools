//! 电台图标（scripts/gen_icon.py 的 radio 样式产物）：
//! 内嵌进二进制，安装到用户 hicolor 图标主题（窗口/任务栏/通知按名
//! 取图），并解码为托盘像素。

use std::path::PathBuf;

use yihu_core::pwarn;

/// 彩色 32×32：托盘播放态。
pub const RADIO_32: &[u8] = include_bytes!("../icons/32x32.png");
/// 灰度 32×32：托盘停止态（去饱和，弱化「在播」感知）。
pub const RADIO_32_STOPPED: &[u8] = include_bytes!("../icons/radio-32-stopped.png");
/// hicolor 安装用三档尺寸（hicolor 约定尺寸档）。
pub const RADIO_128: &[u8] = include_bytes!("../icons/128x128.png");
pub const RADIO_256: &[u8] = include_bytes!("../icons/256x256.png");
pub const RADIO_512: &[u8] = include_bytes!("../icons/512x512.png");

/// 图标主题名：与 application_id 一致（hicolor 安装路径同名）。
pub const ICON_NAME: &str = "tools.yihu.radio";

/// GNOME Wayland 的 Shell 按 application id 找同名 .desktop 文件取
/// 产品名与图标（BUG-010 图标轮：没有它，任务栏/alt-tab 显示原始
/// app-id 与默认齿轮图标）。文件 id 必须与 application_id 完全一致。
const DESKTOP_ID: &str = "tools.yihu.radio.desktop";

/// 安装图标主题 + desktop 文件（幂等：内容一致就跳过）。
/// 纯装饰，失败只记日志——托盘像素是内嵌的，不受影响。
pub fn install_assets() {
    for (size, png) in [
        (32, RADIO_32),
        (128, RADIO_128),
        (256, RADIO_256),
        (512, RADIO_512),
    ] {
        let dir = hicolor_dir(size);
        let path = dir.join("apps").join(format!("{ICON_NAME}.png"));
        if std::fs::read(&path).is_ok_and(|b| b == png) {
            continue;
        }
        if std::fs::create_dir_all(&dir).is_err() {
            pwarn!("图标目录创建失败：{}", dir.display());
            continue;
        }
        if let Err(e) = std::fs::write(&path, png) {
            pwarn!("图标写入失败 {}：{e}", path.display());
        }
    }
    install_desktop();
}

/// 安装 desktop 文件到用户 applications 目录（应用网格可见「一呼电台」，
/// 运行窗口据此显示产品名与图标）。
fn install_desktop() {
    let Ok(exe) = std::env::current_exe() else {
        pwarn!("无法定位自身路径，desktop 文件跳过");
        return;
    };
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_default();
    let dir = data_home.join("applications");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(DESKTOP_ID);
    let content = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=一呼电台\n\
         Name[en]=Yihu Radio\n\
         Exec=\"{}\"\n\
         Icon={ICON_NAME}\n\
         Terminal=false\n\
         Categories=Audio;AudioVideo;Utility;\n\
         StartupNotify=true\n"
    , exe.display());
    if std::fs::read_to_string(&path).is_ok_and(|b| b == content) {
        return;
    }
    if let Err(e) = std::fs::write(&path, &content) {
        pwarn!("desktop 写入失败 {}：{e}", path.display());
    }
}

fn hicolor_dir(size: u32) -> PathBuf {
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_default();
    data_home.join(format!("icons/hicolor/{size}x{size}"))
}

/// 解码 PNG 为 ksni ARGB32 像素（与旧 draw_icon 输出同序：A,R,G,B）。
fn decode_argb(png: &'static [u8]) -> Result<(i32, i32, Vec<u8>), String> {
    let pb = gtk::gdk_pixbuf::Pixbuf::from_read(std::io::Cursor::new(png))
        .map_err(|e| format!("{e}"))?;
    let (w, h) = (pb.width(), pb.height());
    let n = pb.n_channels() as usize;
    if n != 4 {
        return Err(format!("像素通道数 {n} ≠ 4（RGBA）"));
    }
    let src = pb.read_pixel_bytes();
    let rowstride = pb.rowstride() as usize;
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h as usize {
        let row = &src[y * rowstride..y * rowstride + w as usize * 4];
        for p in row.chunks_exact(4) {
            // RGBA(PNG) → A,R,G,B（ksni ARGB32 序）
            px.extend_from_slice(&[p[3], p[0], p[1], p[2]]);
        }
    }
    Ok((w, h, px))
}

/// 托盘两态像素（播放=彩色 / 停止=灰度）。**须在主线程调用一次**
/// （gdk-pixbuf 解码），托盘线程只取结果。
pub fn tray_pixmaps() -> (ksni::Icon, ksni::Icon) {
    let (play, stop) = match (decode_argb(RADIO_32), decode_argb(RADIO_32_STOPPED)) {
        (Ok(p), Ok(s)) => (p, s),
        (Err(e), _) | (_, Err(e)) => {
            pwarn!("托盘图标解码失败，回退纯色占位：{e}");
            let f = (32i32, 32i32, vec![[255u8, 0xc6, 0x46, 0x00]; 32 * 32].concat());
            (f.clone(), f)
        }
    };
    (
        ksni::Icon { width: play.0, height: play.1, data: play.2 },
        ksni::Icon { width: stop.0, height: stop.1, data: stop.2 },
    )
}
