//! 外部插件会话管理：呼出拉起、查询广播、结果回传、收起杀灭。
//!
//! 生命周期：呼出（summon）拉起全部启用插件的进程并发送 init；
//! 每次 keystroke 向各会话广播 query（行式 JSON）；
//! 面板收起（hide）时对进程组 SIGKILL 整组杀灭 → 待命零进程零内存。
//! 插件连续退出（5 分钟内 4 次）则本会话禁用，避免反复崩溃刷屏。

use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use mt_core::plugins;

use crate::app::PanelEntry;

const MAX_FAILURES: usize = 4;
const FAILURE_WINDOW: Duration = Duration::from_secs(5 * 60);

pub enum PluginEvent {
    Results { plugin: String, query_id: u64, items: Vec<PanelEntry> },
    Exited { plugin: String },
}

struct Session {
    id: String,
    child: Child,
    stdin: ChildStdin,
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
    /// 当前查询（文本, id）——过期结果丢弃
    current_query: Option<(String, u64)>,
    /// 各插件对当前查询的最新结果（query_id, rows）；未超期的旧结果保留，避免闪烁
    latest: HashMap<String, (u64, Vec<PanelEntry>)>,
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
            current_query: None,
            latest: HashMap::new(),
        }
    }

    /// 呼出时调用：确保全部启用插件的会话存活（幂等）。
    /// 崩溃超限的插件本次运行期跳过；持久禁用走中心「插件」页。
    pub fn ensure_sessions(&mut self) {
        let debug = std::env::var_os("YIHU_PANEL_DEBUG").is_some();
        let state = mt_core::plugins::PluginsState::load();
        let (installed, errors) = plugins::list_installed();
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
            match self.spawn_session(&inst) {
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

    fn spawn_session(&mut self, inst: &plugins::Installed) -> io::Result<Session> {
        let id = inst.manifest.id.clone();
        let entry = inst.entry_path();
        let mut child = Command::new(&entry)
            .current_dir(&inst.dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0) // 组级杀灭，不留孤儿
            .spawn()?;
        let mut stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");
        let data_dir = inst.dir.display().to_string();
        writeln!(
            stdin,
            "{{\"type\":\"init\",\"api\":1,\"data_dir\":{}}}",
            serde_json::to_string(&data_dir).expect("路径是合法 JSON 字符串")
        )?;
        let tx = self.tx.clone();
        let reader_id = id.clone();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(l) => {
                        if let Some(ev) = parse_line(&reader_id, &l) {
                            if tx.send(ev).is_err() {
                                break;
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = tx.send(PluginEvent::Exited { plugin: reader_id });
        });
        Ok(Session { id, child, stdin })
    }

    /// 每次 keystroke：向全部会话广播查询。空文本清空插件结果。
    pub fn broadcast(&mut self, text: &str) {
        if text.is_empty() {
            self.latest.clear();
            self.current_query = None;
            return;
        }
        self.query_seq += 1;
        let id = self.query_seq;
        self.current_query = Some((text.to_string(), id));
        let line = format!(
            "{{\"type\":\"query\",\"id\":{id},\"text\":{}}}",
            serde_json::to_string(text).expect("文本是合法 JSON 字符串")
        );
        self.send_all(&line);
    }

    /// 收起时调用：进程组级 SIGKILL，整组杀灭。
    pub fn kill_all(&mut self) {
        for s in &mut self.sessions {
            unsafe {
                libc::kill(-(s.child.id() as i32), libc::SIGKILL);
            }
            let _ = s.child.wait();
        }
        self.sessions.clear();
        self.latest.clear();
        self.current_query = None;
    }

    /// 主循环 tick 调用：处理结果/退出事件。
    /// 返回是否有影响展示的变化（新结果 / 插件退出）。
    pub fn drain(&mut self) -> bool {
        let mut dirty = false;
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
                Ok(PluginEvent::Exited { plugin }) => {
                    self.record_failure(&plugin);
                    // 回收子进程并移除死会话；摘除其结果
                    self.sessions.retain_mut(|s| {
                        if s.id == plugin {
                            let _ = s.child.wait();
                            false
                        } else {
                            true
                        }
                    });
                    if self.latest.remove(&plugin).is_some() {
                        dirty = true;
                    }
                }
                Err(_) => break,
            }
        }
        dirty
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
            if s.stdin.write_all(line.as_bytes()).is_ok()
                && s.stdin.write_all(b"\n").is_ok()
                && s.stdin.flush().is_ok()
            {
                return;
            }
        }
    }

    fn send_all(&mut self, line: &str) {
        let dead: Vec<String> = self
            .sessions
            .iter_mut()
            .filter_map(|s| {
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
        }
    }
}

/// 解析插件输出行。非 results 行（ready 等）与解析失败返回 None。
fn parse_line(plugin: &str, line: &str) -> Option<PluginEvent> {
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
    }
    let msg = serde_json::from_str::<Msg>(line).ok()?;
    if msg.kind != "results" || msg.items.is_empty() {
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
