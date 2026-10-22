//! WASM 插件运行时（M5 追加，架构终版 §4）：wasmtime 后端。
//!
//! manifest `entry` 以 `.wasm` 结尾的插件走本后端：wasmtime 实例化 +
//! Linker 函数桥驱动。与 bwrap+stdio 双轨并存——按 entry 后缀路由。
//!
//! W4 能力面：每个宿主能力 = 一个独立导入函数（模块 `yihu`，kebab 名，
//! 统一 7 参签名）。**能力即安全**：装载期枚举模块 imports，导入未声明
//! 能力的模块直接拒载；请求期再过 `caps::evaluate`（同一套词表/声明/
//! 限额，与 stdio 轨共用）。调用模型与 stdio 的 capability_request 一致：
//! 导入只入队即返回（执行在 GTK/后台线程，绝不阻塞 WASM 调用栈——硬性
//! 规则 #1），结果经插件导出的 `on_response(id, ok, ptr, len)` 回传。
//!
//! 性能红线（架构终版）：编译产物磁盘缓存（首次 ~231ms → 缓存 ~0.4ms）、
//! 实例随面板生命周期复用、查询调用 ~0.2ms。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use wasmtime::{Caller, Engine, Linker, Module, Store};
use yihu_core::permissions as perms;

/// 单次呼出周期内每插件能力请求上限（与 stdio 轨一致）
const MAX_PENDING: usize = 64;
/// ui.show 并发上限（表单是主线程重资源）
const MAX_UIS: usize = 8;
/// 回包载荷上限：共享区 1MiB 与查询协议共用，留余量
const MAX_RESPONSE_BYTES: usize = 768 * 1024;

/// 面板结果行（与 app::PanelEntry 字段对齐，避免循环依赖用自有类型）
#[derive(Debug)]
pub struct WasmRow {
    pub title: String,
    pub subtitle: String,
    pub payload: String,
}

/// drain 出的能力请求（PluginMgr 转 sessions::CapRequest 分发）
pub struct WasmCapReq {
    pub request_id: u64,
    pub capability: &'static str,
    pub params: serde_json::Value,
}

/// drain 出的整套挂起事项
#[derive(Default)]
pub struct WasmDrain {
    pub caps: Vec<WasmCapReq>,
    /// (ui_id, ui.show spec JSON)
    pub uis: Vec<(u64, serde_json::Value)>,
}

/// (导入名, 能力名)。settings-reset 与 settings-write 同能力不同 op。
const IMPORTS: &[(&str, &str)] = &[
    ("clipboard-write", perms::CLIPBOARD_WRITE),
    ("clipboard-read", perms::CLIPBOARD_READ),
    ("open-uri", perms::OPEN_URI),
    ("launch-app", perms::LAUNCH_APP),
    ("notify", perms::NOTIFY),
    ("screenshot-take", perms::SCREENSHOT_TAKE),
    ("network-fetch", perms::NETWORK_FETCH),
    ("settings-write", perms::SETTINGS_WRITE),
    ("settings-reset", perms::SETTINGS_WRITE),
    ("settings-read", perms::SETTINGS_READ),
    ("fs-write", perms::FS_WRITE),
    ("fs-read", perms::FS_READ),
    ("audio-play", perms::AUDIO_PLAY),
    ("audio-stop", perms::AUDIO_STOP),
    ("audio-volume", perms::AUDIO_VOLUME),
    ("qr-generate", perms::QR_GENERATE),
    ("qr-decode", perms::QR_DECODE),
    ("md-render", perms::MD_RENDER),
    ("md-view", perms::MD_VIEW),
    ("sticky-pin", perms::STICKY_PIN),
    ("sticky-close", perms::STICKY_CLOSE),
];

/// 非能力的辅助导入（宿主渲染 UI / 调试日志，不受 manifest 门禁）
const AUX_IMPORTS: &[&str] = &["ui-show", "log"];

/// 导入名 → evaluate() 的 params JSON（键序与 caps.rs 读取约定一致）
fn build_params(name: &str, a: &str, b: &str, c: &str) -> serde_json::Value {
    match name {
        "notify" => serde_json::json!({"summary": a, "body": b}),
        "screenshot-take" => serde_json::json!({"mode": a, "clipboard": !b.is_empty()}),
        "settings-write" => serde_json::json!({"schema": a, "key": b, "value": c, "op": "set"}),
        "settings-reset" => serde_json::json!({"schema": a, "key": b, "op": "reset"}),
        "settings-read" => serde_json::json!({"schema": a, "key": b}),
        "md-render" => serde_json::json!({"mode": a, "text": b}),
        "md-view" => serde_json::json!({"title": a, "text": b}),
        "fs-write" => serde_json::json!({"path": a, "text": b}),
        "sticky-pin" | "sticky-close" => serde_json::json!({"id": a}),
        "audio-volume" => serde_json::json!({"pct": a.parse::<u64>().map(serde_json::Value::from).unwrap_or(serde_json::Value::Null)}),
        "open-uri" => serde_json::json!({"uri": a}),
        "launch-app" => serde_json::json!({"desktop_id": a}),
        "network-fetch" | "audio-play" => serde_json::json!({"url": a}),
        "clipboard-write" | "qr-generate" => serde_json::json!({"text": a}),
        "fs-read" => serde_json::json!({"path": a}),
        // clipboard-read / qr-decode / audio-stop 无参
        _ => serde_json::json!({}),
    }
}

/// 宿主状态：能力/UI 请求的接收容器（宿主在调用返回后 drain）
#[derive(Default)]
struct HostState {
    plugin_id: String,
    caps: Vec<WasmCapReq>,
    uis: Vec<(u64, serde_json::Value)>,
    next_ui_id: u64,
}

/// 从 WASM 线性内存读字符串（ptr 为共享区绝对地址，len≤0 视为空）
fn read_mem(
    caller: &mut Caller<'_, HostState>,
    ptr: i32,
    len: i32,
) -> Result<String, wasmtime::Error> {
    if len <= 0 || ptr <= 0 {
        return Ok(String::new());
    }
    let mem = caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .ok_or_else(|| wasmtime::Error::msg("WASM 无 memory 导出"))?;
    let mut buf = vec![0u8; len as usize];
    mem.read(caller, ptr as usize, &mut buf)?;
    Ok(String::from_utf8_lossy(&buf).to_string())
}

/// 注册全部 yihu.* 导入（统一 7 参签名）。能力导入入队待 drain；
/// ui-show 入队待渲染；log 仅调试输出。
fn register_imports(linker: &mut Linker<HostState>) -> Result<(), wasmtime::Error> {
    for (name, capability) in IMPORTS {
        let name = *name;
        let capability = *capability;
        linker.func_wrap(
            "yihu",
            name,
            move |mut caller: Caller<'_, HostState>,
                  id: i32,
                  ap: i32,
                  al: i32,
                  bp: i32,
                  bl: i32,
                  cp: i32,
                  cl: i32|
                  -> Result<i32, wasmtime::Error> {
                let a = read_mem(&mut caller, ap, al)?;
                let b = read_mem(&mut caller, bp, bl)?;
                let c = read_mem(&mut caller, cp, cl)?;
                let state = caller.data_mut();
                if state.caps.len() >= MAX_PENDING {
                    return Ok(-1);
                }
                state.caps.push(WasmCapReq {
                    request_id: id.max(0) as u64,
                    capability,
                    params: build_params(name, &a, &b, &c),
                });
                Ok(0)
            },
        )?;
    }
    linker.func_wrap(
        "yihu",
        "ui-show",
        |mut caller: Caller<'_, HostState>,
         _id: i32,
         ap: i32,
         al: i32,
         _: i32,
         _: i32,
         _: i32,
         _: i32|
         -> Result<i32, wasmtime::Error> {
            let spec_text = read_mem(&mut caller, ap, al)?;
            let state = caller.data_mut();
            if state.uis.len() >= MAX_UIS {
                return Ok(-1);
            }
            match serde_json::from_str::<serde_json::Value>(&spec_text) {
                Ok(spec) => {
                    let ui_id = state.next_ui_id;
                    state.next_ui_id += 1;
                    state.uis.push((ui_id, spec));
                    Ok(0)
                }
                Err(_) => Ok(-2),
            }
        },
    )?;
    linker.func_wrap(
        "yihu",
        "log",
        |mut caller: Caller<'_, HostState>,
         _id: i32,
         ap: i32,
         al: i32,
         _: i32,
         _: i32,
         _: i32,
         _: i32|
         -> Result<i32, wasmtime::Error> {
            let msg = read_mem(&mut caller, ap, al)?;
            if std::env::var_os("YIHU_PANEL_DEBUG").is_some() {
                eprintln!("wasm[{}]: {msg}", caller.data().plugin_id);
            }
            Ok(0)
        },
    )?;
    Ok(())
}

/// 一个 WASM 插件的运行实例（跨查询复用，模块编译一次）
pub struct WasmPlugin {
    engine: Engine,
    store: Store<HostState>,
    instance: wasmtime::Instance,
    /// manifest 声明的权限（能力请求分发的授权依据）
    declared: std::sync::Arc<Vec<String>>,
    /// 上次使用（空闲回收依据；v1 常驻面板进程）
    last_used: Instant,
}

impl WasmPlugin {
    /// 从 manifest 目录加载 entry.wasm。模块编译产物缓存到
    /// <wasm 同目录>/.<name>.cwasm（首次编译 ~200ms → 缓存 <1ms）。
    /// `declared` 是 manifest permissions 原始串：装载期按它校验全部
    /// yihu.* 导入（能力即安全，未声明即拒载）。
    pub fn load(wasm_path: &Path, declared: &[String]) -> Result<WasmPlugin, String> {
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

        // 装载期能力校验（能力即安全）：imports ⊆ manifest 声明
        for imp in module.imports() {
            if imp.module() != "yihu" {
                return Err(format!(
                    "拒载 {wasm_path:?}：未知导入模块 {:?}（只允许 yihu）",
                    imp.module()
                ));
            }
            let name = imp.name();
            if AUX_IMPORTS.contains(&name) {
                continue;
            }
            let Some((_, cap)) = IMPORTS.iter().find(|(n, _)| *n == name) else {
                return Err(format!("拒载 {wasm_path:?}：未知的宿主导入 yihu.{name}"));
            };
            let declared_ok =
                perms::has_plain(declared, cap) || !perms::params_of(declared, cap).is_empty();
            if !declared_ok {
                return Err(format!(
                    "拒载 {wasm_path:?}：导入 yihu.{name} 需要 manifest 声明 {cap}，但未声明（declared={declared:?}）"
                ));
            }
        }

        let mut store = Store::new(
            &engine,
            HostState {
                plugin_id: String::new(),
                ..HostState::default()
            },
        );
        let mut linker = Linker::new(&engine);
        register_imports(&mut linker).map_err(|e| format!("注册宿主导入失败：{e}"))?;
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
            declared: std::sync::Arc::new(declared.to_vec()),
            last_used: Instant::now(),
        })
    }

    /// 向共享区写入字节，返回 (绝对地址, 长度)
    fn write_heap(&mut self, bytes: &[u8]) -> Result<(i32, i32), String> {
        let memory = self
            .instance
            .get_memory(&mut self.store, "memory")
            .ok_or("无 memory 导出")?;
        let alloc = self
            .instance
            .get_typed_func::<i32, i32>(&mut self.store, "alloc")
            .map_err(|e| format!("无 alloc：{e}"))?;
        let ptr = alloc
            .call(&mut self.store, bytes.len() as i32)
            .map_err(|e| e.to_string())?;
        if ptr < 0 {
            return Err("WASM 共享区溢出".into());
        }
        memory
            .write(&mut self.store, ptr as usize, bytes)
            .map_err(|e| e.to_string())?;
        Ok((ptr, bytes.len() as i32))
    }

    /// 执行一次查询，返回结果行。
    /// 内存协议（同 cronkit.wasm）：宿主写文本 → query(ptr,len,now) →
    /// result_count/result_field 取回。offset 均为线性内存绝对地址。
    pub fn query(&mut self, text: &str) -> Result<Vec<WasmRow>, String> {
        self.last_used = Instant::now();
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let query_fn = self
            .instance
            .get_typed_func::<(i32, i32, i64), i32>(&mut self.store, "query")
            .map_err(|e| format!("无 query：{e}"))?;

        let (qptr, qlen) = self.write_heap(text.as_bytes())?;
        query_fn
            .call(&mut self.store, (qptr, qlen, now_secs))
            .map_err(|e| format!("query 执行失败：{e}"))?;

        self.read_rows()
    }

    /// 读取当前结果缓冲（不重跑 query）。插件在 on_response 里重设行后
    /// （异步搜索回包 / 播放状态行），宿主经此取回并刷新面板列表。
    pub fn take_results(&mut self) -> Result<Vec<WasmRow>, String> {
        self.last_used = Instant::now();
        self.read_rows()
    }

    /// result_count/result_field 协议读取（query 与 take_results 共用）
    fn read_rows(&mut self) -> Result<Vec<WasmRow>, String> {
        let memory = self
            .instance
            .get_memory(&mut self.store, "memory")
            .ok_or("无 memory 导出")?;
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

    /// 结果行激活（旧版运行时插件无 activate 导出时为无操作：
    /// 激活退化为宿主复制 payload，与 W3 行为一致）
    pub fn activate(&mut self, payload: &str) -> Result<(), String> {
        self.last_used = Instant::now();
        let Some(f) = self
            .instance
            .get_typed_func::<(i32, i32), i32>(&mut self.store, "activate")
            .ok()
        else {
            return Ok(());
        };
        let (ptr, len) = self.write_heap(payload.as_bytes())?;
        f.call(&mut self.store, (ptr, len))
            .map_err(|e| format!("activate 执行失败：{e}"))?;
        Ok(())
    }

    /// 能力执行回包（成功带 data、失败带 error 文案）。插件无
    /// on_response 导出时为无操作。
    pub fn deliver_response(
        &mut self,
        request_id: u64,
        ok: bool,
        error: &str,
        data: Option<&str>,
    ) -> Result<(), String> {
        let Some(f) = self
            .instance
            .get_typed_func::<(i32, i32, i32, i32), i32>(&mut self.store, "on_response")
            .ok()
        else {
            return Ok(());
        };
        let payload: String = if ok {
            data.unwrap_or("").to_string()
        } else {
            error.to_string()
        };
        if payload.len() > MAX_RESPONSE_BYTES {
            // 共享区与查询协议共用，超大载荷不进 WASM（network.fetch 大响应
            // 场景列 W5+ 扩展）
            return self.deliver_response(request_id, false, "载荷超出 WASM 共享区限制", None);
        }
        let (ptr, len) = self.write_heap(payload.as_bytes())?;
        f.call(&mut self.store, (request_id as i32, if ok { 1 } else { 0 }, ptr, len))
            .map_err(|e| format!("on_response 执行失败：{e}"))?;
        Ok(())
    }

    /// ui.show 表单事件回传（submit/cancel，JSON）
    pub fn deliver_ui_event(&mut self, json: &str) -> Result<(), String> {
        let Some(f) = self
            .instance
            .get_typed_func::<(i32, i32), i32>(&mut self.store, "on_ui_event")
            .ok()
        else {
            return Ok(());
        };
        let (ptr, len) = self.write_heap(json.as_bytes())?;
        f.call(&mut self.store, (ptr, len))
            .map_err(|e| format!("on_ui_event 执行失败：{e}"))?;
        Ok(())
    }

    /// 取走本次 query/activate 期间插件发出的能力与 UI 请求
    pub fn drain(&mut self) -> WasmDrain {
        let s = self.store.data_mut();
        WasmDrain {
            caps: std::mem::take(&mut s.caps),
            uis: std::mem::take(&mut s.uis),
        }
    }

    pub fn last_used(&self) -> Instant {
        self.last_used
    }

    pub fn declared(&self) -> &[String] {
        &self.declared
    }

    /// 线性内存页数 × 64KiB = 当前内存占用
    pub fn mem_kb(&mut self) -> u64 {
        self.instance
            .get_memory(&mut self.store, "memory")
            .map(|m| m.size(&self.store) as u64 * 64)
            .unwrap_or(0)
    }
}

/// 全部已加载 WASM 插件（id → 实例）
#[derive(Default)]
pub struct WasmRegistry {
    plugins: HashMap<String, WasmPlugin>,
}

impl WasmRegistry {
    /// entry 以 .wasm 结尾则加载（幂等：已加载跳过），返回是否属于本后端。
    /// `declared` = manifest permissions 原始串（装载期能力校验依据）。
    pub fn ensure_loaded(
        &mut self,
        id: &str,
        entry_path: &Path,
        declared: &[String],
    ) -> Result<bool, String> {
        if entry_path.extension().is_none_or(|e| e != "wasm") {
            return Ok(false);
        }
        if self.plugins.contains_key(id) {
            return Ok(true);
        }
        let mut plugin = WasmPlugin::load(entry_path, declared)?;
        plugin.store.data_mut().plugin_id = id.to_string();
        status_map()
            .lock()
            .unwrap()
            .insert(id.to_string(), plugin.mem_kb());
        self.plugins.insert(id.to_string(), plugin);
        Ok(true)
    }

    /// 查询；错误原样上抛（调用方计失败滑窗，不再静默吞）
    pub fn query(&mut self, id: &str, text: &str) -> Result<Vec<WasmRow>, String> {
        self.plugins
            .get_mut(id)
            .ok_or_else(|| format!("WASM 插件 {id} 未加载"))?
            .query(text)
    }

    /// 全部 WASM 插件广播查询：(id, Result) 列表，Err 由调用方计失败
    pub fn broadcast(&mut self, text: &str) -> Vec<(String, Result<Vec<WasmRow>, String>)> {
        self.plugins
            .iter_mut()
            .map(|(id, p)| (id.clone(), p.query(text)))
            .collect()
    }

    /// 激活；插件未加载返回 None
    pub fn activate(&mut self, id: &str, payload: &str) -> Option<Result<(), String>> {
        Some(self.plugins.get_mut(id)?.activate(payload))
    }

    /// 取回某插件的结果缓冲（on_response 重设行后）；未加载返回 None
    pub fn take_results(&mut self, id: &str) -> Option<Result<Vec<WasmRow>, String>> {
        Some(self.plugins.get_mut(id)?.take_results())
    }

    /// 排干全部插件的挂起请求：(id, drain)
    pub fn drain_all(&mut self) -> Vec<(String, WasmDrain)> {
        self.plugins
            .iter_mut()
            .map(|(id, p)| (id.clone(), p.drain()))
            .collect()
    }

    /// 能力回包路由（PluginMgr::route_cap_response 调用）
    pub fn deliver_response(
        &mut self,
        id: &str,
        request_id: u64,
        ok: bool,
        error: &str,
        data: Option<&str>,
    ) -> Option<Result<(), String>> {
        Some(
            self.plugins
                .get_mut(id)?
                .deliver_response(request_id, ok, error, data),
        )
    }

    pub fn deliver_ui_event(&mut self, id: &str, json: &str) -> Option<Result<(), String>> {
        Some(self.plugins.get_mut(id)?.deliver_ui_event(json))
    }

    pub fn permissions_of(&self, id: &str) -> Option<&[String]> {
        self.plugins.get(id).map(|p| p.declared())
    }

    pub fn is_loaded(&self, id: &str) -> bool {
        self.plugins.contains_key(id)
    }

    pub fn loaded_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.plugins.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// 卸载实例（停用即时化用）；返回是否确实在册
    pub fn unload(&mut self, id: &str) -> bool {
        let removed = self.plugins.remove(id).is_some();
        if removed {
            status_map().lock().unwrap().remove(id);
        }
        removed
    }
}

/// 进程级运行状态快照（id → 线性内存 KB）。PluginStatus 走 zbus 线程，
/// 够不着主线程的 Rc<WasmRegistry>，这里以互斥量做最小快照（同进程）。
fn status_map() -> &'static Mutex<HashMap<String, u64>> {
    static STATUS: std::sync::OnceLock<Mutex<HashMap<String, u64>>> = std::sync::OnceLock::new();
    STATUS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// WASM 插件状态快照（PluginStatus 用）：(id, 内存 KB)
pub fn status_snapshot() -> Vec<(String, u64)> {
    status_map().lock().unwrap().iter().map(|(k, v)| (k.clone(), *v)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wasm_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../plugins/{name}/wasm/target/wasm32-unknown-unknown/release/{name}_wasm.wasm"))
    }

    #[test]
    fn loads_and_queries_cronkit() {
        let p = wasm_path("cronkit");
        if !p.exists() {
            eprintln!("跳过：cronkit.wasm 未构建");
            return;
        }
        let mut reg = WasmRegistry::default();
        // cronkit 不用能力：空声明即可装载
        assert!(reg.ensure_loaded("cronkit", &p, &[]).unwrap());
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
    fn radio_panel_shell_query_and_async_rows() {
        // W6 mini 壳端到端：入口行 + 状态行异步刷新 + 直搜占位/回包
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
            "../../plugins/radio/wasm/target/wasm32-unknown-unknown/release/radio_panel_wasm.wasm",
        );
        if !p.exists() {
            eprintln!("跳过：radio panel.wasm 未构建");
            return;
        }
        let declared = vec![
            "network.fetch@search.qingting.fm".to_string(),
            "fs.read@~/.local/share/yihu/plugin-data/radio/status.json".to_string(),
        ];
        let mut reg = WasmRegistry::default();
        assert!(reg.ensure_loaded("radio", &p, &declared).unwrap());

        // ① 裸词头：入口行 + fs.read 状态请求
        let rows = reg.query("radio", "电台").unwrap();
        assert_eq!(rows.len(), 1, "入口行，实际 {rows:?}");
        assert!(rows[0].title.contains("电台控制台"));
        assert_eq!(rows[0].payload, "open");
        let drained = reg.drain_all();
        let caps = &drained.iter().find(|(id, _)| id == "radio").unwrap().1.caps;
        assert_eq!(caps.len(), 1);
        assert_eq!(caps[0].capability, "fs.read");
        assert_eq!(
            caps[0].params["path"],
            "~/.local/share/yihu/plugin-data/radio/status.json"
        );
        // 状态回包：正在播 → 状态行置顶
        // 执行器契约：data 一律 JSON 引号包裹（spawn_fsread_job 同款）
        let status = serde_json::to_string(
            &serde_json::json!({
                "playing": true,
                "channel": {"id": 1133, "title": "杭州交通91.8电台", "cover": ""},
                "updated_at": 0
            })
            .to_string(),
        )
        .unwrap();
        reg.deliver_response("radio", caps[0].request_id, true, "", Some(&status))
            .unwrap()
            .unwrap();
        let rows = reg.take_results("radio").unwrap().unwrap();
        assert_eq!(rows.len(), 2, "状态行+入口行，实际 {rows:?}");
        assert!(rows[0].title.contains("正在播放"));
        assert_eq!(rows[0].payload, "stop");

        // ② 直搜：占位行 + network.fetch 请求
        let rows = reg.query("radio", "电台 交通").unwrap();
        assert!(rows[0].title.contains("搜索"), "{rows:?}");
        let drained = reg.drain_all();
        let caps = &drained.iter().find(|(id, _)| id == "radio").unwrap().1.caps;
        assert_eq!(caps.len(), 1);
        assert_eq!(caps[0].capability, "network.fetch");
        assert!(caps[0].params["url"]
            .as_str()
            .unwrap()
            .contains("k=%E4%BA%A4%E9%80%9A"));
        // 搜索回包：channel_live 过滤 + play payload
        let search = serde_json::to_string(
            &serde_json::json!({
                "errcode": 0,
                "data": {"data": {"docs": [
                    {"id": 1133, "title": "杭州交通91.8电台", "type": "channel_live",
                     "audience_count": 584054},
                {"id": 9, "title": "某播客", "type": "channel_ondemand"}
                ]}}}
            )
            .to_string(),
        )
        .unwrap();
        reg.deliver_response("radio", caps[0].request_id, true, "", Some(&search))
            .unwrap()
            .unwrap();
        let rows = reg.take_results("radio").unwrap().unwrap();
        assert_eq!(rows.len(), 1, "点播被滤掉，实际 {rows:?}");
        assert!(rows[0].title.contains("杭州交通"));
        assert_eq!(rows[0].payload, "play:1133|杭州交通91.8电台");
    }

    #[test]
    fn mdworkbench_panel_shell_query() {
        // W7 收编 mini 壳：词头命中出行。回归位：manifest 权限声明非法
        // （fs.read 缺 @glob、process.spawn 不在词表）曾致整插件注册表
        // 拒载，md / markdown 搜索全空。
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
            "../../plugins/mdworkbench/wasm/target/wasm32-unknown-unknown/release/mdworkbench_panel_wasm.wasm",
        );
        if !p.exists() {
            eprintln!("跳过：mdworkbench panel.wasm 未构建");
            return;
        }
        let declared = vec![
            "fs.read@~/**".to_string(),
            "fs.write@~/**".to_string(),
            "notify".to_string(),
        ];
        let mut reg = WasmRegistry::default();
        assert!(reg.ensure_loaded("mdworkbench", &p, &declared).unwrap());
        for q in ["md", "markdown", "笔记", "md 笔记本"] {
            let rows = reg.query("mdworkbench", q).unwrap();
            assert_eq!(rows.len(), 1, "查询 {q:?} 应命中入口行，实际 {rows:?}");
            assert!(rows[0].title.contains("Markdown 工作台"));
            assert_eq!(rows[0].payload, "open");
        }
        // 非本插件词头静默
        assert!(reg.query("mdworkbench", "hello").unwrap().is_empty());
    }

    #[test]
    fn non_wasm_entry_rejected() {
        let mut reg = WasmRegistry::default();
        assert!(!reg.ensure_loaded("x", Path::new("/tmp/nothing.py"), &[]).unwrap());
    }

    #[test]
    fn undeclared_capability_import_rejected() {
        // theme-switch.wasm 导入 settings-write + notify：manifest 缺
        // notify 声明必须拒载（能力即安全）
        let p = wasm_path("theme-switch");
        if !p.exists() {
            eprintln!("跳过：theme-switch.wasm 未构建");
            return;
        }
        let mut reg = WasmRegistry::default();
        let missing = vec!["settings.write@org.gnome.desktop.interface".to_string()];
        let err = reg
            .ensure_loaded("theme-switch", &p, &missing)
            .unwrap_err();
        assert!(err.contains("notify"), "{err}");
        // 声明齐全则装载成功
        let full = vec![
            "settings.write@org.gnome.desktop.interface".to_string(),
            "notify".to_string(),
        ];
        assert!(reg.ensure_loaded("theme-switch", &p, &full).unwrap());
        let rows = reg.query("theme-switch", "深色").unwrap();
        assert!(!rows.is_empty(), "深色词头应出切换行");
    }

    #[test]
    fn theme_switch_activate_emits_settings_write() {
        // W4 全链路（除真实执行）：query → activate → 能力请求入队 →
        // drain → caps::evaluate 直接消化（与 stdio 轨同一套授权）
        let p = wasm_path("theme-switch");
        if !p.exists() {
            eprintln!("跳过：theme-switch.wasm 未构建");
            return;
        }
        let full = vec![
            "settings.write@org.gnome.desktop.interface".to_string(),
            "notify".to_string(),
        ];
        let mut reg = WasmRegistry::default();
        reg.ensure_loaded("theme-switch", &p, &full).unwrap();
        assert!(reg.query("theme-switch", "dark").unwrap().len() == 2);
        reg.activate("theme-switch", "prefer-dark").unwrap().unwrap();
        let drains = reg.drain_all();
        let (_, d) = drains.iter().find(|(id, _)| id == "theme-switch").unwrap();
        assert_eq!(d.caps.len(), 1, "activate 应发出一个能力请求");
        let req = &d.caps[0];
        assert_eq!(req.capability, "settings.write");
        assert_eq!(req.params["schema"], "org.gnome.desktop.interface");
        assert_eq!(req.params["key"], "color-scheme");
        assert_eq!(req.params["value"], "prefer-dark");
        let action = crate::caps::evaluate(&full, req.capability, &req.params).unwrap();
        assert!(matches!(
            action,
            crate::caps::CapAction::SettingsWrite { ref value, .. } if value == "prefer-dark"
        ));
    }
}
