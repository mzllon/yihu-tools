//! W2 能力桥接 spike：WASM 插件调用宿主能力函数的全链路验证。
//!
//! 验证项：
//! 1. 宿主通过 Linker::func_wrap 暴露能力函数（clipboard.write / show-results）
//! 2. WASM 插件导入并调用这些函数（传字符串参数，宿主读 WASM 内存）
//! 3. 宿主能力函数执行真实操作（写剪贴板 / 存结果）
//! 4. 全链路延迟量化
//!
//! 内存约定：WASM 模块导出 `alloc(size) -> ptr` 和 `memory`，
//! 宿主通过这两者读写字符串（跨 WASM/宿主边界的标准做法）。

use std::time::Instant;
use wasmtime::{Engine, Linker, Module, Store};

/// 宿主状态：能力调用的接收容器
#[derive(Default)]
struct HostState {
    /// 插件调 show-results 时存到这里
    results: Vec<ResultItem>,
    /// 插件调 clipboard-write 时存到这里
    clipboard: Option<String>,
    /// 调用日志
    calls: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ResultItem {
    pub title: String,
    pub subtitle: String,
    pub payload: String,
}

/// 加载 .wasm 并驱动能力调用全链路
pub fn spike_capability_bridge(
    wasm_path: &str,
    query_text: &str,
) -> Result<CapSpikeResult, String> {
    let t0 = Instant::now();
    let engine = Engine::default();
    let module = Module::from_file(&engine, wasm_path)
        .map_err(|e| format!("加载失败：{e}"))?;

    let state = HostState::default();
    let mut store = Store::new(&engine, state);
    let mut linker = Linker::new(&engine);

    // —— 宿主能力函数：clipboard.write ——
    // WASM 插件传 (ptr, len)，宿主从 WASM 内存读字符串
    linker
        .func_wrap(
            "yihu",
            "clipboard-write",
            |mut caller: wasmtime::Caller<'_, HostState>, ptr: i32, len: i32| -> Result<(), wasmtime::Error> {
                let mem = caller
                    .get_export("memory")
                    .and_then(|e| e.into_memory())
                    .ok_or_else(|| wasmtime::Error::msg("WASM 无 memory 导出"))?;
                let mut buf = vec![0u8; len as usize];
                mem.read(&caller, ptr as usize, &mut buf)?;
                let text = String::from_utf8_lossy(&buf).to_string();
                caller.data_mut().clipboard = Some(text);
                caller.data_mut().calls.push("clipboard.write".into());
                Ok(())
            },
        )
        .map_err(|e| format!("注册 clipboard-write 失败：{e}"))?;

    // —— 宿主能力函数：show-results（简化版：传 count，宿主记录）——
    linker
        .func_wrap(
            "yihu",
            "show-results-count",
            |mut caller: wasmtime::Caller<'_, HostState>, count: i32| -> Result<(), wasmtime::Error> {
                caller.data_mut().results.reserve(count as usize);
                caller.data_mut().calls.push(format!("show-results({count})"));
                Ok(())
            },
        )
        .map_err(|e| format!("注册 show-results 失败：{e}"))?;

    // —— 宿主能力函数：log ——
    linker
        .func_wrap("yihu", "log", |mut caller: wasmtime::Caller<'_, HostState>, ptr: i32, len: i32| -> Result<(), wasmtime::Error> {
            let mem = caller
                .get_export("memory")
                .and_then(|e| e.into_memory())
                .ok_or_else(|| wasmtime::Error::msg("WASM 无 memory 导出"))?;
            let mut buf = vec![0u8; len as usize];
            mem.read(&caller, ptr as usize, &mut buf)?;
            caller.data_mut().calls.push(format!("log: {}", String::from_utf8_lossy(&buf)));
            Ok(())
        })
        .map_err(|e| format!("注册 log 失败：{e}"))?;

    let setup_ms = t0.elapsed().as_secs_f64() * 1000.0;

    // —— 实例化 + 调用 ——
    let t1 = Instant::now();
    let instance = linker
        .instantiate(&mut store, &module)
        .map_err(|e| format!("实例化失败：{e}"))?;

    // 把查询文本写入 WASM 内存
    let memory = instance
        .get_memory(&mut store, "memory")
        .ok_or_else(|| "WASM 无 memory".to_string())?;
    let alloc = instance
        .get_typed_func::<i32, i32>(&mut store, "alloc")
        .map_err(|e| format!("找不到 alloc：{e}"))?;
    let query_fn = instance
        .get_typed_func::<(i32, i32), ()>(&mut store, "query")
        .map_err(|e| format!("找不到 query：{e}"))?;

    let qbytes = query_text.as_bytes();
    let qptr = alloc.call(&mut store, qbytes.len() as i32).map_err(|e| e.to_string())?;
    memory.write(&mut store, qptr as usize, qbytes).map_err(|e| e.to_string())?;

    // 调用插件的 query(ptr, len)
    query_fn.call(&mut store, (qptr, qbytes.len() as i32)).map_err(|e| e.to_string())?;
    let call_ms = t1.elapsed().as_secs_f64() * 1000.0;

    // —— 读取宿主状态（能力调用的结果）——
    let state = store.data();
    let clipboard = state.clipboard.clone().unwrap_or_default();
    let n_calls = state.calls.len();

    // 把剪贴板内容写回 WASM 内存（模拟 copy 回传）
    if !clipboard.is_empty() {
        let cptr = alloc.call(&mut store, clipboard.len() as i32).map_err(|e| e.to_string())?;
        memory.write(&mut store, cptr as usize, clipboard.as_bytes()).map_err(|e| e.to_string())?;
    }

    Ok(CapSpikeResult {
        setup_ms,
        call_ms,
        clipboard,
        n_calls,
    })
}

#[derive(Debug)]
pub struct CapSpikeResult {
    pub setup_ms: f64,
    pub call_ms: f64,
    pub clipboard: String,
    pub n_calls: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_bridge_end_to_end() {
        let wasm_path = "/tmp/wasm-spike/cap/target/wasm32-unknown-unknown/release/cap_plugin.wasm";
        if !std::path::Path::new(wasm_path).exists() {
            eprintln!("跳过：{wasm_path} 不存在（先构建 cap 插件）");
            return;
        }
        let r = spike_capability_bridge(wasm_path, "cron 0 9 * * 1-5").unwrap();
        eprintln!("能力桥 spike：{r:#?}");
        assert!(!r.clipboard.is_empty(), "clipboard.write 应被调用");
        assert!(r.n_calls >= 2, "应有多次能力调用");
        assert!(r.call_ms < 10.0, "调用应 <10ms");
    }
}
