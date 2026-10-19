//! PulseAudio Simple API（dlopen 运行时加载）：推模式写 PCM。
//!
//! 流经 pulse 服务器形成 sink-input——「与应用音量」（pactl 单流音量）
//! 语义与 gst-play 时期一致，GNOME 音量面板/声音设置里显示为「一呼电台」。
//! libpulse 是 GNOME 桌面必装件，只做运行时探测、零构建期依赖
//! （省掉 libpulse-dev）。签名是 pa_simple 系公开稳定 API（2007 年定形）。

use std::ffi::CString;
use std::sync::Arc;

/// pa_stream_direction_t.PA_STREAM_PLAYBACK。
const PA_STREAM_PLAYBACK: i32 = 1;
/// pa_sample_format_t.PA_SAMPLE_S16NE；本机目标（x86-64/aarch64）小端即 S16LE=3。
const PA_SAMPLE_S16NE: u32 = 3;

/// pa_sample_spec（S16NE + rate + channels），repr(C) 对拍 pulse/sample.h。
#[repr(C)]
#[derive(Clone, Copy)]
struct SampleSpec {
    format: u32,
    rate: u32,
    channels: u8,
}

type NewFn = unsafe extern "C" fn(
    *const std::ffi::c_char,
    *const std::ffi::c_char,
    i32,
    *const std::ffi::c_char,
    *const std::ffi::c_char,
    *const SampleSpec,
    *const std::ffi::c_void,
    *const std::ffi::c_void,
    *mut i32,
) -> *mut std::ffi::c_void;
type FreeFn = unsafe extern "C" fn(*mut std::ffi::c_void);
type WriteFn = unsafe extern "C" fn(*mut std::ffi::c_void, *const std::ffi::c_void, usize, *mut i32) -> i32;
type FlushFn = unsafe extern "C" fn(*mut std::ffi::c_void, *mut i32) -> i32;

pub struct Lib {
    dl: *mut std::ffi::c_void,
    new: NewFn,
    free: FreeFn,
    write: WriteFn,
    flush: FlushFn,
}

unsafe impl Send for Lib {}
unsafe impl Sync for Lib {}

impl Lib {
    pub fn load() -> Option<Arc<Lib>> {
        ["libpulse-simple.so.0", "libpulse-simple.so.1", "libpulse-simple.so"]
            .iter()
            .find_map(|name| try_load(name).map(Arc::new))
    }
}

fn try_load(name: &str) -> Option<Lib> {
    let c = CString::new(name).ok()?;
    let dl = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if dl.is_null() {
        return None;
    }
    let lib = Lib {
        dl,
        new: sym(dl, "pa_simple_new")?,
        free: sym(dl, "pa_simple_free")?,
        write: sym(dl, "pa_simple_write")?,
        flush: sym(dl, "pa_simple_flush")?,
    };
    Some(lib)
}

fn sym<T>(dl: *mut std::ffi::c_void, name: &str) -> Option<T> {
    let c = CString::new(name).ok()?;
    let p = unsafe { libc::dlsym(dl, c.as_ptr()) };
    if p.is_null() {
        unsafe { libc::dlclose(dl) };
        None
    } else {
        Some(unsafe { std::mem::transmute_copy::<*mut std::ffi::c_void, T>(&p) })
    }
}

impl Drop for Lib {
    fn drop(&mut self) {
        unsafe { libc::dlclose(self.dl) };
    }
}

/// 一条已连接的播放流。仅解码线程内创建与销毁（不跨线程）。
pub struct Sink {
    lib: Arc<Lib>,
    s: *mut std::ffi::c_void,
}

impl Sink {
    /// 连接默认输出设备；失败（无声卡/服务器不可达）返回 None。
    pub fn connect(lib: Arc<Lib>, rate: u32, channels: u8) -> Option<Sink> {
        let spec = SampleSpec {
            format: PA_SAMPLE_S16NE,
            rate,
            channels,
        };
        let name = CString::new("yihu-radio").ok()?;
        let stream = CString::new("播放").ok()?;
        let s = unsafe {
            (lib.new)(
                std::ptr::null(), // server：默认
                name.as_ptr(),
                PA_STREAM_PLAYBACK,
                std::ptr::null(), // device：默认
                stream.as_ptr(),
                &spec,
                std::ptr::null(), // channel map：按声道数取默认
                std::ptr::null(), // buffer attr：服务器默认（~2s 缓冲，吸收网络抖动）
                std::ptr::null_mut(),
            )
        };
        if s.is_null() {
            None
        } else {
            Some(Sink { lib, s })
        }
    }

    /// 写一帧交错 PCM；false = 与服务器连接异常。
    pub fn write(&self, pcm: &[i16]) -> bool {
        let bytes = unsafe {
            std::slice::from_raw_parts(pcm.as_ptr() as *const u8, std::mem::size_of_val(pcm))
        };
        unsafe {
            (self.lib.write)(
                self.s,
                bytes.as_ptr() as *const std::ffi::c_void,
                bytes.len(),
                std::ptr::null_mut(),
            ) == 0
        }
    }
}

impl Drop for Sink {
    fn drop(&mut self) {
        // 先 flush 丢弃服务端已缓冲的 ~2s 音频：停播立即静音，不留尾巴。
        unsafe {
            (self.lib.flush)(self.s, std::ptr::null_mut());
            (self.lib.free)(self.s);
        }
    }
}
