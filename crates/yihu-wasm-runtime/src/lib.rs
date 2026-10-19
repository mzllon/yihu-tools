//! WASM 插件侧运行时：宿主 Linker 协议的插件端封装。
//!
//! 插件只需实现 `query_handler(text) -> Vec<Row>` 并调用 [`export_plugin!`]：
//!
//! ```no_run
//! use yihu_wasm_runtime::{export_plugin, Row};
//!
//! fn query_handler(text: &str) -> Vec<Row> {
//!     if text == "hi" {
//!         vec![Row::new("你好", "WASM", "hi!")]
//!     } else {
//!         vec![]
//!     }
//! }
//!
//! export_plugin!(query_handler);
//! ```
//!
//! 协议要点（踩坑沉淀，勿改）：
//! - 共享区 = 固定静态数组（Vec 会被 dlmalloc 搬家使宿主偏移失效）；
//! - 偏移一律线性内存绝对地址（heap_base + 内部偏移）；
//! - 不用 thread_local（wasm32 伪 TLS）。


/// 共享区容量（1 MiB；插件查询文本+结果字符串远小于此）
const HEAP_CAP: usize = 1 << 20;

pub mod cap;

/// 结果行
pub struct Row {
    pub title: String,
    pub subtitle: String,
    pub payload: String,
}

impl Row {
    pub fn new(title: impl Into<String>, subtitle: impl Into<String>, payload: impl Into<String>) -> Row {
        Row { title: title.into(), subtitle: subtitle.into(), payload: payload.into() }
    }

    fn encode(&self) -> Vec<u8> {
        // 简单帧：3 个 (u32 len, bytes) 段，避免 JSON
        let mut out = Vec::with_capacity(
            12 + self.title.len() + self.subtitle.len() + self.payload.len(),
        );
        for seg in [&self.title, &self.subtitle, &self.payload] {
            out.extend_from_slice(&(seg.len() as u32).to_le_bytes());
            out.extend_from_slice(seg.as_bytes());
        }
        out
    }
}

static mut ROWS: Option<Vec<Row>> = None;

fn rows() -> &'static mut Vec<Row> {
    unsafe {
        let ptr = &raw mut ROWS;
        (*ptr).get_or_insert_with(Vec::new)
    }
}

/// 宏：导出协议函数并绑定 handler。handler 签名 `fn(&str) -> Vec<Row>`。
/// 支持可选的第二形式 `export_plugin!(handler, with_time)`，handler 收
/// `(text, now_secs)`（需要当前时间的插件，如 cron）。
/// 插件查询处理函数 trait（export_plugin! 要求实现）
pub trait QueryHandler {
    /// 处理一条查询文本；now_secs = 宿主提供的当前时间
    fn handle(text: &str, now_secs: i64) -> Vec<Row>
    where
        Self: Sized;
}

/// 完整插件处理函数：查询 + 可选的激活/能力回包/UI 事件。
/// 旧式 `QueryHandler` 经毯子实现自动成为 `PluginHandler`（激活等
/// 全部为空实现），新插件按需覆写即可。
pub trait PluginHandler {
    fn handle(text: &str, now_secs: i64) -> Vec<Row>
    where
        Self: Sized;
    /// 结果行被用户激活（payload = 结果行第三字段）
    fn activate(_payload: &str) {}
    /// 能力执行回包（data 为宿主写入共享区的载荷，JSON 或原始文本）
    fn on_response(_id: i32, _ok: bool, _data: &[u8]) {}
    /// ui.show 表单的 submit/cancel 事件（JSON）
    fn on_ui_event(_json: &str) {}
}

impl<T: QueryHandler> PluginHandler for T {
    fn handle(text: &str, now_secs: i64) -> Vec<Row> {
        <T as QueryHandler>::handle(text, now_secs)
    }
}

/// 从共享区绝对地址读回字符串（宿主写入的协议约定；宏使用）
#[doc(hidden)]
pub fn read_abs(ptr_abs: i32, len: i32) -> String {
    unsafe {
        let base = &raw const __HEAP as usize;
        let ptr = ptr_abs as i64 - base as i64;
        if ptr < 0 || len < 0 || (ptr as usize + len as usize) > __HEAP.len() {
            return String::new();
        }
        String::from_utf8_lossy(&__HEAP[ptr as usize..ptr as usize + len as usize]).to_string()
    }
}

#[macro_export]
macro_rules! export_plugin {
    ($ty:ty) => {
        #[no_mangle]
        pub extern "C" fn query(ptr_abs: i32, len: i32, now_secs: i64) -> i32 {
            unsafe {
                let base = &raw const $crate::__HEAP as usize;
                let ptr = ptr_abs as i64 - base as i64;
                if ptr < 0 || (ptr as usize + len as usize) > $crate::__HEAP.len() {
                    $crate::__rows_store(vec![$crate::Row::new(
                        "✗ 协议错位",
                        &format!("ptr_abs={ptr_abs} base={base} len={len}"),
                        "",
                    )]);
                    return -2;
                }
                let text = String::from_utf8_lossy(
                    &$crate::__HEAP[ptr as usize..ptr as usize + len as usize],
                )
                .to_string();
                $crate::__rows_clear();
                // 写指针归零（BUG-006）：共享区此前只增不减，面板守护进程
                // 长期运行会把它耗尽、插件静默失效。归零安全：此刻查询文本
                // 已拷出；随后 handle() 的能力参数、结果读取的追加都写在
                // 归零后的区域，宿主读取发生在 query 返回之后，无活跃指针
                // 指向被覆盖区。
                $crate::__heap_reset();
                let rows = std::panic::catch_unwind(|| {
                    <$ty as $crate::PluginHandler>::handle(&text, now_secs)
                })
                .unwrap_or_else(|p| {
                    let msg = p
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| p.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "panic".into());
                    vec![$crate::Row::new("✗ 插件 panic", &msg, "")]
                });
                $crate::__rows_store(rows);
                0
            }
        }

        /// 结果行激活（W4）：宿主在用户回车时调用
        #[no_mangle]
        pub extern "C" fn activate(ptr_abs: i32, len: i32) -> i32 {
            let payload = $crate::read_abs(ptr_abs, len);
            let _ = std::panic::catch_unwind(|| {
                <$ty as $crate::PluginHandler>::activate(&payload)
            });
            0
        }

        /// 能力执行回包（W4）：ok != 0 为成功；data 在共享区
        #[no_mangle]
        pub extern "C" fn on_response(id: i32, ok: i32, ptr_abs: i32, len: i32) -> i32 {
            let data = $crate::read_abs(ptr_abs, len);
            let _ = std::panic::catch_unwind(|| {
                <$ty as $crate::PluginHandler>::on_response(id, ok != 0, data.as_bytes())
            });
            0
        }

        /// ui.show 表单事件（W4）：JSON 载荷
        #[no_mangle]
        pub extern "C" fn on_ui_event(ptr_abs: i32, len: i32) -> i32 {
            let json = $crate::read_abs(ptr_abs, len);
            let _ = std::panic::catch_unwind(|| {
                <$ty as $crate::PluginHandler>::on_ui_event(&json)
            });
            0
        }
    };
}

// 内部：宏展开需要的符号（导出给宿主的 result_* 协议在此统一实现）
#[doc(hidden)]
#[no_mangle]
pub extern "C" fn __rows_clear() {
    rows().clear();
}

#[doc(hidden)]
pub fn __rows_store(new_rows: Vec<Row>) {
    *rows() = new_rows;
}

/// 内部：共享区写指针归零（export_plugin! 的 query 导出在每次查询开头
/// 调用；BUG-006——见宏内注释）
#[doc(hidden)]
pub fn __heap_reset() {
    unsafe { HEAP_LEN = 0 };
}

#[doc(hidden)]
#[no_mangle]
pub extern "C" fn result_count() -> i32 {
    rows().len() as i32
}

#[doc(hidden)]
#[no_mangle]
pub extern "C" fn result_field_len(i: i32, field: i32) -> i32 {
    let rows = rows();
    let Some(row) = rows.get(i as usize) else { return 0 };
    match field {
        0 => row.title.len() as i32,
        1 => row.subtitle.len() as i32,
        _ => row.payload.len() as i32,
    }
}

#[doc(hidden)]
#[no_mangle]
pub extern "C" fn result_field(i: i32, field: i32) -> i32 {
    let rows = rows();
    let Some(row) = rows.get(i as usize) else { return -1 };
    let bytes = match field {
        0 => row.title.as_bytes(),
        1 => row.subtitle.as_bytes(),
        _ => row.payload.as_bytes(),
    };
    unsafe {
        let start = HEAP_LEN;
        let end = HEAP_LEN + bytes.len();
        if end > HEAP_CAP {
            return -1;
        }
        __HEAP[start..end].copy_from_slice(bytes);
        HEAP_LEN = end;
        (&raw const __HEAP as usize + start as usize) as i32
    }
}

/// 内部 HEAP 符号（宏使用）；不导出给宿主
#[doc(hidden)]
pub static mut __HEAP: [u8; HEAP_CAP] = [0u8; HEAP_CAP];
pub static mut HEAP_LEN: usize = 0;

/// 内部 HEAP 写入（宿主 write 走线性内存，插件侧只需 alloc 记账）
#[doc(hidden)]
pub fn __heap_alloc(size: i32) -> i32 {
    unsafe {
        let start = HEAP_LEN as i32;
        let new_len = HEAP_LEN + size as usize;
        if new_len > HEAP_CAP {
            return -1;
        }
        HEAP_LEN = new_len;
        (&raw const __HEAP as usize + start as usize) as i32
    }
}

// 让 alloc 走 __heap_alloc（统一记账）
#[no_mangle]
pub extern "C" fn alloc(size: i32) -> i32 {
    __heap_alloc(size)
}

