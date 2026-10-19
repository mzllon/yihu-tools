//! 旧播放引擎（gst-play-1.0 子进程 + 直播清单本地镜像 + pactl 单流音量）。
//!
//! 已被纯 Rust 内嵌引擎（engine.rs）取代，按环境变量
//! `YIHU_RADIO_ENGINE=gst` 回退启用——过渡期保险：新引擎实战一个版本、
//! 面板侧 player.rs 换装后再整体移除本模块。M5 验证的三个关键点（清单
//! 镜像、--flags=audio、pactl 应用音量）在本模块原样保留。
//!
//! 生命周期差异：引擎在本插件进程内，窗体隐藏不退出、收听不断；真正的
//! 退出路径（--quit / SIGTERM / 窗内退出）统一走 [`GstEngine::stop`]。

use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// 回退开关：`YIHU_RADIO_ENGINE=gst` 时 engine.rs 全部走本模块。
pub(crate) fn enabled() -> bool {
    std::env::var_os("YIHU_RADIO_ENGINE").is_some_and(|v| v == "gst")
}


const PLAYER_LOG: &str = "/tmp/yihu-radio-player.log";
const MIRROR_DIR: &str = "/tmp/yihu-radio-player";
/// 本地镜像清单路径；播放器消费它，镜像线程持续刷新。
const MIRROR_MANIFEST: &str = "/tmp/yihu-radio-player/live.m3u8";
/// 清单刷新间隔；蜻蜓直播清单目标分片时长约 7s。
const MIRROR_INTERVAL: Duration = Duration::from_secs(4);

/// 播放参数；单独抽出便于测试。
fn gst_args(manifest_uri: &str) -> Vec<String> {
    vec!["--flags=audio".to_owned(), manifest_uri.to_owned()]
}

/// 把清单中的相对地址改写为绝对地址。
/// `//host/path` → 补 scheme；`/path` → 补 scheme+host；其余原样保留。
fn absolutize_manifest(manifest: &str, base_uri: &str) -> String {
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

/// 阻塞式 GET 文本（清单镜像线程用）。
fn fetch_text(uri: &str) -> Result<String, String> {
    let resp = ureq::get(uri)
        .timeout(Duration::from_secs(8))
        .call()
        .map_err(|e| format!("清单获取失败：{e}"))?;
    let mut text = String::new();
    resp.into_reader()
        .take(4 * 1024 * 1024)
        .read_to_string(&mut text)
        .map_err(|e| format!("清单读取失败：{e}"))?;
    Ok(text)
}

use std::io::Read as _;

/// 从 `pactl list sink-inputs` 输出中定位属于 pid 的 sink-input 序号。
pub(crate) fn find_sink_input(pactl_output: &str, pid: u32) -> Option<u32> {
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

/// 缺失的系统组件（apt 包名）。
fn probe_missing() -> Vec<&'static str> {
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

/// 清理本用户遗留的孤儿播放进程（插件进程被 SIGKILL 时 gst-play 会存活）。
pub(crate) fn kill_orphan_players() {
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

/// 旧引擎（Send + Sync：play 在后台线程调用，其余在主线程）。
pub(crate) struct GstEngine {
    child: Mutex<Option<Child>>,
    sink_index: Mutex<Option<u32>>,
    mirror_stop: Mutex<Option<std::sync::Arc<AtomicBool>>>,
}

impl GstEngine {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// 播放前探测运行环境，缺失时返回可安装的包名列表。
    pub(crate) fn probe(&self) -> Result<(), String> {
        let missing = probe_missing();
        if missing.is_empty() {
            return Ok(());
        }
        let hint = yihu_core::distro::install_hint(&missing);
        Err(format!("缺少播放组件：{}（{}）", missing.join("、"), hint))
    }

    /// 播放 `stream_url`：清孤儿与陈旧清单，启动镜像线程，首份新清单落地
    /// 后拉起 gst-play。**阻塞可达 5s——只能在后台线程调用**。
    pub(crate) fn play(&self, stream_url: &str) -> Result<(), String> {
        self.probe()?;
        self.stop();
        kill_orphan_players();
        // 陈旧清单会让播放器拉起后拉到 404 旧分片（表现为无声），必须删除。
        let _ = std::fs::remove_file(MIRROR_MANIFEST);

        let stop = std::sync::Arc::new(AtomicBool::new(false));
        *self.mirror_stop.lock().unwrap() = Some(stop.clone());
        let stream_url = stream_url.to_owned();
        let stop_for_thread = stop.clone();
        std::thread::Builder::new()
            .name("radio-mirror".into())
            .spawn(move || loop {
                match fetch_text(&stream_url) {
                    Ok(text) => {
                        let fixed = absolutize_manifest(&text, &stream_url);
                        let _ = write_manifest_atomic(MIRROR_MANIFEST, &fixed);
                    }
                    Err(e) => {
                        yihu_core::pwarn!("清单镜像获取失败：{e}");
                    }
                }
                if stop_for_thread.load(Ordering::Acquire) {
                    break;
                }
                std::thread::sleep(MIRROR_INTERVAL);
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
        self.spawn_player();
        if !self.is_playing() {
            return Err("播放器启动失败，详见 /tmp/yihu-radio-player.log".to_owned());
        }
        Ok(())
    }

    fn spawn_player(&self) {
        let manifest_uri = format!("file://{MIRROR_MANIFEST}");
        let log = std::fs::create_dir_all(MIRROR_DIR).ok().and_then(|_| {
            std::fs::File::create(PLAYER_LOG).ok()
        });
        let mut cmd = Command::new("gst-play-1.0");
        cmd.args(gst_args(&manifest_uri))
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        match log {
            // 同进程组：本插件退出路径能连 gst-play 一起收掉
            Some(log) => cmd.stderr(Stdio::from(log)),
            None => cmd.stderr(Stdio::null()),
        };
        *self.child.lock().unwrap() = cmd.spawn().ok();
    }

    /// false 表示播放进程已退出；原因见 `PLAYER_LOG`。
    pub(crate) fn is_playing(&self) -> bool {
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

    /// 单独调节本播放流音量（0–100）。
    pub(crate) fn set_volume(&self, pct: u32) {
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

    /// 停播：镜像线程退出 + 子进程收割。
    pub(crate) fn stop(&self) {
        if let Some(stop) = self.mirror_stop.lock().unwrap().take() {
            stop.store(true, Ordering::Release);
        }
        *self.sink_index.lock().unwrap() = None;
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Default for GstEngine {
    fn default() -> Self {
        Self {
            child: Mutex::new(None),
            sink_index: Mutex::new(None),
            mirror_stop: Mutex::new(None),
        }
    }
}

impl Drop for GstEngine {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_player_args() {
        assert_eq!(
            gst_args("file:///tmp/yihu-radio-player/live.m3u8"),
            vec![
                "--flags=audio".to_owned(),
                "file:///tmp/yihu-radio-player/live.m3u8".to_owned()
            ]
        );
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
    }

    #[test]
    fn fresh_engine_reports_stopped_and_stop_is_safe() {
        let engine = GstEngine::new();
        assert!(!engine.is_playing());
        engine.stop();
        assert!(!engine.is_playing());
    }

    #[test]
    fn locates_sink_input_by_pid() {
        let out = "\
Sink Input #267
	Specification: ...
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
