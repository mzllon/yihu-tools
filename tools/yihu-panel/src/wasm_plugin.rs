//! WASM 插件运行时（M5 追加，架构终版 §4）：wasmtime 后端。
//!
//! manifest `entry` 以 `.wasm` 结尾的插件走本后端：wasmtime 实例化 +
//! Linker 函数桥驱动（协议见 crates/yihu-wasm-spike/src/integration.rs 的
//! cronkit.wasm 先例）。与 bwrap+stdio 双轨并存——按 entry 后缀路由。
//!
//! 性能红线（架构终版）：编译产物磁盘缓存（首次 231ms → 缓存 0.4ms）、
//! 实例随面板生命周期复用、查询调用 ~0.2ms。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use wasmtime::{Engine, Linker, Module, Store};

/// 面板结果行（与 app::PanelEntry 字段对齐，避免循环依赖用自有类型）
pub struct WasmRow {
    pub title: String,
    pub subtitle: String,
    pub payload: String,
}

/// 一个 WASM 插件的运行实例（跨查询复用，模块编译一次）
pub struct WasmPlugin {
    #[allow(dead_code)]
    engine: Engine,
    store: Store<HostState>,
    instance: wasmtime::Instance,
    /// 上次使用（空闲回收依据；v1 常驻面板进程）
    last_used: Instant,
    /// 查询文本暂存（结果行带出，供 payload 展示）
    last_query: String,
}

#[derive(Default)]
struct HostState;

impl WasmPlugin {
    /// 从 manifest 目录加载 entry.wasm。模块编译产物缓存到
    /// <wasm 同目录>/.<name>.cwasm（首次编译 ~200ms → 缓存 <1ms）。
    pub fn load(wasm_path: &Path) -> Result<WasmPlugin, String> {
        let engine = Engine::default();

        // 编译缓存：wasmtime 序列化产物与源 wasm 同目录
        let cache_path: PathBuf = {
            let stem = wasm_path.with_extension("");
            let name = stem
                .file_name()
                .map(|s| format!(".{}.cwasm", s.to_string_lossy()))
                .unwrap_or_else(|| ".plugin.cwasm".into());
            stem.with_file_name(name)
        };
        let module = match std::fs::read(&cache_path) {
            Ok(bytes) => unsafe {
                // SAFETY：缓存产物由同版本 wasmtime 序列化；版本不匹配会
                // 反序列化失败，落到重新编译
                Module::deserialize(&engine, &bytes)
                    .or_else(|_| Module::from_file(&engine, wasm_path))
                    .map_err(|e| format!("加载 {wasm_path:?}：{e}"))?
            },
            Err(_) => {
                let m = Module::from_file(&engine, wasm_path)
                    .map_err(|e| format!("加载 {wasm_path:?}：{e}"))?;
                if let Ok(serialized) = m.serialize() {
                    let _ = std::fs::write(&cache_path, serialized);
                }
                m
            }
        };

        let mut store = Store::new(&engine, HostState);
        let linker = Linker::new(&engine);
        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|e| format!("实例化失败：{e}"))?;
        // 启动探测：确保导出齐全（fail-fast 装载期，不留半成品）
        instance
            .get_typed_func::<(), i32>(&mut store, "result_count")
            .map_err(|e| format!("缺 result_count 导出：{e}"))?;
        Ok(WasmPlugin {
            engine,
            store,
            instance,
            last_used: Instant::now(),
            last_query: String::new(),
        })
    }

    /// 执行一次查询，返回结果行。
    /// 内存协议（同 cronkit.wasm）：宿主写文本 → query(ptr,len,now) →
    /// result_count/result_field 取回。offset 均为线性内存绝对地址。
    pub fn query(&mut self, text: &str) -> Result<Vec<WasmRow>, String> {
        self.last_used = Instant::now();
        self.last_query = text.to_string();
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let memory = self
            .instance
            .get_memory(&mut self.store, "memory")
            .ok_or("无 memory 导出")?;
        let alloc = self
            .instance
            .get_typed_func::<i32, i32>(&mut self.store, "alloc")
            .map_err(|e| format!("无 alloc：{e}"))?;
        let query_fn = self
            .instance
            .get_typed_func::<(i32, i32, i64), i32>(&mut self.store, "query")
            .map_err(|e| format!("无 query：{e}"))?;
        let result_count = self
            .instance
            .get_typed_func::<(), i32>(&mut self.store, "result_count")
            .map_err(|e| format!("无 result_count：{e}"))?;
        let result_field = self
            .instance
            .get_typed_func::<(i32, i32), i32>(&mut self.store, "result_field")
            .map_err(|e| format!("无 result_field：{e}"))?;
        let result_field_len = self
            .instance
            .get_typed_func::<(i32, i32), i32>(&mut self.store, "result_field_len")
            .map_err(|e| format!("无 result_field_len：{e}"))?;

        let qbytes = text.as_bytes();
        let qptr = alloc
            .call(&mut self.store, qbytes.len() as i32)
            .map_err(|e| e.to_string())?;
        if qptr < 0 {
            return Err("WASM 共享区溢出".into());
        }
        memory
            .write(&mut self.store, qptr as usize, qbytes)
            .map_err(|e| e.to_string())?;
        query_fn
            .call(&mut self.store, (qptr, qbytes.len() as i32, now_secs))
            .map_err(|e| format!("query 执行失败：{e}"))?;

        let n = result_count.call(&mut self.store, ()).map_err(|e| e.to_string())?;
        let mut rows = Vec::new();
        for i in 0..n {
            let mut fields = [const { String::new() }; 3];
            for (f, field) in fields.iter_mut().enumerate() {
                let len = result_field_len
                    .call(&mut self.store, (i, f as i32))
                    .map_err(|e| e.to_string())?;
                if len <= 0 {
                    continue;
                }
                // result_field 把字符串追加到共享区并返回绝对地址，须立刻读
                let off = result_field
                    .call(&mut self.store, (i, f as i32))
                    .map_err(|e| e.to_string())?;
                let mut buf = vec![0u8; len as usize];
                memory
                    .read(&self.store, off as usize, &mut buf)
                    .map_err(|e| e.to_string())?;
                *field = String::from_utf8_lossy(&buf).to_string();
            }
            rows.push(WasmRow {
                title: fields[0].clone(),
                subtitle: fields[1].clone(),
                payload: fields[2].clone(),
            });
        }
        Ok(rows)
    }

    pub fn last_used(&self) -> Instant {
        self.last_used
    }
}

/// 全部已加载 WASM 插件（id → 实例）
#[derive(Default)]
pub struct WasmRegistry {
    plugins: HashMap<String, WasmPlugin>,
}

impl WasmRegistry {
    /// entry 以 .wasm 结尾则加载（幂等：已加载跳过），返回是否属于本后端
    pub fn ensure_loaded(&mut self, id: &str, entry_path: &Path) -> Result<bool, String> {
        if entry_path.extension().is_none_or(|e| e != "wasm") {
            return Ok(false);
        }
        if self.plugins.contains_key(id) {
            return Ok(true);
        }
        let plugin = WasmPlugin::load(entry_path)?;
        self.plugins.insert(id.to_string(), plugin);
        Ok(true)
    }

    /// 查询；插件未加载时静默返回空（加载失败在 ensure 阶段已报）
    pub fn query(&mut self, id: &str, text: &str) -> Option<Vec<WasmRow>> {
        self.plugins.get_mut(id)?.query(text).ok()
    }

    pub fn is_loaded(&self, id: &str) -> bool {
        self.plugins.contains_key(id)
    }

    /// 全部 WASM 插件广播查询：返回 (id, rows) 列表（空结果 = 插件自认
    /// 词头不匹配，调用方直接丢弃）
    pub fn broadcast(&mut self, text: &str) -> Vec<(String, Vec<WasmRow>)> {
        self.plugins
            .iter_mut()
            .filter_map(|(id, p)| p.query(text).ok().map(|rows| (id.clone(), rows)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wasm_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../plugins/cronkit/wasm/target/wasm32-unknown-unknown/release/cronkit_wasm.wasm")
    }

    #[test]
    fn loads_and_queries_cronkit() {
        let p = wasm_path();
        if !p.exists() {
            eprintln!("跳过：cronkit.wasm 未构建");
            return;
        }
        let mut reg = WasmRegistry::default();
        assert!(reg.ensure_loaded("cronkit", &p).unwrap());
        let rows = reg.query("cronkit", "cron 0 9 * * 1-5").unwrap();
        assert!(rows.len() == 6, "解读+5 预览，实际 {}", rows.len());
        assert!(rows[0].title.starts_with("📋"), "{}", rows[0].title);
        // 二次查询（复用实例）
        let rows = reg.query("cronkit", "cron @daily").unwrap();
        assert!(rows.len() == 6 && rows[0].title.contains("每天"));
        // 非本插件查询静默
        assert!(reg.query("cronkit", "hello world").unwrap().is_empty());
    }

    #[test]
    fn non_wasm_entry_rejected() {
        let mut reg = WasmRegistry::default();
        assert!(!reg.ensure_loaded("x", Path::new("/tmp/nothing.py")).unwrap());
    }
}
