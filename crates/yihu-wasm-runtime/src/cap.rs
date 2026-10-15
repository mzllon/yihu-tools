//! WASM 插件侧能力调用封装（W4）。
//!
//! 每个宿主能力对应一个独立导入函数（模块名 `yihu`，统一 7 参签名
//! `(id, p1ptr, p1len, p2ptr, p2len, p3ptr, p3len) -> i32`）——架构终版
//! §4「manifest permissions = import 白名单」：宿主装载期枚举模块导入，
//! 导入了未声明能力的模块直接拒载。
//!
//! 调用模型与 stdio 轨的 capability_request 相同：**异步**。导入函数只把
//! 请求入队宿主即返回（宿主执行走 GTK/后台线程，绝不阻塞在 WASM 调用栈
//! 里——硬性规则 #1）；结果经插件导出的 `on_response(id, ok, ptr, len)`
//! 回传。wrapper 返回请求 id 供插件关联。

use std::sync::atomic::{AtomicI32, Ordering};

// 宿主导入（模块 yihu）。未引用的声明会被 LLVM 裁剪，不会出现在
// 最终模块的 import 段——宿主装载期校验因此不会误伤未用能力的插件。
// 宿主目标编译时以失败桩替代（crate 保持可测试；桩返回 -1 = 请求不入队）。
#[cfg(target_arch = "wasm32")]
mod imp {
    // 导入函数名 = 宿主 Linker 名表里的 kebab 名（#[link_name] 指定；
    // Rust 标识符本身叫 __cap_* 无所谓）
    #[link(wasm_import_module = "yihu")]
    extern "C" {
        #[link_name = "clipboard-write"]
        pub fn __cap_clipboard_write(id: i32, a: i32, al: i32, b: i32, bl: i32, c: i32, cl: i32) -> i32;
        #[link_name = "clipboard-read"]
        pub fn __cap_clipboard_read(id: i32, a: i32, al: i32, b: i32, bl: i32, c: i32, cl: i32) -> i32;
        #[link_name = "open-uri"]
        pub fn __cap_open_uri(id: i32, a: i32, al: i32, b: i32, bl: i32, c: i32, cl: i32) -> i32;
        #[link_name = "launch-app"]
        pub fn __cap_launch_app(id: i32, a: i32, al: i32, b: i32, bl: i32, c: i32, cl: i32) -> i32;
        #[link_name = "notify"]
        pub fn __cap_notify(id: i32, a: i32, al: i32, b: i32, bl: i32, c: i32, cl: i32) -> i32;
        #[link_name = "screenshot-take"]
        pub fn __cap_screenshot_take(id: i32, a: i32, al: i32, b: i32, bl: i32, c: i32, cl: i32) -> i32;
        #[link_name = "network-fetch"]
        pub fn __cap_network_fetch(id: i32, a: i32, al: i32, b: i32, bl: i32, c: i32, cl: i32) -> i32;
        #[link_name = "settings-write"]
        pub fn __cap_settings_write(id: i32, a: i32, al: i32, b: i32, bl: i32, c: i32, cl: i32) -> i32;
        #[link_name = "settings-reset"]
        pub fn __cap_settings_reset(id: i32, a: i32, al: i32, b: i32, bl: i32, c: i32, cl: i32) -> i32;
        #[link_name = "settings-read"]
        pub fn __cap_settings_read(id: i32, a: i32, al: i32, b: i32, bl: i32, c: i32, cl: i32) -> i32;
        #[link_name = "fs-write"]
        pub fn __cap_fs_write(id: i32, a: i32, al: i32, b: i32, bl: i32, c: i32, cl: i32) -> i32;
        #[link_name = "fs-read"]
        pub fn __cap_fs_read(id: i32, a: i32, al: i32, b: i32, bl: i32, c: i32, cl: i32) -> i32;
        #[link_name = "audio-play"]
        pub fn __cap_audio_play(id: i32, a: i32, al: i32, b: i32, bl: i32, c: i32, cl: i32) -> i32;
        #[link_name = "audio-stop"]
        pub fn __cap_audio_stop(id: i32, a: i32, al: i32, b: i32, bl: i32, c: i32, cl: i32) -> i32;
        #[link_name = "audio-volume"]
        pub fn __cap_audio_volume(id: i32, a: i32, al: i32, b: i32, bl: i32, c: i32, cl: i32) -> i32;
        #[link_name = "qr-generate"]
        pub fn __cap_qr_generate(id: i32, a: i32, al: i32, b: i32, bl: i32, c: i32, cl: i32) -> i32;
        #[link_name = "qr-decode"]
        pub fn __cap_qr_decode(id: i32, a: i32, al: i32, b: i32, bl: i32, c: i32, cl: i32) -> i32;
        #[link_name = "md-render"]
        pub fn __cap_md_render(id: i32, a: i32, al: i32, b: i32, bl: i32, c: i32, cl: i32) -> i32;
        #[link_name = "md-view"]
        pub fn __cap_md_view(id: i32, a: i32, al: i32, b: i32, bl: i32, c: i32, cl: i32) -> i32;
        #[link_name = "sticky-pin"]
        pub fn __cap_sticky_pin(id: i32, a: i32, al: i32, b: i32, bl: i32, c: i32, cl: i32) -> i32;
        #[link_name = "sticky-close"]
        pub fn __cap_sticky_close(id: i32, a: i32, al: i32, b: i32, bl: i32, c: i32, cl: i32) -> i32;
        #[link_name = "ui-show"]
        pub fn __yihu_ui_show(id: i32, a: i32, al: i32, b: i32, bl: i32, c: i32, cl: i32) -> i32;
        #[link_name = "log"]
        pub fn __yihu_log(id: i32, a: i32, al: i32, b: i32, bl: i32, c: i32, cl: i32) -> i32;
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    // 宿主桩：宏按名字生成同名函数，签名统一 8 参（含 id）
    macro_rules! stubs {
        ($($name:ident),* $(,)?) => {
            $(
                pub unsafe extern "C" fn $name(
                    _id: i32, _a: i32, _al: i32, _b: i32, _bl: i32, _c: i32, _cl: i32,
                ) -> i32 {
                    -1
                }
            )*
        };
    }
    stubs!(
        __cap_clipboard_write, __cap_clipboard_read, __cap_open_uri, __cap_launch_app,
        __cap_notify, __cap_screenshot_take, __cap_network_fetch, __cap_settings_write,
        __cap_settings_reset, __cap_settings_read, __cap_fs_write, __cap_fs_read,
        __cap_audio_play, __cap_audio_stop, __cap_audio_volume, __cap_qr_generate,
        __cap_qr_decode, __cap_md_render, __cap_md_view, __cap_sticky_pin,
        __cap_sticky_close, __yihu_ui_show, __yihu_log,
    );
}

use imp::*;

/// 把字符串写入共享区，返回 (绝对地址, 长度)。空串 → (0, 0)。
fn push(s: &str) -> (i32, i32) {
    if s.is_empty() {
        return (0, 0);
    }
    let ptr = super::alloc(s.len() as i32);
    if ptr < 0 {
        return (-1, 0);
    }
    unsafe {
        let base = &raw const super::__HEAP as usize;
        let off = ptr as usize - base;
        super::__HEAP[off..off + s.len()].copy_from_slice(s.as_bytes());
    }
    (ptr, s.len() as i32)
}

static NEXT_ID: AtomicI32 = AtomicI32::new(1);

/// 统一入队：自动分配请求 id，转发给对应宿主导入。返回请求 id
/// （供 on_response 关联）；宿主队列满返回 -1。
fn req(
    f: unsafe extern "C" fn(i32, i32, i32, i32, i32, i32, i32) -> i32,
    args: [&str; 3],
) -> i32 {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let (a, al) = push(args[0]);
    let (b, bl) = push(args[1]);
    let (c, cl) = push(args[2]);
    if a < 0 || b < 0 || c < 0 {
        return -1;
    }
    let ok = unsafe { f(id, a, al, b, bl, c, cl) };
    if ok < 0 {
        -1
    } else {
        id
    }
}

// —— 能力 wrapper（参数按宿主 caps::evaluate 的 params 键序）——
/// 写剪贴板。data = 无。
pub fn clipboard_write(text: &str) -> i32 {
    req(__cap_clipboard_write, [text, "", ""])
}
/// 读剪贴板。on_response.data = 文本。
pub fn clipboard_read() -> i32 {
    req(__cap_clipboard_read, ["", "", ""])
}
pub fn open_uri(uri: &str) -> i32 {
    req(__cap_open_uri, [uri, "", ""])
}
pub fn launch_app(desktop_id: &str) -> i32 {
    req(__cap_launch_app, [desktop_id, "", ""])
}
/// 发系统通知。on_response 收执行结果。
pub fn notify(summary: &str, body: &str) -> i32 {
    req(__cap_notify, [summary, body, ""])
}
/// 截屏（mode = "full"/"area"；clip = true 时结果进剪贴板）。
pub fn screenshot_take(mode: &str, clip: bool) -> i32 {
    req(__cap_screenshot_take, [mode, if clip { "1" } else { "" }, ""])
}
/// 经宿主 GET。on_response.data = 响应体（≤ 共享区余量）。
pub fn network_fetch(url: &str) -> i32 {
    req(__cap_network_fetch, [url, "", ""])
}
/// 写 gsettings（schema 必须在 manifest settings.write 声明集内）。
pub fn settings_write(schema: &str, key: &str, value: &str) -> i32 {
    req(__cap_settings_write, [schema, key, value])
}
/// 重置 gsettings 键。
pub fn settings_reset(schema: &str, key: &str) -> i32 {
    req(__cap_settings_reset, [schema, key, ""])
}
/// 读 gsettings。on_response.data = 值文本。
pub fn settings_read(schema: &str, key: &str) -> i32 {
    req(__cap_settings_read, [schema, key, ""])
}
/// 宿主代写文件（路径须命中 manifest fs.write glob）。
pub fn fs_write(path: &str, text: &str) -> i32 {
    req(__cap_fs_write, [path, text, ""])
}
/// 宿主代读文件。on_response.data = 文本。
pub fn fs_read(path: &str) -> i32 {
    req(__cap_fs_read, [path, "", ""])
}
pub fn audio_play(url: &str) -> i32 {
    req(__cap_audio_play, [url, "", ""])
}
pub fn audio_stop() -> i32 {
    req(__cap_audio_stop, ["", "", ""])
}
pub fn audio_volume(pct: u32) -> i32 {
    req(__cap_audio_volume, [&pct.to_string(), "", ""])
}
pub fn qr_generate(text: &str) -> i32 {
    req(__cap_qr_generate, [text, "", ""])
}
pub fn qr_decode() -> i32 {
    req(__cap_qr_decode, ["", "", ""])
}
/// mode = "preview"/"html"（html 模式 on_response.data = HTML）。
pub fn md_render(mode: &str, text: &str) -> i32 {
    req(__cap_md_render, [mode, text, ""])
}
pub fn md_view(title: &str, text: &str) -> i32 {
    req(__cap_md_view, [title, text, ""])
}
pub fn sticky_pin(sticky_id: &str) -> i32 {
    req(__cap_sticky_pin, [sticky_id, "", ""])
}
pub fn sticky_close(sticky_id: &str) -> i32 {
    req(__cap_sticky_close, [sticky_id, "", ""])
}

/// 请求宿主渲染声明式 UI 表单（spec 与 stdio 轨 ui.show 同构）。
/// 用户提交/取消经 on_ui_event 回 JSON。
pub fn ui_show(spec: &str) -> i32 {
    req(__yihu_ui_show, [spec, "", ""])
}

/// 调试日志（YIHU_PANEL_DEBUG=1 时宿主输出到 stderr）。
pub fn log(msg: &str) -> i32 {
    req(__yihu_log, [msg, "", ""])
}
