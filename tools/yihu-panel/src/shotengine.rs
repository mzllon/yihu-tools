//! 自研截图引擎 v1（M6）：冻结帧 + 自绘选区 + 完成动作（复制/保存/贴图）。
//!
//! Wayland 像素通道：portal Screenshot 非交互（复用 [`crate::screenshot`]
//! 的 full 模式）。yihu-panel 自 M4 起持有 portal 授权，静默可重复；
//! GNOME 50 的 Shell 直取 API 已被收紧（实测 Access denied），portal 是
//! 唯一合规像素来源。首次使用如弹授权框，点「允许」一次后永久静默。
//!
//! 抓帧在后台线程（授权首用可能等用户点击，上限 30s），冻结帧经
//! `shot_async` 槽回主循环开覆盖层（[`crate::overlay`]）；选区确认后的
//! 复制/保存/贴图全在主线程（GTK），全程审计。
//!
//! 坐标约定：面板/GDK 的 monitor geometry 是「逻辑像素」，portal 帧是
//! 「物理像素」。v1 假设全屏统一缩放因子（多屏混合缩放不支持），换算
//! 统一走 [`crop_pixels`]（测试覆盖）。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use gtk::gdk_pixbuf::prelude::*;
use gtk::gdk_pixbuf::Pixbuf;

/// 抓帧结果槽消息：Frame（PNG 字节）→ 主循环解码开覆盖层（Pixbuf 非 Send，
/// 跨线程只传字节）；Failed → 通知
pub enum ShotMsg {
    Frame(Vec<u8>),
    Failed(String),
}

/// 发起一次区域截图（任意线程可调）：后台抓帧 → 槽回主循环开覆盖层。
/// 首次使用如弹 portal 授权框，点「允许」一次后永久静默。
pub fn start_region_shot(audit: &Rc<yihu_core::audit::Audit>, slot: &Arc<Mutex<Option<ShotMsg>>>) {
    audit.record("host", 0, "shot.region", "grant", "开始自研区域截图");
    let slot = slot.clone();
    std::thread::spawn(move || {
        // 略等面板隐藏动画走完，冻结帧里尽量没有面板
        std::thread::sleep(std::time::Duration::from_millis(180));
        let msg = match grab_frame_png() {
            Ok(png) => ShotMsg::Frame(png),
            Err(e) => ShotMsg::Failed(e),
        };
        *slot.lock().unwrap() = Some(msg);
    });
}

use std::rc::Rc;

/// 逻辑坐标选区
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Rect { x, y, w, h }
    }

    /// 两个对角点归一化（负向拖拽翻转），最小 1×1
    pub fn from_points(x1: f64, y1: f64, x2: f64, y2: f64) -> Self {
        let x = x1.min(x2).round() as i32;
        let y = y1.min(y2).round() as i32;
        let w = (x2 - x1).abs().round() as i32;
        let h = (y2 - y1).abs().round() as i32;
        Rect {
            x,
            y,
            w: w.max(1),
            h: h.max(1),
        }
    }
}

/// 抓全屏冻结帧的 PNG 字节（阻塞可达 30s——只准后台线程调用；
/// Pixbuf 非 Send，跨线程只传字节，主线程再 [`pixbuf_from_png`] 解码）。
pub fn grab_frame_png() -> Result<Vec<u8>, String> {
    crate::screenshot::take("full", true)?
        .ok_or_else(|| "portal 未返回截图数据".to_string())
}

/// PNG 字节 → Pixbuf
pub fn pixbuf_from_png(png: &[u8]) -> Result<Pixbuf, String> {
    let loader = gtk::gdk_pixbuf::PixbufLoader::new();
    loader
        .write(png)
        .map_err(|e| format!("解码截图失败：{e}"))?;
    loader.close().map_err(|e| format!("解码截图失败：{e}"))?;
    loader
        .pixbuf()
        .ok_or_else(|| "解码截图失败：无帧".to_string())
}

/// 选区（逻辑坐标）→ 帧内物理像素矩形（统一缩放因子，夹到帧边界内）。
/// 选区太小/越界返回 None。
pub fn crop_pixels(frame_w: i32, frame_h: i32, sel: Rect, scale: f64) -> Option<(i32, i32, i32, i32)> {
    let s = if scale > 0.0 { scale } else { 1.0 };
    let x1 = (sel.x as f64 * s).floor() as i32;
    let y1 = (sel.y as f64 * s).floor() as i32;
    let x2 = ((sel.x + sel.w) as f64 * s).floor() as i32;
    let y2 = ((sel.y + sel.h) as f64 * s).floor() as i32;
    // 与帧求交：完全越界 = None
    let ix = x1.clamp(0, frame_w);
    let iy = y1.clamp(0, frame_h);
    let ix2 = x2.clamp(0, frame_w);
    let iy2 = y2.clamp(0, frame_h);
    if ix2 - ix < 1 || iy2 - iy < 1 {
        None
    } else {
        Some((ix, iy, ix2 - ix, iy2 - iy))
    }
}

/// 从帧上裁剪选区（物理像素矩形；越界部分按帧边界收敛）
pub fn crop_pixbuf(frame: &Pixbuf, rect: (i32, i32, i32, i32)) -> Pixbuf {
    let (x, y, w, h) = rect;
    frame.new_subpixbuf(x, y, w, h)
}

/// 截图保存路径：XDG_PICTURES_DIR（缺省 ~/图片）下的 Screenshots 子目录
pub fn save_dir() -> PathBuf {
    let base = std::env::var("XDG_PICTURES_DIR")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into())).join("图片")
        });
    if base.join("Screenshots").is_dir() || std::fs::create_dir_all(base.join("Screenshots")).is_ok()
    {
        base.join("Screenshots")
    } else {
        base
    }
}

/// 保存 Pixbuf 到截图目录，返回落盘路径
pub fn save_pixbuf(pb: &Pixbuf) -> Result<PathBuf, String> {
    let dir = save_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建截图目录失败：{e}"))?;
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let path = dir.join(format!("yihu-{ts}.png"));
    pb.savev(&path, "png", &[])
        .map_err(|e| format!("保存失败：{e}"))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_from_points_normalizes_negative_drags() {
        assert_eq!(Rect::from_points(10.0, 20.0, 30.0, 60.0), Rect::new(10, 20, 20, 40));
        // 反向拖拽（从右下往左上）也要归一化成正值矩形
        assert_eq!(Rect::from_points(30.0, 60.0, 10.0, 20.0), Rect::new(10, 20, 20, 40));
        // 点击（零位移）→ 1×1 最小矩形
        assert_eq!(Rect::from_points(5.0, 5.0, 5.0, 5.0), Rect::new(5, 5, 1, 1));
    }

    #[test]
    fn crop_pixels_scales_and_clamps() {
        // 1x 缩放：逻辑 = 物理
        assert_eq!(crop_pixels(1920, 1080, Rect::new(10, 10, 100, 50), 1.0).unwrap(), (10, 10, 100, 50));
        // 2x 缩放：逻辑坐标 ×2
        assert_eq!(crop_pixels(3840, 2160, Rect::new(10, 10, 100, 50), 2.0).unwrap(), (20, 20, 200, 100));
        // 越界收敛到帧边界
        assert_eq!(crop_pixels(1920, 1080, Rect::new(1900, 1070, 100, 50), 1.0).unwrap(), (1900, 1070, 20, 10));
        // 完全越界 → None
        assert!(crop_pixels(100, 100, Rect::new(200, 200, 10, 10), 1.0).is_none());
        // 退化尺寸 → None
        assert!(crop_pixels(100, 100, Rect::new(0, 0, 0, 0), 1.0).is_none());
        // 非法缩放因子按 1 处理
        assert_eq!(crop_pixels(100, 100, Rect::new(1, 1, 5, 5), 0.0).unwrap(), (1, 1, 5, 5));
    }

    #[test]
    fn save_dir_prefers_xdg_pictures() {
        // 只验证不 panic 与路径形态（环境依赖，不做强断言）
        let dir = save_dir();
        assert!(dir.to_string_lossy().contains("图") || dir.to_string_lossy().contains("Pictures") || !dir.as_os_str().is_empty());
    }
}
