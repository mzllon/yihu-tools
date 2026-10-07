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
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use yihu_core::plugins;

use crate::app::PanelEntry;

const MAX_FAILURES: usize = 4;
const FAILURE_WINDOW: Duration = Duration::from_secs(5 * 60);
/// 常驻会话空闲自退阈值（M4 冻结决策 #7：socket-activation 列 M5，
/// v1 = 收起不杀 + 空闲发 shutdown 自退）
pub const RESIDENT_IDLE_EXIT: Duration = Duration::from_secs(300);
/// shutdown 后的宽限：仍不退出则组级 SIGKILL
pub const RESIDENT_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

pub enum PluginEvent {
    Results { plugin: String, query_id: u64, items: Vec<PanelEntry> },
    Exited { plugin: String, gen: u64 },
    Capability(CapRequest),
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
    /// manifest 声明的能力集（安装时已过词表白名单），能力代理按它强制
    permissions: HashSet<String>,
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
        let permissions: HashSet<String> = inst.manifest.permissions.iter().cloned().collect();
        let tx = self.tx.clone();
        let reader_id = id.clone();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(l) => {
                        if let Some(ev) = parse_line(&reader_id, gen, &l) {
                            if tx.send(ev).is_err() {
                                break;
                            }
                        }
                    }
                    Err(_) => break,
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

    /// 查询某插件会话声明的能力集（会话不存在 = None，请求无从发起）。
    pub fn permissions_of(&self, plugin: &str) -> Option<&HashSet<String>> {
        self.sessions.iter().find(|s| s.id == plugin).map(|s| &s.permissions)
    }

    /// 回复能力请求（capability_response）。会话已死则静默丢弃。
    pub fn respond(&mut self, plugin: &str, request_id: u64, ok: bool, error: &str) {
        let line = if ok {
            format!("{{\"type\":\"capability_response\",\"id\":{request_id},\"ok\":true}}")
        } else {
            format!(
                "{{\"type\":\"capability_response\",\"id\":{request_id},\"ok\":false,\"error\":{}}}",
                serde_json::to_string(error).unwrap_or_else(|_| "\"\"".into())
            )
        };
        self.send_all_to(plugin, &line);
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
                        .contains(yihu_core::permissions::SELECTED_FILES_READ),
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
        let keep: Vec<usize> = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| s.resident)
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

    /// 某插件当前滑动窗口内的失败计数（同上）
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn failures_of(&self, plugin: &str) -> usize {
        self.recent_failures(plugin)
    }

    /// 主循环 tick 调用：处理结果/退出/能力请求事件。
    /// 返回（是否有影响展示的变化, 待处理能力请求列表）。
    /// 能力请求在 drain 返回后处理（执行/回复会重借 mgr，不能在借用中回调）。
    pub fn drain(&mut self) -> (bool, Vec<CapRequest>) {
        let mut dirty = false;
        let mut caps = Vec::new();
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
                Ok(PluginEvent::Capability(cap)) => caps.push(cap),
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
        (dirty, caps)
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
            // M4 穿插项：落盘持久禁用（重启后仍禁用；中心「插件」页开关可恢复）
            let mut st = yihu_core::plugins::PluginsState::load();
            st.set_disabled(plugin, true);
            if let Err(e) = st.save_to(&yihu_core::plugins::state_path()) {
                eprintln!("yihu-panel: 崩溃禁用落盘失败：{e}");
            }
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
            r#"{"type":"capability_request","id":42,"capability":"clipboard.write","params":{"text":"hi"}}"#,
        )
        .unwrap();
        match r {
            PluginEvent::Capability(req) => {
                assert_eq!((req.plugin.as_str(), req.gen, req.request_id), ("p", 7, 42));
                assert_eq!(req.capability, "clipboard.write");
                assert_eq!(req.params["text"], "hi");
            }
            _ => panic!("应为 Capability"),
        }

        // 非协议行与畸形行忽略
        assert!(parse_line("p", 1, r#"{"type":"ready"}"#).is_none());
        assert!(parse_line("p", 1, "not json").is_none());
        assert!(parse_line("p", 1, r#"{"type":"capability_request"}"#).is_none());
    }

    // ---- 端到端：真实 bwrap + 假 python 插件走完整能力环 ----

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
        rid = 1000 + m["id"]
        cap = "clipboard.write" if text != "notify" else "notify"
        send({"type": "capability_request", "id": rid, "capability": cap,
              "params": {"text": "hello", "summary": "s"}})
        pending[rid] = m["id"]
    elif t == "capability_response":
        qid = pending.pop(m["id"], None)
        if qid is not None:
            ok = m.get("ok", False)
            title = "granted" if ok else "denied:" + m.get("error", "?")
            send({"type": "results", "query_id": qid,
                  "items": [{"title": title, "payload": "p"}]})
    elif t == "shutdown":
        sys.exit(0)  # 常驻空闲自退（M4 二期②）
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

    /// fake-plugin：声明 clipboard.write + selected_files.read；
    /// plain-plugin：零权限 + 常驻（验证 context 门控、能力拒绝与
    /// resident 空闲自退）
    fn make_registry(base: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        for (id, name, perms, resident) in [
            (
                "fake-plugin",
                "假插件",
                "[\"clipboard.write\", \"selected_files.read\"]",
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
            // 与 install_from_dir 语义一致：入口必须可执行（bwrap 直接 execvp）
            std::fs::set_permissions(
                dir.join("plugin.py"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
    }

    /// 反复 drain 直到条件满足（超时 panic）。注意 latest 保留旧查询的
    /// 结果（防闪烁语义），跨查询等待必须用谓词而非「非空」。
    fn wait_for(
        mgr: &mut PluginMgr,
        timeout: Duration,
        mut cond: impl FnMut(&PluginMgr, &mut Vec<CapRequest>) -> bool,
    ) -> Vec<CapRequest> {
        let deadline = Instant::now() + timeout;
        let mut caps = Vec::new();
        loop {
            let (_, mut more) = mgr.drain();
            caps.append(&mut more);
            if cond(mgr, &mut caps) {
                return caps;
            }
            if Instant::now() > deadline {
                panic!("超时：cap={caps:?} rows={:?}", mgr.latest_rows());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
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

        // ① 未声明能力（notify）→ 拒绝回环：deny 文案原样回给插件
        mgr.broadcast("notify");
        let caps = wait_for(&mut mgr, Duration::from_secs(10), |_, c| !c.is_empty());
        assert_eq!(caps[0].capability, "notify");
        assert!(caps[0].gen > 0);
        let req1 = (caps[0].plugin.clone(), caps[0].request_id);
        mgr.respond(&req1.0, req1.1, false, "manifest 未声明能力 notify");
        wait_for(&mut mgr, Duration::from_secs(10), |m, _| {
            m.latest_rows().iter().any(|r| r.title.starts_with("denied:manifest 未声明能力 notify"))
        });

        // ② 已声明能力（clipboard.write）→ 授权回环
        mgr.broadcast("copy");
        let caps = wait_for(&mut mgr, Duration::from_secs(10), |_, c| !c.is_empty());
        assert_eq!(caps[0].capability, "clipboard.write");
        mgr.respond(&caps[0].plugin, caps[0].request_id, true, "");
        wait_for(&mut mgr, Duration::from_secs(10), |m, _| {
            m.latest_rows().iter().any(|r| r.title == "granted")
        });

        // ③ context 门控：声明 selected_files.read 的会话拿到文件，零权限
        //    会话拿到空 context（两条结果并存可对照）
        mgr.set_context_files(vec!["/tmp/a.txt".into(), "/tmp/b.txt".into()]);
        mgr.broadcast("files");
        wait_for(&mut mgr, Duration::from_secs(10), |m, _| {
            let titles: Vec<String> = m.latest_rows().iter().map(|r| r.title.clone()).collect();
            titles.contains(&"files:2".to_string()) && titles.contains(&"files:0".to_string())
        });
        // ⑤ 常驻（resident）：kill_all 后 plain-plugin 存活、fake-plugin
        //    已死；空闲超阈值立即 shutdown → 插件 exit(0) 自退且不计失败
        mgr.kill_all();
        assert!(!mgr.is_session_alive("fake-plugin"));
        assert!(mgr.is_session_alive("plain-plugin"));
        mgr.sweep_idle_with(Duration::ZERO, Duration::from_secs(10));
        wait_for(&mut mgr, Duration::from_secs(10), |m, _| {
            !m.is_session_alive("plain-plugin")
        });
        assert_eq!(mgr.failures_of("plain-plugin"), 0, "自退不是失败");

        // ⑥ 收起即清：全部会话结束后再注入并广播，无任何应答
        mgr.set_context_files(vec!["/tmp/a.txt".into()]);
        mgr.broadcast("files");
        assert!(mgr.latest_rows().is_empty(), "会话已收起，不应有结果");
        assert!(mgr.permissions_of("fake-plugin").is_none());
        let _ = std::fs::remove_dir_all(&base);
    }
}
