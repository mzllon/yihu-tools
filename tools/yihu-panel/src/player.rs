//! 电台播放执行端（M5 电台插件化）：宿主代播的后端。
//!
//! 插件沙箱无网无声——播放全部发生在宿主（面板守护进程）侧：
//! gst-play-1.0 子进程 + 直播清单本地镜像。守护进程常驻，收起面板
//! 音乐不断；守护进程退出（Drop）即停播，满足「零孤儿」约束。
//!
//! 为什么镜像清单：蜻蜓直播清单里的分片是协议相对地址（`//host/...`），
//! GStreamer 的 HLS 解析器会错误拼接为「错误主机 + 路径」导致分片 404、
//! 永远无法预滚（浏览器按标准解析所以正常）。宿主把清单改写为绝对地址
//! 落到本地文件，播放器消费本地清单即可正常拉流。
//!
//! 音量为「按流单独控制」：经 pactl 定位本播放进程的 sink-input，
//! 只调本播放的音量，不影响系统与其他应用。
//!
//! owner 跟踪：play() 记录发起播放的插件会话 id，供 tick 检查——
//! 属主会话死亡（崩溃/禁用重启）即停播，避免孤儿音频。

use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

pub const PLAYER_LOG: &str = "/tmp/yihu-radio-player.log";
/// 本地镜像清单路径；播放器消费它，镜像线程持续刷新。
pub const MIRROR_MANIFEST: &str = "/tmp/yihu-radio/live.m3u8";
/// 清单刷新间隔；蜻蜓直播清单目标分片时长约 7s。
const MIRROR_INTERVAL: Duration = Duration::from_secs(4);

/// 播放参数；单独抽出便于测试。
/// `--flags=audio` 把 playbin 收窄为纯音频：直播 TS 流会被 typefind
/// 识别为 video/mpegts，默认会拉起视频窗口（黑窗），去掉视频分支即可。
pub fn gst_args(manifest_uri: &str) -> Vec<String> {
    vec!["--flags=audio".to_owned(), manifest_uri.to_owned()]
}

/// 把清单中的相对地址改写为绝对地址。
/// `//host/path` → 补 scheme；`/path` → 补 scheme+host；其余原样保留。
/// 分片行才会以 `/` 开头，`#` 与空行不受影响。
pub fn absolutize_manifest(manifest: &str, base_uri: &str) -> String {
    let (scheme, authority) = match base_uri.split_once("://") {
        Some((scheme, rest)) => {
            let authority = rest.split('/').next().unwrap_or_default().to_owned();
            (scheme.to_owned(), authority)
        }
        None => ("https".to_owned(), String::new()),
    };
    manifest
        .lines()
        .map(|line| {
            let trimmed = line.trim_end_matches('\r');
            if let Some(rest) = trimmed.strip_prefix("//") {
                format!("{scheme}://{rest}")
            } else if trimmed.starts_with('/') {
                format!("{scheme}://{authority}{trimmed}")
            } else {
                trimmed.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

/// 原子化写入镜像清单，避免播放器读到半个文件。
fn write_manifest_atomic(path: &str, text: &str) -> std::io::Result<()> {
    let path = std::path::Path::new(path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("m3u8.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// 阻塞式 GET 文本（清单镜像线程用）。gio TLS 由 glib-networking 提供。
fn fetch_text(uri: &str) -> Result<String, String> {
    use gtk::gio::prelude::FileExtManual;
    let (bytes, _) = gtk::gio::File::for_uri(uri)
        .load_contents(gtk::gio::Cancellable::NONE)
        .map_err(|e| e.to_string())?;
    String::from_utf8(bytes.to_vec()).map_err(|_| "响应不是有效 UTF-8".to_owned())
}

/// 从 `pactl list sink-inputs` 输出中定位属于 pid 的 sink-input 序号。
/// 输出形如：
/// ```text
/// Sink Input #268
///     ...
///     application.process.id = "598467"
/// ```
pub fn find_sink_input(pactl_output: &str, pid: u32) -> Option<u32> {
    let needle = format!("application.process.id = \"{pid}\"");
    pactl_output
        .split("Sink Input #")
        .filter_map(|block| {
            let idx: String = block
                .chars()
                .skip_while(|c| !c.is_ascii_digit())
                .take_while(char::is_ascii_digit)
                .collect();
            idx.parse::<u32>().ok().map(|idx| (idx, block))
        })
        .find(|(_, block)| block.contains(&needle))
        .map(|(idx, _)| idx)
}

fn has_command(cmd: &str) -> bool {
    Command::new(cmd)
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn has_gst_element(name: &str) -> bool {
    Command::new("gst-inspect-1.0")
        .arg(name)
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// 电台播放缺失的系统组件（apt 包名）。
pub fn probe_missing() -> Vec<&'static str> {
    let mut missing = Vec::new();
    if !has_command("gst-play-1.0") {
        missing.push("gstreamer1.0-tools");
    }
    if !has_gst_element("tsdemux") {
        missing.push("gstreamer1.0-plugins-bad");
    }
    if !has_gst_element("avdec_aac") {
        missing.push("gstreamer1.0-libav");
    }
    if !has_gst_element("pulsesink") {
        missing.push("gstreamer1.0-plugins-good");
    }
    missing
}

/// 清理本用户遗留的孤儿播放进程（宿主被强制终止时 gst-play 会存活）。
/// gst-play-1.0 极少被其他程序使用，按进程名精确匹配即可安全清理。
pub fn kill_orphan_players() {
    let Ok(entries) = std::fs::read_dir("/proc") else { return };
    let self_uid = std::fs::metadata("/proc/self")
        .map(|meta| std::os::unix::fs::MetadataExt::uid(&meta))
        .unwrap_or(u32::MAX);
    for entry in entries.flatten() {
        let Some(pid) = entry.file_name().to_string_lossy().parse::<u32>().ok() else {
            continue;
        };
        if pid == std::process::id() {
            continue;
        }
        let Ok(comm) = std::fs::read_to_string(format!("/proc/{pid}/comm")) else {
            continue;
        };
        if comm.trim() != "gst-play-1.0" {
            continue;
        }
        let owned = std::fs::metadata(format!("/proc/{pid}"))
            .map(|meta| std::os::unix::fs::MetadataExt::uid(&meta) == self_uid)
            .unwrap_or(false);
        if owned {
            let _ = Command::new("kill")
                .args(["-9", &pid.to_string()])
                .status();
        }
    }
}

/// Sync 安全的 RadioPlayer：跨线程共享（Arc）给能力作业线程与主循环。
#[derive(Default)]
pub struct RadioPlayer {
    child: Mutex<Option<Child>>,
    /// 播放流在声音服务器中的 sink-input 序号缓存；切台/停止后失效。
    sink_index: Mutex<Option<u32>>,
    pactl_ok: Mutex<bool>,
    mirror_stop: Mutex<Option<std::sync::Arc<AtomicBool>>>,
    /// 发起当前播放的插件会话 id（kill_all/会话死亡停播的依据）。
    owner: Mutex<Option<String>>,
}

impl RadioPlayer {
    pub fn new() -> Self {
        Self::default()
    }

    /// 播放前探测运行环境，缺失时返回可安装的 apt 包名列表。
    pub fn probe(&self) -> Result<(), String> {
        let missing = probe_missing();
        if missing.is_empty() {
            return Ok(());
        }
        Err(format!(
            "缺少播放组件：{}（请安装：sudo apt install {}）",
            missing.join("、"),
            missing.join(" ")
        ))
    }

    /// 播放 `stream_url`：清理孤儿播放进程与陈旧清单，启动清单镜像线程，
    /// 首份新清单落地后（通常 <1s，离线时最多等 5s）拉起 gst-play 子进程。
    /// **阻塞可达 5s——只能在后台线程调用**（能力作业线程）。
    pub fn play(&self, stream_url: &str, owner: &str) -> Result<(), String> {
        self.probe()?;
        self.stop();
        kill_orphan_players();
        // 陈旧清单会让播放器拉起后拉到 404 旧分片（表现为无声），必须删除。
        let _ = std::fs::remove_file(MIRROR_MANIFEST);

        let stop = std::sync::Arc::new(AtomicBool::new(false));
        {
            let mut slot = self.mirror_stop.lock().unwrap();
            *slot = Some(stop.clone());
        }
        let stream_url = stream_url.to_owned();
        let stop_for_thread = stop.clone();
        std::thread::Builder::new()
            .name("yihu-radio-mirror".into())
            .spawn(move || {
                loop {
                    match fetch_text(&stream_url) {
                        Ok(text) => {
                            let fixed = absolutize_manifest(&text, &stream_url);
                            let _ = write_manifest_atomic(MIRROR_MANIFEST, &fixed);
                        }
                        Err(e) => {
                            eprintln!("[电台] 清单镜像获取失败：{e}");
                        }
                    }
                    if stop_for_thread.load(Ordering::Acquire) {
                        break;
                    }
                    std::thread::sleep(MIRROR_INTERVAL);
                }
            })
            .map_err(|e| format!("无法启动清单镜像：{e}"))?;

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::fs::metadata(MIRROR_MANIFEST).is_err() {
            if std::time::Instant::now() > deadline {
                stop.store(true, Ordering::Release);
                return Err("直播清单获取失败，请检查网络后重试".to_owned());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        self.spawn_player(&Self::manifest_uri());
        *self.sink_index.lock().unwrap() = None;
        if !self.child_playing() {
            return Err("播放器启动失败，详见 /tmp/yihu-radio-player.log".to_owned());
        }
        *self.owner.lock().unwrap() = Some(owner.to_owned());
        Ok(())
    }

    fn child_playing(&self) -> bool {
        self.child.lock().unwrap().is_some()
    }

    fn manifest_uri() -> String {
        format!("file://{MIRROR_MANIFEST}")
    }

    fn spawn_player(&self, manifest_uri: &str) {
        let log = std::fs::File::create(PLAYER_LOG).ok();
        let mut cmd = Command::new("gst-play-1.0");
        cmd.args(gst_args(manifest_uri))
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        match log {
            Some(log) => cmd.stderr(Stdio::from(log)),
            None => cmd.stderr(Stdio::null()),
        };
        // 独立进程组：绝不并入任何插件会话组，收起杀灭/禁用插件都波及不到它；
        // 生命周期由宿主显式 stop / Drop / 属主死亡兜底管理。
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let child = cmd.spawn().ok();
        *self.child.lock().unwrap() = child;
    }

    /// 当前播放的属主会话 id（无播放 = None）。
    pub fn owner(&self) -> Option<String> {
        self.owner.lock().unwrap().clone()
    }

    /// false 表示播放进程已退出；原因见 `PLAYER_LOG`。
    pub fn is_playing(&self) -> bool {
        let mut slot = self.child.lock().unwrap();
        match slot.as_mut() {
            Some(child) => match child.try_wait() {
                Ok(Some(_)) | Err(_) => {
                    *slot = None;
                    false
                }
                Ok(None) => true,
            },
            None => false,
        }
    }

    /// 单独调节本播放流音量（0–100），不改系统与其他应用的音量。
    pub fn set_volume(&self, pct: u32) {
        let pid = self.child.lock().unwrap().as_ref().map(Child::id);
        let Some(pid) = pid else { return };
        let mut sink = self.sink_index.lock().unwrap();
        if sink.is_none() {
            let resolved = Command::new("pactl")
                .env("LC_ALL", "C")
                .arg("list")
                .args(["sink-inputs"])
                .output()
                .ok()
                .filter(|out| out.status.success())
                .map(|out| find_sink_input(&String::from_utf8_lossy(&out.stdout), pid))
                .unwrap_or(None);
            *sink = resolved;
            *self.pactl_ok.lock().unwrap() = resolved.is_some();
        }
        if let Some(idx) = *sink {
            let ok = Command::new("pactl")
                .env("LC_ALL", "C")
                .args(["set-sink-input-volume", &idx.to_string(), &format!("{pct}%")])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok {
                // 序号可能已失效（切台/声服务器重启），下次重新解析。
                *sink = None;
            }
        }
    }

    pub fn stop(&self) {
        if let Some(stop) = self.mirror_stop.lock().unwrap().take() {
            stop.store(true, Ordering::Release);
        }
        *self.owner.lock().unwrap() = None;
        *self.sink_index.lock().unwrap() = None;
        let child = self.child.lock().unwrap().take();
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for RadioPlayer {
    fn drop(&mut self) {
        // 守护进程退出即停播：不留孤儿音频
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_player_args() {
        assert_eq!(
            gst_args("file:///tmp/yihu-radio/live.m3u8"),
            vec![
                "--flags=audio".to_owned(),
                "file:///tmp/yihu-radio/live.m3u8".to_owned()
            ]
        );
    }

    #[test]
    fn fresh_player_reports_stopped_and_stop_is_safe() {
        let player = RadioPlayer::new();
        assert!(!player.is_playing());
        player.stop();
        assert!(!player.is_playing());
        assert!(player.owner().is_none());
    }

    #[test]
    fn absolutizes_protocol_relative_uris() {
        let manifest = "#EXTM3U\n#EXTINF:7.01,\n//ls-hw-ot.qtfm.cn/live/1133/64k/a.ts\n";
        let fixed = absolutize_manifest(manifest, "https://ls.qingting.fm/live/1133/64k.m3u8");
        assert!(fixed.contains("https://ls-hw-ot.qtfm.cn/live/1133/64k/a.ts"));
        assert!(fixed.starts_with("#EXTM3U"));
    }

    #[test]
    fn absolutizes_root_relative_and_keeps_absolute() {
        let manifest = "/live/1133/64k/a.ts\nhttps://cdn.example.com/b.ts\n#EXTM3U\n";
        let fixed = absolutize_manifest(manifest, "https://ls.qingting.fm/live/x.m3u8");
        assert!(fixed.contains("https://ls.qingting.fm/live/1133/64k/a.ts"));
        assert!(fixed.contains("https://cdn.example.com/b.ts"));
        assert!(fixed.contains("#EXTM3U"));
    }

    #[test]
    fn locates_sink_input_by_pid() {
        let out = "\
Sink Input #267
	Specification: ...
	application.name = \"other\"
	application.process.id = \"999\"

Sink Input #268
	Specification: ...
	application.process.id = \"598467\"
";
        assert_eq!(find_sink_input(out, 598467), Some(268));
        assert_eq!(find_sink_input(out, 1), None);
        assert_eq!(find_sink_input("", 598467), None);
    }
}
