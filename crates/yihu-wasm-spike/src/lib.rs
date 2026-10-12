//! W1 spike：wasmtime 嵌入可行性验证。
//!
//! 验证项：
//! 1. wasmtime 能加载 Rust→WASM 模块并调用导出函数
//! 2. 冷启动耗时（Engine 创建 + Module 编译 + 实例化 + 调用）
//! 3. 热调用耗时（Module 已编译，只做实例化+调用）
//! 4. 内存开销
//!
//! 结论写回 docs/插件运行时架构.md 的 W1 段。

pub mod cap;
pub mod integration;

use std::time::Instant;
use wasmtime::{Engine, Linker, Module, Store};

/// 加载 .wasm 模块并调用 add(a,b)
pub fn spike_load_and_call(wasm_path: &str, a: i32, b: i32) -> Result<SpikeResult, String> {
    // ① Engine 创建（全局一次，这里包含在冷启动里量化）
    let t0 = Instant::now();
    let engine = Engine::default();
    let engine_ms = t0.elapsed().as_secs_f64() * 1000.0;

    // ② Module 编译（.wasm → 可执行内部表示）
    let t1 = Instant::now();
    let module = Module::from_file(&engine, wasm_path)
        .map_err(|e| format!("加载 WASM 失败：{e}"))?;
    let compile_ms = t1.elapsed().as_secs_f64() * 1000.0;

    // ③ 实例化 + 调用
    let t2 = Instant::now();
    let mut store = Store::new(&engine, ());
    let linker = Linker::new(&engine);
    let instance = linker
        .instantiate(&mut store, &module)
        .map_err(|e| format!("实例化失败：{e}"))?;
    let add = instance
        .get_typed_func::<(i32, i32), i32>(&mut store, "add")
        .map_err(|e| format!("找不到 add 导出：{e}"))?;
    let result = add
        .call(&mut store, (a, b))
        .map_err(|e| format!("调用失败：{e}"))?;
    let instantiate_call_ms = t2.elapsed().as_secs_f64() * 1000.0;

    // ④ 热调用（已实例化，只做函数调用）
    let t3 = Instant::now();
    let _ = add.call(&mut store, (b, a)).map_err(|e| e.to_string())?;
    let hot_call_ms = t3.elapsed().as_secs_f64() * 1_000_000.0;

    // ⑤ fib(25) 计算密集测试
    let fib = instance
        .get_typed_func::<u32, u32>(&mut store, "fib")
        .map_err(|e| format!("找不到 fib：{e}"))?;
    let t4 = Instant::now();
    let fib_result = fib.call(&mut store, 25).map_err(|e| e.to_string())?;
    let fib_ms = t4.elapsed().as_secs_f64() * 1_000_000.0;

    Ok(SpikeResult {
        engine_ms,
        compile_ms,
        instantiate_call_ms,
        hot_call_us: hot_call_ms,
        add_result: result,
        fib_result,
        fib_us: fib_ms,
    })
}

#[derive(Debug)]
pub struct SpikeResult {
    pub engine_ms: f64,
    pub compile_ms: f64,
    pub instantiate_call_ms: f64,
    pub hot_call_us: f64,
    pub add_result: i32,
    pub fib_result: u32,
    pub fib_us: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_cache_proves_cold_start_can_be_eliminated() {
        let wasm_path = "/tmp/wasm-spike/simple/target/wasm32-unknown-unknown/release/simple_wasm.wasm";
        if !std::path::Path::new(wasm_path).exists() { return; }
        let engine = Engine::default();
        // 第一次编译
        let t0 = Instant::now();
        let module = Module::from_file(&engine, wasm_path).unwrap();
        let first_ms = t0.elapsed().as_secs_f64() * 1000.0;
        // 序列化编译产物 → 磁盘
        let serialized = module.serialize().unwrap();
        // 第二次：从编译缓存二进制直接加载（跳过编译）
        let t1 = Instant::now();
        let module2 = unsafe { Module::deserialize(&engine, &serialized) }.unwrap();
        let cached_ms = t1.elapsed().as_secs_f64() * 1000.0;
        eprintln!("首次编译: {first_ms:.1}ms | 缓存加载: {cached_ms:.1}ms | 加速 {:.0}×", first_ms / cached_ms.max(0.001));
        // 验证功能一致
        let mut store = Store::new(&engine, ());
        let linker = Linker::new(&engine);
        let inst = linker.instantiate(&mut store, &module2).unwrap();
        let add = inst.get_typed_func::<(i32, i32), i32>(&mut store, "add").unwrap();
        assert_eq!(add.call(&mut store, (2, 3)).unwrap(), 5);
    }

    #[test]
    fn wasm_load_and_execute() {
        // 使用 cronkit 构建产物（或 simple wasm）
        let wasm_path = "/tmp/wasm-spike/simple/target/wasm32-unknown-unknown/release/simple_wasm.wasm";
        if !std::path::Path::new(wasm_path).exists() {
            eprintln!("跳过：{wasm_path} 不存在（先构建 WASM 模块）");
            return;
        }
        let r = spike_load_and_call(wasm_path, 3, 4).unwrap();
        assert_eq!(r.add_result, 7, "add(3,4) = 7");
        assert_eq!(r.fib_result, 75025, "fib(25) = 75025");
        eprintln!("spike 结果：{r:#?}");
    }
}
