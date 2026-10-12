//! W2 正式集成：cronkit.wasm 完整功能通过宿主 Linker 驱动。
//!
//! 内存协议：
//! - 插件导出 `alloc(size) -> ptr`、`memory_size() -> len`
//! - 宿主写查询文本进 HEAP → 调 `query(ptr, len, now_secs) -> status`
//! - 宿主按 `result_count()` / `result_field(i, field)` / `result_field_len(i, field)`
//!   逐行取回结果（字段写进 HEAP 追加区，返回偏移）

use wasmtime::{Engine, Linker, Module, Store};

#[derive(Default)]
struct HostState;

/// 驱动 cronkit.wasm：返回结果行 (title, subtitle, payload)
pub fn run_cronkit_wasm(
    wasm_path: &str,
    query_text: &str,
    now_secs: i64,
) -> Result<Vec<(String, String, String)>, String> {
    let engine = Engine::default();
    let module =
        Module::from_file(&engine, wasm_path).map_err(|e| format!("加载失败：{e}"))?;
    let mut store = Store::new(&engine, HostState);
    let linker = Linker::new(&engine);
    let instance = linker
        .instantiate(&mut store, &module)
        .map_err(|e| format!("实例化失败：{e}"))?;

    let memory = instance
        .get_memory(&mut store, "memory")
        .ok_or("无 memory 导出")?;
    let alloc = instance
        .get_typed_func::<i32, i32>(&mut store, "alloc")
        .map_err(|e| format!("无 alloc：{e}"))?;
    let query_fn = instance
        .get_typed_func::<(i32, i32, i64), i32>(&mut store, "query")
        .map_err(|e| format!("无 query：{e}"))?;
    let result_count = instance
        .get_typed_func::<(), i32>(&mut store, "result_count")
        .map_err(|e| format!("无 result_count：{e}"))?;
    let result_field = instance
        .get_typed_func::<(i32, i32), i32>(&mut store, "result_field")
        .map_err(|e| format!("无 result_field：{e}"))?;
    let result_field_len = instance
        .get_typed_func::<(i32, i32), i32>(&mut store, "result_field_len")
        .map_err(|e| format!("无 result_field_len：{e}"))?;

    // 写查询文本（alloc 返回线性内存绝对地址；宿主全程只认绝对地址，
    // 数组内偏移 ↔ 绝对地址换算在插件内部完成）
    let qbytes = query_text.as_bytes();
    let qptr = alloc
        .call(&mut store, qbytes.len() as i32)
        .map_err(|e| e.to_string())?;
    memory
        .write(&mut store, qptr as usize, qbytes)
        .map_err(|e| e.to_string())?;

    // 执行查询（status = 插件返回码，负数 = 解析失败）
    query_fn
        .call(&mut store, (qptr, qbytes.len() as i32, now_secs))
        .map_err(|e| e.to_string())?;

    // 取回结果
    let mut out = Vec::new();
    let n = result_count.call(&mut store, ()).map_err(|e| e.to_string())?;
    for i in 0..n {
        let mut fields = [const { String::new() }; 3];
        for (f, field) in fields.iter_mut().enumerate() {
            let len = result_field_len
                .call(&mut store, (i, f as i32))
                .map_err(|e| e.to_string())?;
            if len <= 0 {
                continue;
            }
            // result_field 会把字符串追加到 HEAP 尾部并返回起始偏移——
            // 必须调用后立刻按返回的偏移读取
            let off = result_field
                .call(&mut store, (i, f as i32))
                .map_err(|e| e.to_string())?;
            let mut buf = vec![0u8; len as usize];
            memory
                .read(&store, off as usize, &mut buf)
                .map_err(|e| e.to_string())?;
            *field = String::from_utf8_lossy(&buf).to_string();
        }
        out.push((fields[0].clone(), fields[1].clone(), fields[2].clone()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wasm() -> String {
        // 测试 cwd 不确定：从 CARGO_MANIFEST_DIR 反推仓库根
        let manifest = env!("CARGO_MANIFEST_DIR");
        format!("{manifest}/../../plugins/cronkit/wasm/target/wasm32-unknown-unknown/release/cronkit_wasm.wasm")
    }

    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn passgen_wasm_generates() {
        let manifest = env!("CARGO_MANIFEST_DIR");
        let path = format!(
            "{manifest}/../../plugins/passgen/wasm/target/wasm32-unknown-unknown/release/passgen_wasm.wasm"
        );
        if !std::path::Path::new(&path).exists() {
            eprintln!("跳过：passgen_wasm.wasm 未构建");
            return;
        }
        let rows = run_cronkit_wasm(&path, "pass", 1_800_000_000).unwrap();
        assert!(rows.len() == 3, "3 行，实际 {}", rows.len());
        for r in &rows {
            assert_eq!(r.0.len(), 16, "{}", r.0);
            assert!(r.0.chars().any(|c| c.is_ascii_digit()), "{}", r.0);
            assert!(r.0.chars().any(|c| "!@#$%^&*".contains(c)), "{}", r.0);
        }
        eprintln!("passgen.wasm ✓ 3×16 位");
    }

    #[test]
    fn sqlfmt_wasm_formats() {
        let manifest = env!("CARGO_MANIFEST_DIR");
        let path = format!(
            "{manifest}/../../plugins/sqlfmt/wasm/target/wasm32-unknown-unknown/release/sqlfmt_wasm.wasm"
        );
        if !std::path::Path::new(&path).exists() {
            eprintln!("跳过：sqlfmt_wasm.wasm 未构建");
            return;
        }
        let rows = run_cronkit_wasm(
            &path,
            "fmt\x1fselect id, name from users where age > 18 and name like 'a%'",
            1,
        )
        .unwrap();
        let sql = &rows[0].2;
        assert!(
            sql.starts_with("SELECT id,\n  name\nFROM users\nWHERE age > 18\n  AND name LIKE"),
            "{sql:?}"
        );
        let rows = run_cronkit_wasm(&path, "compact\x1fselect  a ,  b from t", 1).unwrap();
        assert_eq!(rows[0].2, "select a, b from t");
        eprintln!("sqlfmt.wasm ✓ 格式化+压缩");
    }

    #[test]
    fn randgen_wasm_generates() {
        let manifest = env!("CARGO_MANIFEST_DIR");
        let path = format!(
            "{manifest}/../../plugins/randgen/wasm/target/wasm32-unknown-unknown/release/randgen_wasm.wasm"
        );
        if !std::path::Path::new(&path).exists() {
            eprintln!("跳过：randgen_wasm.wasm 未构建");
            return;
        }
        let rows = run_cronkit_wasm(&path, "rand 5-20", 1_800_000_000).unwrap();
        assert!(rows.len() == 5, "5 行，实际 {}", rows.len());
        for r in &rows {
            let n: i64 = r.0.parse().unwrap_or(-1);
            assert!((5..=20).contains(&n), "范围外：{}", r.0);
        }
        // uuid 直达
        let rows = run_cronkit_wasm(&path, "uuid", 1_800_000_000).unwrap();
        assert!(rows[0].0.len() == 36 && rows[0].0.as_bytes()[14] == b'4', "{}", rows[0].0);
        eprintln!("randgen.wasm ✓ 范围+UUID");
    }

    #[test]
    fn csvkit_wasm_converts() {
        let manifest = env!("CARGO_MANIFEST_DIR");
        let path = format!(
            "{manifest}/../../plugins/csvkit/wasm/target/wasm32-unknown-unknown/release/csvkit_wasm.wasm"
        );
        if !std::path::Path::new(&path).exists() {
            eprintln!("跳过：csvkit_wasm.wasm 未构建");
            return;
        }
        // csvkit 协议：mode \x1f delim \x1f header \x1f text
        let query = "c2j\x1fauto\x1f1\x1fname,age\nalice,30\nbob,25";
        let rows = run_cronkit_wasm(&path, query, 1_800_000_000).unwrap();
        eprintln!("[csvkit-row] {}", rows[0].1);
        assert!(rows[0].0.starts_with("✓ JSON"), "{}", rows[0].0);
        assert!(rows[0].2.contains("\"alice\""), "{}", rows[0].2);
        // 反向
        let query2 = "j2c\x1fauto\x1f1\x1f[{\"name\":\"alice\",\"age\":\"30\"}]";
        let rows = run_cronkit_wasm(&path, query2, 1_800_000_000).unwrap();
        assert!(rows[0].0.starts_with("✓ CSV"), "{}", rows[0].0);
        assert!(rows[0].2.contains("alice,30"), "{}", rows[0].2);
        eprintln!("csvkit.wasm 双向转换 ✓");
    }

    #[test]
    fn cronkit_wasm_full_pipeline() {
        if !std::path::Path::new(&wasm()).exists() {
            eprintln!("跳过：cronkit_wasm.wasm 未构建");
            return;
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let rows = run_cronkit_wasm(&wasm(), "cron 0 9 * * 1-5", now).unwrap();
        assert!(rows.len() == 6, "解读 1 行 + 预览 5 行，实际 {}", rows.len());
        assert!(rows[0].0.starts_with("📋"), "解读行：{}", rows[0].0);
        assert!(rows[0].0.contains("周一"), "应含周一：{}", rows[0].0);
        // 预览行格式
        assert!(rows[1].2.ends_with("09:00:00"), "payload：{}", rows[1].2);
        eprintln!("cronkit.wasm 结果：");
        for r in &rows {
            eprintln!("  {} | {} | {}", r.0, r.1, r.2);
        }
    }

    #[test]
    fn cronkit_wasm_rejects_bad_expr() {
        if !std::path::Path::new(&wasm()).exists() {
            return;
        }
        let now = 1_800_000_000;
        let rows = run_cronkit_wasm(&wasm(), "cron 0 9 * * 99", now).unwrap();
        assert!(rows[0].0.starts_with("✗"), "应报错误行：{}", rows[0].0);
    }

    #[test]
    fn cronkit_wasm_ignores_foreign_query() {
        if !std::path::Path::new(&wasm()).exists() {
            return;
        }
        let rows = run_cronkit_wasm(&wasm(), "hello world", 1_800_000_000).unwrap();
        assert!(rows.is_empty(), "非本插件查询应静默：{rows:?}");
    }
}
