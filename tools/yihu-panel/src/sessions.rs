//! 外部插件会话管理：呼出拉起、查询广播、结果回传、收起杀灭。
//!
//! 生命周期：呼出（summon）拉起全部启用插件的进程并发送 init；
//! 每次 keystroke 向各会话广播 query（行式 JSON）；
//! 面板收起（hide）时对进程组 SIGKILL 整组杀灭 → 待命零进程零内存。
//! 插件连续退出（5 分钟内 4 次）则本会话禁用，避免反复崩溃刷屏。
//!
//! M4 起 spawn 统一走 sandbox::command（bwrap 白名单，fail-closed）；
//! 主循环回调里仍禁止任何无界阻塞（BUG-001 教训）——wait() 仅在
//! 「读线程已见 EOF / kill_all 同约定」的有界路径上调用。

use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use yihu_core::plugins;

use crate::app::PanelEntry;

const MAX_FAILURES: usize = 4;
const FAILURE_WINDOW: Duration = Duration::from_secs(5 * 60);
/// 插件输出单行上限：超限视为协议违约，断开会话（审查 M-2，
/// 读线程内存必须有界）。results 条数上限同理 1000（parse_line）。
const MAX_LINE_BYTES: usize = 1024 * 1024;
/// 单次 drain 最多处理的能力请求数（审查 M-1：洪水在多 tick 间分摊）
const MAX_CAPS_PER_DRAIN: usize = 64;
/// 常驻会话空闲自退阈值（M4 冻结决策 #7：socket-activation 列 M5，
/// v1 = 收起不杀 + 空闲发 shutdown 自退）
pub const RESIDENT_IDLE_EXIT: Duration = Duration::from_secs(300);
/// shutdown 后的宽限：仍不退出则组级 SIGKILL
pub const RESIDENT_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// 插件请求渲染的 UI 表单（drain 第三槽，主线程渲染）
#[derive(Debug)]
pub struct UiShowRequest {
    pub plugin: String,
    pub gen: u64,
    pub ui_id: u64,
    pub spec: serde_json::Value,
}

pub enum PluginEvent {
    Results { plugin: String, query_id: u64, items: Vec<PanelEntry> },
    Exited { plugin: String, gen: u64 },
    Capability(CapRequest),
    /// UI 插件层：插件请求宿主渲染声明式表单（api:1 加法扩展）
    UiShow { plugin: String, gen: u64, ui_id: u64, spec: serde_json::Value },
}

/// 插件发来的能力请求（capability_request）。gen 用于审计关联；
/// params 保留原始 JSON，形状校验在 caps::evaluate。
#[derive(Debug)]
pub struct CapRequest {
    pub plugin: String,
    pub gen: u64,
    pub request_id: u64,
    pub capability: String,
    pub params: serde_json::Value,
}

struct Session {
    id: String,
    /// 会话代数：id 每次呼出都相同（如 passgen），只有 (id, gen) 唯一定位
    /// 一次会话。旧代的迟到 Exited 事件若按 id 误配新一代会话，会对
    /// 活进程 wait() 把主循环永久卡死（0×0 不可呼出的根源）。
    gen: u64,
    child: Child,
    stdin: ChildStdin,
    /// manifest 声明的权限原始串（含「能力@参数」，已过词表校验），
    /// 能力代理按它逐请求强制
    permissions: Vec<String>,
    /// 常驻 provider：收起不杀，空闲自退
    resident: bool,
    /// 已发 shutdown、等待自退（Exited 不计失败；超宽限强杀）
    shutting_down_since: Option<Instant>,
    /// 最近一次宿主→插件通信时刻（query/activate），空闲自退的基准
    last_used: Instant,
}

/// 外部插件会话管理器。跨呼出保持（失败历史），会话随面板收起整组杀灭。
pub struct PluginMgr {
    tx: Sender<PluginEvent>,
    rx: Receiver<PluginEvent>,
    sessions: Vec<Session>,
    /// (插件 id, 退出时间)——崩溃/异常退出的滑动窗口
    failures: Vec<(String, Instant)>,
    /// 连续失败超限，本运行期禁用的插件
    disabled_by_failures: HashSet<String>,
    query_seq: u64,
    /// 会话代数计数器：每次 spawn 递增
    gen: u64,
    /// 当前查询（文本, id）——过期结果丢弃
    current_query: Option<(String, u64)>,
    /// 各插件对当前查询的最新结果（query_id, rows）；未超期的旧结果保留，避免闪烁
    latest: HashMap<String, (u64, Vec<PanelEntry>)>,
    /// 选中文件上下文（SelectFiles 注入，收起即清）；query 时按
    /// manifest 权限（selected_files.read）转发给对应会话
    context_files: Vec<String>,
}

impl PluginMgr {
    pub fn new() -> PluginMgr {
        let (tx, rx) = mpsc::channel();
        PluginMgr {
            tx,
            rx,
            sessions: Vec::new(),
            failures: Vec::new(),
            disabled_by_failures: HashSet::new(),
            query_seq: 0,
            gen: 0,
            current_query: None,
            latest: HashMap::new(),
            context_files: Vec::new(),
        }
    }

    /// 呼出时调用：确保全部启用插件的会话存活（幂等）。
    /// 崩溃超限的插件本次运行期跳过；持久禁用走中心「插件」页。
    /// M4 起插件统一经 bwrap 沙箱拉起（fail-closed：沙箱不可用则不拉起，
    /// 见 sandbox 模块与 docs/插件基座安全模型与发布策略.md）。
    pub fn ensure_sessions(&mut self) {
        self.ensure_sessions_in(&plugins::plugins_dir(), &plugins::data_home())
    }

    /// 同上，注册表根与数据根可注入（测试：临时目录，不碰真实用户数据）。
    pub fn ensure_sessions_in(&mut self, registry: &std::path::Path, data_root: &std::path::Path) {
        let debug = std::env::var_os("YIHU_PANEL_DEBUG").is_some();
        if !crate::sandbox::available() {
            eprintln!(
                "yihu-panel: bwrap 不可用，插件沙箱无法建立，本次运行期不拉起任何插件（fail-closed）"
            );
            return;
        }
        let state = yihu_core::plugins::PluginsState::load();
        let (installed, errors) = plugins::list_installed_in(registry);
        if debug {
            eprintln!(
                "yihu-panel: ensure_sessions 注册表 {} 个插件，错误 {} 条，现有会话 {}",
                installed.len(),
                errors.len(),
                self.sessions.len()
            );
        }
        for e in errors {
            eprintln!("yihu-panel: 插件注册表问题：{e}");
        }
        for inst in installed {
            let id = inst.manifest.id.clone();
            if self.sessions.iter().any(|s| s.id == id)
                || state.is_disabled(&id)
                || self.disabled_by_failures.contains(&id)
            {
                continue;
            }
            if self.recent_failures(&id) >= MAX_FAILURES {
                eprintln!("yihu-panel: 插件 {id} 连续失败 {MAX_FAILURES} 次，本次运行期禁用");
                self.disabled_by_failures.insert(id);
                continue;
            }
            match self.spawn_session(&inst, debug, data_root) {
                Ok(sess) => {
                    if debug {
                        eprintln!("yihu-panel: 已拉起插件会话 {id}");
                    }
                    self.sessions.push(sess);
                }
                Err(e) => {
                    eprintln!("yihu-panel: 拉起插件 {} 失败：{e}", id);
                    self.record_failure(&id);
                }
            }
        }
    }

    fn spawn_session(
        &mut self,
        inst: &plugins::Installed,
        debug: bool,
        data_root: &std::path::Path,
    ) -> io::Result<Session> {
        let id = inst.manifest.id.clone();
        self.gen += 1;
        let gen = self.gen;
        // 唯一可写数据目录（0700）；插件目录经 bwrap 只读挂载，运行期写入
        // 全部落在此处，宿主真实路径不进沙箱（init 只给虚拟路径 /data）
        plugins::ensure_plugin_data_dir_in(data_root, &id)?;
        let mut child = crate::sandbox::command(
            &inst.manifest.entry,
            &inst.dir,
            &plugins::plugin_data_dir_in(data_root, &id),
        )?
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(if debug { Stdio::inherit() } else { Stdio::null() })
        .process_group(0) // 组级杀灭，不留孤儿（杀的是 bwrap 进程组）
        .spawn()?;
        let mut stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");
        let data_dir = crate::sandbox::SANDBOX_DATA_DIR;
        writeln!(
            stdin,
            "{{\"type\":\"init\",\"api\":1,\"data_dir\":{}}}",
            serde_json::to_string(&data_dir).expect("路径是合法 JSON 字符串")
        )?;
        let permissions: Vec<String> = inst.manifest.permissions.clone();
        let tx = self.tx.clone();
        let reader_id = id.clone();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut buf = Vec::with_capacity(512);
                // E2E-DEBUG: 收到行即打印（调试后移除）
                // 行长硬上限：take 限流读入，超限断开会话（Exited → 计失败）
                let mut limited = (&mut reader).take((MAX_LINE_BYTES + 1) as u64);
                match limited.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) if n > MAX_LINE_BYTES => {
                        eprintln!("yihu-panel: 插件 {reader_id} 单行超限（>1 MiB），断开会话");
                        break;
                    }
                    Ok(_) => {
                        let line = String::from_utf8_lossy(&buf);
                        let line = line.trim_end_matches(['\n', '\r']);
                        if let Some(ev) = parse_line(&reader_id, gen, line) {
                            if tx.send(ev).is_err() {
                                break;
                            }
                        }
                    }
                }
            }
            let _ = tx.send(PluginEvent::Exited { plugin: reader_id, gen });
        });
        Ok(Session {
            id,
            gen,
            child,
            stdin,
            permissions,
            resident: inst.manifest.resident,
            shutting_down_since: None,
            last_used: Instant::now(),
        })
    }

    /// 查询某插件会话声明的权限原始串（按 id 模糊；能力路径一律走
    /// permissions_of_gen 精确配对，此方法留作调试）
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn permissions_of(&self, plugin: &str) -> Option<&Vec<String>> {
        self.sessions.iter().find(|s| s.id == plugin).map(|s| &s.permissions)
    }

    /// 同上，按 (id, gen) 精确配对（BUG-001 教训：旧代的迟到请求不得
    /// 借新一代会话的身份被执行/回复）。
    pub fn permissions_of_gen(&self, plugin: &str, gen: u64) -> Option<&Vec<String>> {
        self.sessions
            .iter()
            .find(|s| s.id == plugin && s.gen == gen)
            .map(|s| &s.permissions)
    }

    /// 回复能力请求（capability_response），按 (id, gen) 精确投递：
    /// 旧代会话的迟到请求只丢弃、不回复（回复错位会让新代插件的
    /// pending 表错乱）。会话已死或代数不符则静默丢弃。
    /// `data` 为成功时的返回载荷（JSON 文本，如 network.fetch 响应体），
    /// 协议加法扩展：{"type":"capability_response","id":N,"ok":true,"data":...}
    pub fn respond_gen(
        &mut self,
        plugin: &str,
        gen: u64,
        request_id: u64,
        ok: bool,
        error: &str,
        data: Option<&str>,
    ) {
        let line = if ok {
            match data {
                Some(d) => format!(
                    "{{\"type\":\"capability_response\",\"id\":{request_id},\"ok\":true,\"data\":{d}}}"
                ),
                None => format!("{{\"type\":\"capability_response\",\"id\":{request_id},\"ok\":true}}"),
            }
        } else {
            format!(
                "{{\"type\":\"capability_response\",\"id\":{request_id},\"ok\":false,\"error\":{}}}",
                serde_json::to_string(error).unwrap_or_else(|_| "\"\"".into())
            )
        };
        for s in &mut self.sessions {
            if s.id != plugin || s.gen != gen {
                continue;
            }
            s.last_used = Instant::now();
            let _ = s.stdin.write_all(line.as_bytes());
            let _ = s.stdin.write_all(b"\n");
            let _ = s.stdin.flush();
            return;
        }
    }

    /// 注入选中文件上下文（app.rs 已净化：≤64 项、单项 ≤4KiB）
    pub fn set_context_files(&mut self, files: Vec<String>) {
        self.context_files = files;
    }

    /// 每次 keystroke：向全部会话广播查询。空文本清空插件结果。
    /// 声明了 selected_files.read 的会话额外携带 context.files。
    pub fn broadcast(&mut self, text: &str) {
        if text.is_empty() {
            self.latest.clear();
            self.current_query = None;
            return;
        }
        self.query_seq += 1;
        let id = self.query_seq;
        self.current_query = Some((text.to_string(), id));
        let text_json = serde_json::to_string(text).expect("文本是合法 JSON 字符串");
        let files_json = if self.context_files.is_empty() {
            None
        } else {
            Some(
                serde_json::to_string(&self.context_files)
                    .expect("路径列表是合法 JSON"),
            )
        };
        let dead: Vec<String> = self
            .sessions
            .iter_mut()
            .filter_map(|s| {
                // 任何宿主→插件通信都刷新空闲基准
                s.last_used = Instant::now();
                let line = match (
                    &files_json,
                    s.permissions
                        .iter()
                        .any(|p| p == yihu_core::permissions::SELECTED_FILES_READ),
                ) {
                    (Some(f), true) => format!(
                        "{{\"type\":\"query\",\"id\":{id},\"text\":{text_json},\"context\":{{\"files\":{f}}}}}"
                    ),
                    _ => format!("{{\"type\":\"query\",\"id\":{id},\"text\":{text_json}}}"),
                };
                (s.stdin.write_all(line.as_bytes()).is_err()
                    || s.stdin.write_all(b"\n").is_err()
                    || s.stdin.flush().is_err())
                .then(|| s.id.clone())
            })
            .collect();
        for id in dead {
            self.record_failure(&id);
            self.sessions.retain(|s| s.id != id);
            self.latest.remove(&id);
        }
    }

    /// 收起时调用：进程组级 SIGKILL 杀灭非常驻会话；常驻会话保留
    /// （空闲自退交由 sweep_idle），仅清结果与上下文。
    pub fn kill_all(&mut self) {
        let pinned: std::collections::HashSet<String> =
            crate::ui::open_plugin_ids().into_iter().collect();
        let keep: Vec<usize> = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| s.resident || pinned.contains(&s.id))
            .map(|(i, _)| i)
            .collect();
        for (i, s) in self.sessions.iter_mut().enumerate() {
            if keep.contains(&i) {
                continue;
            }
            unsafe {
                libc::kill(-(s.child.id() as i32), libc::SIGKILL);
            }
            let _ = s.child.wait();
        }
        self.sessions.retain(|s| s.resident);
        self.latest.clear();
        self.current_query = None;
        self.context_files.clear(); // 选中文件上下文一次性：收起即失效
    }

    /// 常驻会话空闲清扫（面板隐藏时由常驻泵调用）：空闲超阈值发
    /// shutdown；宽限后仍不退则组级 SIGKILL。返回是否发生了清扫
    /// （调试日志用）。
    pub fn sweep_idle(&mut self) -> bool {
        self.sweep_idle_with(RESIDENT_IDLE_EXIT, RESIDENT_SHUTDOWN_GRACE)
    }

    /// 同上，阈值可注入（测试用）。
    pub fn sweep_idle_with(&mut self, idle_exit: Duration, grace: Duration) -> bool {
        let mut acted = false;
        for s in &mut self.sessions {
            if !s.resident {
                continue;
            }
            match s.shutting_down_since {
                None => {
                    if s.last_used.elapsed() >= idle_exit {
                        let line = "{\"type\":\"shutdown\"}";
                        if s.stdin.write_all(line.as_bytes()).is_ok()
                            && s.stdin.write_all(b"\n").is_ok()
                            && s.stdin.flush().is_ok()
                        {
                            s.shutting_down_since = Some(Instant::now());
                            acted = true;
                        } else {
                            // 管道已断：插件已死，交给 Exited 事件处理
                            acted = true;
                        }
                    }
                }
                Some(t0) => {
                    if t0.elapsed() >= grace {
                        unsafe {
                            libc::kill(-(s.child.id() as i32), libc::SIGKILL);
                        }
                        let _ = s.child.wait();
                        acted = true;
                        // 会话移除交给 Exited 事件（读线程 EOF 已必至）
                    }
                }
            }
        }
        acted
    }

    /// 常驻会话是否仍在（跨收起存活校验；cfg(test) 之外暂无调用方）
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_session_alive(&self, plugin: &str) -> bool {
        self.sessions.iter().any(|s| s.id == plugin)
    }

    /// 会话当前代数（测试/调试用）
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn session_gen(&self, plugin: &str) -> Option<u64> {
        self.sessions.iter().find(|s| s.id == plugin).map(|s| s.gen)
    }

    /// 某插件当前滑动窗口内的失败计数（同上）
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn failures_of(&self, plugin: &str) -> usize {
        self.recent_failures(plugin)
    }

    /// 主循环 tick 调用：处理结果/退出/能力请求/UI 请求事件。
    /// 返回（是否有影响展示的变化, 待处理能力请求, 待渲染 UI 请求）。
    /// 请求在 drain 返回后处理（执行/回复会重借 mgr，不能在借用中回调）。
    pub fn drain(&mut self) -> (bool, Vec<CapRequest>, Vec<UiShowRequest>) {
        let mut dirty = false;
        let mut caps = Vec::new();
        let mut uis = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(PluginEvent::Results { plugin, query_id, items }) => {
                    let current = self.current_query.as_ref().map(|(_, id)| *id);
                    if current == Some(query_id) {
                        let rows: Vec<PanelEntry> = items
                            .into_iter()
                            .map(|it| PanelEntry {
                                title: it.title,
                                subtitle: it.subtitle,
                                icon_spec: it.icon_spec,
                                kind: "plugin",
                                payload: format!("{plugin}|{}", it.payload),
                            })
                            .collect();
                        self.latest.insert(plugin, (query_id, rows));
                        dirty = true;
                    }
                }
                Ok(PluginEvent::Capability(cap)) => {
                    caps.push(cap);
                    if caps.len() >= MAX_CAPS_PER_DRAIN {
                        break; // 洪水分摊到多 tick（审查 M-1）
                    }
                }
                Ok(PluginEvent::UiShow { plugin, gen, ui_id, spec }) => {
                    uis.push(UiShowRequest { plugin, gen, ui_id, spec });
                }
                Ok(PluginEvent::Exited { plugin, gen }) => {
                    // 只有 (id, gen) 都匹配才认账：旧代（已收起会话）的迟到
                    // 退出事件直接丢弃，否则按 id 会误配新一代活会话，
                    // 对活进程 wait() 卡死主循环（面板从此呼不出的根源）。
                    if let Some(i) = self
                        .sessions
                        .iter()
                        .position(|s| s.id == plugin && s.gen == gen)
                    {
                        // 空闲自退（shutdown 后退出）不算失败
                        let shutting_down = self.sessions[i].shutting_down_since.is_some();
                        let s = &mut self.sessions[i];
                        // 读线程已见 stdout EOF；组级 SIGKILL 确保进程真正
                        // 终结，wait() 与 kill_all 同约定（有界）
                        unsafe {
                            libc::kill(-(s.child.id() as i32), libc::SIGKILL);
                        }
                        let _ = s.child.wait();
                        self.sessions.remove(i);
                        if !shutting_down {
                            self.record_failure(&plugin);
                        }
                        if self.latest.remove(&plugin).is_some() {
                            dirty = true;
                        }
                    }
                }
                Err(_) => break,
            }
        }
        (dirty, caps, uis)
    }

    /// 向指定会话回传 UI 事件（ui.event）
    pub fn send_ui_event(&mut self, plugin: &str, gen: u64, ui_id: u64, event: &str, values: &str) {
        let line = if event == "submit" {
            format!(
                "{{\"type\":\"ui.event\",\"ui_id\":{ui_id},\"event\":\"submit\",\"values\":{values}}}"
            )
        } else {
            format!("{{\"type\":\"ui.event\",\"ui_id\":{ui_id},\"event\":\"{event}\"}}")
        };
        self.send_to_session(plugin, gen, &line);
    }

    /// 向指定会话发送一行协议消息（UI 事件回传等宿主主动消息）
    pub fn send_to_session(&mut self, plugin: &str, gen: u64, line: &str) {
        for s in &mut self.sessions {
            if s.id != plugin || s.gen != gen {
                continue;
            }
            s.last_used = Instant::now();
            let _ = s.stdin.write_all(line.as_bytes());
            let _ = s.stdin.write_all(b"\n");
            let _ = s.stdin.flush();
            return;
        }
    }

    /// 当前查询下全部插件结果行（应用按 id 稳定排序）。
    pub fn latest_rows(&self) -> Vec<PanelEntry> {
        let mut ids: Vec<&String> = self.latest.keys().collect();
        ids.sort();
        ids.into_iter()
            .flat_map(|id| self.latest[id.as_str()].1.iter().cloned())
            .collect()
    }

    /// 转发激活到来源插件会话。
    pub fn activate(&mut self, plugin: &str, payload: &str) {
        let line = format!(
            "{{\"type\":\"activate\",\"id\":{},\"payload\":{}}}",
            self.query_seq,
            serde_json::to_string(payload).expect("payload 是合法 JSON 字符串")
        );
        self.send_all_to(plugin, &line);
    }

    fn send_all_to(&mut self, plugin: &str, line: &str) {
        for s in &mut self.sessions {
            if s.id != plugin {
                continue;
            }
            s.last_used = Instant::now();
            if s.stdin.write_all(line.as_bytes()).is_ok()
                && s.stdin.write_all(b"\n").is_ok()
                && s.stdin.flush().is_ok()
            {
                return;
            }
        }
    }

    fn recent_failures(&self, plugin: &str) -> usize {
        self.failures
            .iter()
            .filter(|(id, t)| id == plugin && t.elapsed() < FAILURE_WINDOW)
            .count()
    }

    fn record_failure(&mut self, plugin: &str) {
        self.failures.push((plugin.to_string(), Instant::now()));
        self.failures.retain(|(_, t)| t.elapsed() < FAILURE_WINDOW);
        if self.recent_failures(plugin) >= MAX_FAILURES {
            self.disabled_by_failures.insert(plugin.to_string());
            eprintln!("yihu-panel: 插件 {plugin} 在 5 分钟内失败 {MAX_FAILURES} 次，本次运行期禁用");
            // M4 穿插项：落盘持久禁用（重启后仍禁用；中心「插件」页开关可恢复）。
            // 落盘必须后台线程——本函数在主循环回调链上（tick/keystroke），
            // 同步 IO 违反硬性规则 #1（审查 I-2）。
            let p = plugin.to_string();
            std::thread::spawn(move || {
                let mut st = yihu_core::plugins::PluginsState::load();
                st.set_disabled(&p, true);
                if let Err(e) = st.save_to(&yihu_core::plugins::state_path()) {
                    eprintln!("yihu-panel: 崩溃禁用落盘失败：{e}");
                }
            });
        }
    }
}

/// 解析插件输出行。results/capability_request 有效；其余行返回 None。
fn parse_line(plugin: &str, gen: u64, line: &str) -> Option<PluginEvent> {
    #[derive(serde::Deserialize)]
    struct Item {
        title: String,
        #[serde(default)]
        subtitle: String,
        #[serde(default)]
        icon: String,
        payload: String,
    }
    #[derive(serde::Deserialize)]
    struct Msg {
        #[serde(rename = "type")]
        kind: String,
        #[serde(default)]
        query_id: u64,
        #[serde(default)]
        items: Vec<Item>,
        #[serde(default)]
        id: u64,
        #[serde(default)]
        capability: String,
        #[serde(default)]
        params: serde_json::Value,
        #[serde(default, rename = "ui_id")]
        ui_id: u64,
        #[serde(default)]
        spec: serde_json::Value,
    }
    let msg = serde_json::from_str::<Msg>(line).ok()?;
    match msg.kind.as_str() {
        "results" => {
            if msg.items.is_empty() {
                return None;
            }
            let items = msg
                .items
                .into_iter()
                .take(1000) // 单条 results 行条数上限（审查 M-1）
                .map(|i| PanelEntry {
                    title: i.title,
                    subtitle: i.subtitle,
                    icon_spec: i.icon,
                    kind: "plugin",
                    payload: i.payload,
                })
                .collect();
            Some(PluginEvent::Results {
                plugin: plugin.to_string(),
                query_id: msg.query_id,
                items,
            })
        }
        "capability_request" => {
            if msg.capability.is_empty() {
                return None;
            }
            Some(PluginEvent::Capability(CapRequest {
                plugin: plugin.to_string(),
                gen,
                request_id: msg.id,
                capability: msg.capability,
                params: msg.params,
            }))
        }
        "ui.show" => {
            if msg.ui_id == 0 {
                return None;
            }
            Some(PluginEvent::UiShow {
                plugin: plugin.to_string(),
                gen,
                ui_id: msg.ui_id,
                spec: msg.spec,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_results_and_capability_request() {
        let r = parse_line(
            "p",
            7,
            r#"{"type":"results","query_id":3,"items":[{"title":"t","payload":"x"}]}"#,
        )
        .unwrap();
        match r {
            PluginEvent::Results { plugin, query_id, items } => {
                assert_eq!((plugin.as_str(), query_id, items.len()), ("p", 3, 1));
            }
            _ => panic!("应为 Results"),
        }

        let r = parse_line(
            "p",
            7,
            r#"{"type":"capability_request","id":42,"capability":"settings.write","params":{"schema":"s"}}"#,
        )
        .unwrap();
        match r {
            PluginEvent::Capability(req) => {
                assert_eq!((req.plugin.as_str(), req.gen, req.request_id), ("p", 7, 42));
                assert_eq!(req.capability, "settings.write");
            }
            _ => panic!("应为 Capability"),
        }
        assert!(parse_line("p", 1, r#"{"type":"ready"}"#).is_none());
        assert!(parse_line("p", 1, "not json").is_none());
        assert!(parse_line("p", 1, r#"{"type":"capability_request"}"#).is_none());
    }

    // ---- 端到端：真实 bwrap + 假 python 插件 ----
    //
    // 装置说明：`acc` 跨步骤累积能力请求、`replied` 标记已回复——
    // 插件对下一查询的请求可能在上一等待窗口内到达，随局部变量丢弃
    // 会出现「读线程明明收到了、谓词永远等不到」的竞态（实测踩坑）。

    const FAKE_PY: &str = r#"#!/usr/bin/python3
import sys, json
def send(o):
    sys.stdout.write(json.dumps(o) + "\n")
    sys.stdout.flush()
pending = {}
for line in sys.stdin:
    m = json.loads(line)
    t = m.get("type")
    if t == "init":
        send({"type": "ready"})
    elif t == "query":
        text = m["text"]
        if text == "files":
            n = len(m.get("context", {}).get("files", []))
            send({"type": "results", "query_id": m["id"],
                  "items": [{"title": f"files:{n}", "payload": "p"}]})
            continue
        if text == "set":
            rid = 2000 + m["id"]
            pending[rid] = m["id"]
            send({"type": "capability_request", "id": rid,
                  "capability": "settings.write",
                  "params": {"schema": "org.gnome.desktop.interface",
                             "key": "color-scheme", "value": "prefer-dark"}})
            continue
        if text == "setbad":
            rid = 3000 + m["id"]
            pending[rid] = m["id"]
            send({"type": "capability_request", "id": rid,
                  "capability": "settings.write",
                  "params": {"schema": "org.gnome.shell.extensions.dash",
                             "key": "x", "value": "y"}})
            continue
        if text == "read":
            rid = 5000 + m["id"]
            pending[rid] = m["id"]
            send({"type": "capability_request", "id": rid,
                  "capability": "settings.read",
                  "params": {"schema": "org.gnome.settings-daemon.plugins.media-keys",
                             "key": "custom-keybindings"}})
            continue
        if text == "write":
            rid = 4000 + m["id"]
            pending[rid] = m["id"]
            send({"type": "capability_request", "id": rid,
                  "capability": "fs.write",
                  "params": {"path": "~/.yihu-e2e-test/sub/data.json", "text": "{\"ok\": true}"}})
            continue
        rid = 1000 + m["id"]
        if text == "shot":
            cap = "screenshot.take"
        else:
            cap = "clipboard.write" if text != "notify" else "notify"
        send({"type": "capability_request", "id": rid, "capability": cap,
              "params": {"text": "hello", "summary": "s"}})
        pending[rid] = m["id"]
    elif t == "capability_response":
        rid = m.get("id")
        if rid in pending:
            qid = pending.pop(rid)
            ok = m.get("ok", False)
            title = "granted" if ok else "denied:" + m.get("error", "?")
            data = m.get("data")
            if data is not None:
                title += ":" + str(data)[:60]
            send({"type": "results", "query_id": qid,
                  "items": [{"title": title, "payload": "p"}]})
    elif t == "shutdown":
        sys.exit(0)
"#;

    fn python3_available() -> bool {
        std::process::Command::new("python3")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// fake-plugin：多能力声明（含带参）；plain-plugin：零权限 + 常驻
    fn make_registry(base: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        for (id, name, perms, resident) in [
            (
                "fake-plugin",
                "假插件",
                "[\"clipboard.write\", \"selected_files.read\", \"settings.write@org.gnome.desktop.interface\", \"fs.write@~/.yihu-e2e-test/**/*.json\", \"settings.read@org.gnome.settings-daemon.plugins.media-keys\"]",
                false,
            ),
            ("plain-plugin", "素插件", "[]", true),
        ] {
            let dir = base.join(id);
            std::fs::create_dir_all(&dir).unwrap();
            let resident_line = if resident { "resident = true\n" } else { "" };
            std::fs::write(
                dir.join("manifest.toml"),
                format!("id = \"{id}\"\nname = \"{name}\"\napi = \"^1\"\nentry = \"plugin.py\"\npermissions = {perms}\n{resident_line}"),
            )
            .unwrap();
            std::fs::write(dir.join("plugin.py"), FAKE_PY).unwrap();
            std::fs::set_permissions(
                dir.join("plugin.py"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
    }

    /// 反复 drain 直到条件满足（超时 panic）。acc 跨步骤累积。
    fn wait_for(
        mgr: &mut PluginMgr,
        acc: &mut Vec<CapRequest>,
        timeout: Duration,
        mut cond: impl FnMut(&PluginMgr, &mut Vec<CapRequest>) -> bool,
    ) {
        let deadline = Instant::now() + timeout;
        loop {
            let (_, mut more, _) = mgr.drain();
            acc.append(&mut more);
            if cond(mgr, acc) {
                return;
            }
            if Instant::now() > deadline {
                panic!("超时：acc={acc:?} rows={:?}", mgr.latest_rows());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// 只等结果行的等待（无能力请求）
    fn wait_rows(mgr: &mut PluginMgr, timeout: Duration, mut cond: impl FnMut(&PluginMgr) -> bool) {
        let deadline = Instant::now() + timeout;
        loop {
            mgr.drain();
            if cond(mgr) {
                return;
            }
            if Instant::now() > deadline {
                panic!("超时：rows={:?}", mgr.latest_rows());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    type Replied = std::collections::HashSet<(String, u64, u64)>;

    /// 找一条匹配且未回复的请求
    fn find_unreplied<'a>(
        acc: &'a [CapRequest],
        replied: &Replied,
        mut pred: impl FnMut(&CapRequest) -> bool,
    ) -> Option<&'a CapRequest> {
        acc.iter()
            .find(|r| !replied.contains(&(r.plugin.clone(), r.gen, r.request_id)) && pred(r))
    }

    fn reply(mgr: &mut PluginMgr, replied: &mut Replied, req: &CapRequest, ok: bool, error: &str) {
        reply_data(mgr, replied, req, ok, error, None);
    }

    fn reply_data(
        mgr: &mut PluginMgr,
        replied: &mut Replied,
        req: &CapRequest,
        ok: bool,
        error: &str,
        data: Option<&str>,
    ) {
        mgr.respond_gen(&req.plugin, req.gen, req.request_id, ok, error, data);
        replied.insert((req.plugin.clone(), req.gen, req.request_id));
    }

    #[test]
    fn capability_roundtrip_end_to_end() {
        if !crate::sandbox::available() || !python3_available() {
            eprintln!("跳过：缺 bwrap 或 python3");
            return;
        }
        let base = std::env::temp_dir().join(format!("yihu-sess-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        make_registry(&base);
        let mut mgr = PluginMgr::new();
        mgr.ensure_sessions_in(&base, &base);
        let mut acc: Vec<CapRequest> = Vec::new();
        let mut replied: Replied = Replied::new();

        // ① 未声明能力（notify）→ 拒绝回环
        mgr.broadcast("notify");
        wait_for(&mut mgr, &mut acc, Duration::from_secs(10), |_, c| {
            find_unreplied(c, &replied, |r| r.capability == "notify").is_some()
        });
        let req1 = find_unreplied(&acc, &replied, |r| r.capability == "notify")
            .unwrap()
            .clone();
        assert!(req1.gen > 0);
        reply(&mut mgr, &mut replied, &req1, false, "manifest 未声明能力 notify");
        wait_rows(&mut mgr, Duration::from_secs(10), |m| {
            m.latest_rows().iter().any(|r| r.title.starts_with("denied:manifest 未声明能力 notify"))
        });

        // ② 已声明能力（clipboard.write）→ 授权回环
        mgr.broadcast("copy");
        wait_for(&mut mgr, &mut acc, Duration::from_secs(10), |_, c| {
            find_unreplied(c, &replied, |r| r.capability == "clipboard.write").is_some()
        });
        let req2 = find_unreplied(&acc, &replied, |r| r.capability == "clipboard.write")
            .unwrap()
            .clone();
        reply(&mut mgr, &mut replied, &req2, true, "");
        wait_rows(&mut mgr, Duration::from_secs(10), |m| {
            m.latest_rows().iter().any(|r| r.title == "granted")
        });

        // ③ context 门控：声明 selected_files.read 的会话拿到文件，零权限拿到空
        mgr.set_context_files(vec!["/tmp/a.txt".into(), "/tmp/b.txt".into()]);
        mgr.broadcast("files");
        wait_rows(&mut mgr, Duration::from_secs(10), |m| {
            let titles: Vec<String> = m.latest_rows().iter().map(|r| r.title.clone()).collect();
            titles.contains(&"files:2".to_string()) && titles.contains(&"files:0".to_string())
        });

        // ③b settings.write：声明内 schema → 授权；越权 schema → 拒绝
        mgr.broadcast("set");
        wait_for(&mut mgr, &mut acc, Duration::from_secs(10), |_, c| {
            find_unreplied(c, &replied, |r| {
                r.capability == "settings.write"
                    && r.params.get("schema").and_then(|v| v.as_str())
                        == Some("org.gnome.desktop.interface")
            })
            .is_some()
        });
        let sw = find_unreplied(&acc, &replied, |r| {
            r.capability == "settings.write"
                && r.params.get("schema").and_then(|v| v.as_str())
                    == Some("org.gnome.desktop.interface")
        })
        .unwrap()
        .clone();
        assert_eq!(sw.params["key"], "color-scheme");
        reply(&mut mgr, &mut replied, &sw, true, "");
        wait_rows(&mut mgr, Duration::from_secs(10), |m| {
            m.latest_rows().iter().any(|r| r.title == "granted")
        });

        mgr.broadcast("setbad");
        wait_for(&mut mgr, &mut acc, Duration::from_secs(10), |_, c| {
            find_unreplied(c, &replied, |r| {
                r.capability == "settings.write"
                    && r.params.get("schema").and_then(|v| v.as_str())
                        == Some("org.gnome.shell.extensions.dash")
            })
            .is_some()
        });
        let bad = find_unreplied(&acc, &replied, |r| {
            r.capability == "settings.write"
                && r.params.get("schema").and_then(|v| v.as_str())
                    == Some("org.gnome.shell.extensions.dash")
        })
        .unwrap()
        .clone();
        reply(&mut mgr, &mut replied, &bad, false, "schema 不在声明白名单");
        wait_rows(&mut mgr, Duration::from_secs(10), |m| {
            m.latest_rows().iter().any(|r| r.title.contains("schema 不在声明白名单"))
        });

        // ③c fs.write：声明 glob 内路径 → 授权；越界路径 → 拒绝
        mgr.broadcast("write");
        wait_for(&mut mgr, &mut acc, Duration::from_secs(10), |_, c| {
            find_unreplied(c, &replied, |r| {
                r.capability == "fs.write"
                    && r.params.get("path").and_then(|v| v.as_str())
                        == Some("~/.yihu-e2e-test/sub/data.json")
            })
            .is_some()
        });
        let fw = find_unreplied(&acc, &replied, |r| r.capability == "fs.write")
            .unwrap()
            .clone();
        reply(&mut mgr, &mut replied, &fw, true, "");
        wait_rows(&mut mgr, Duration::from_secs(10), |m| {
            m.latest_rows().iter().any(|r| r.title.starts_with("granted"))
        });

        // ③d settings.read：data 回传（gsettings get 的真实输出）
        mgr.broadcast("read");
        wait_for(&mut mgr, &mut acc, Duration::from_secs(10), |_, c| {
            find_unreplied(c, &replied, |r| r.capability == "settings.read").is_some()
        });
        let rd = find_unreplied(&acc, &replied, |r| r.capability == "settings.read")
            .unwrap()
            .clone();
        // 值文本 JSON 回传（gsettings get custom-keybindings 的 GVariant 列表）
        let val = serde_json::to_string(&format!("{:?}", "[]"))
            .unwrap_or_else(|_| "\"[\"".into());
        reply_data(&mut mgr, &mut replied, &rd, true, "", Some(&val));
        wait_rows(&mut mgr, Duration::from_secs(10), |m| {
            m.latest_rows().iter().any(|r| r.title.starts_with("granted:"))
        });

        // ④ 未声明 screenshot.take → 拒绝路径（不触发真实截屏）
        mgr.broadcast("shot");
        wait_for(&mut mgr, &mut acc, Duration::from_secs(10), |_, c| {
            find_unreplied(c, &replied, |r| r.capability == "screenshot.take").is_some()
        });
        let shot = find_unreplied(&acc, &replied, |r| r.capability == "screenshot.take")
            .unwrap()
            .clone();
        reply(&mut mgr, &mut replied, &shot, false, "manifest 未声明能力 screenshot.take");
        wait_rows(&mut mgr, Duration::from_secs(10), |m| {
            m.latest_rows().iter().any(|r| r.title.contains("screenshot.take"))
        });

        // ⑤ 常驻（resident）：kill_all 后 plain 存活、fake 已死；空闲 shutdown 自退不计失败
        mgr.kill_all();
        assert!(!mgr.is_session_alive("fake-plugin"));
        assert!(mgr.is_session_alive("plain-plugin"));
        mgr.sweep_idle_with(Duration::ZERO, Duration::from_secs(10));
        wait_rows(&mut mgr, Duration::from_secs(10), |m| {
            !m.is_session_alive("plain-plugin")
        });
        assert_eq!(mgr.failures_of("plain-plugin"), 0, "自退不是失败");

        // ⑥ 收起即清：全部会话结束后再注入并广播，无任何应答
        mgr.set_context_files(vec!["/tmp/a.txt".into()]);
        mgr.broadcast("files");
        assert!(mgr.latest_rows().is_empty());
        assert!(mgr.permissions_of("fake-plugin").is_none());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn oversized_line_kills_session() {
        if !crate::sandbox::available() || !python3_available() {
            eprintln!("跳过：缺 bwrap 或 python3");
            return;
        }
        let base = std::env::temp_dir().join(format!("yihu-sess-huge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        make_registry(&base);
        let py = base.join("fake-plugin/plugin.py");
        let mut src = std::fs::read_to_string(&py).unwrap();
        src = src.replace(
            "        if text == \"files\":",
            "        if text == \"huge\":\n            send({\"type\": \"results\", \"query_id\": m[\"id\"],\n                  \"items\": [{\"title\": \"x\" * (2 * 1024 * 1024), \"payload\": \"p\"}]})\n            continue\n        if text == \"files\":",
        );
        std::fs::write(&py, src).unwrap();

        let mut mgr = PluginMgr::new();
        mgr.ensure_sessions_in(&base, &base);
        mgr.broadcast("huge");
        let deadline = Instant::now() + Duration::from_secs(10);
        while mgr.is_session_alive("fake-plugin") && Instant::now() < deadline {
            mgr.drain();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(mgr.failures_of("fake-plugin") >= 1, "超长行 = 协议违约，应计失败");
        mgr.kill_all();
        let _ = std::fs::remove_dir_all(&base);
    }
}
