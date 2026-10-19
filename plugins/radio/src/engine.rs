//! 播放引擎（播放随插件走）：纯 Rust 内嵌管线——HLS 清单轮询 → 分片下载
//! → MPEG-TS 解复用 → fdk-aac 解码（HE-AAC v1/v2）→ PulseAudio 推流。
//! 取代 gst-play-1.0 子进程（实测省 ~48MB PSS：GStreamer/ffmpeg 全家桶
//! 不再进内存）。环境变量 `YIHU_RADIO_ENGINE=gst` 回退旧引擎（[`legacy`]）。
//!
//! 与面板 player.rs 同源口径（M5 验证的三个关键点的去向）：
//! 1. 蜻蜓清单协议相对地址 → [`stream::join_url`] 内存内改写（gst 路线
//!    仍走 /tmp 镜像）。
//! 2. `--flags=audio` 纯音频收窄 → [`tsdemux`] 只解 PMT 声明的 AAC 音频
//!    PID（实测本台 TS 另带一路 H.264，正好忽略）。
//! 3. 音量「与应用音量生效」→ 仍 pactl 定位 sink-input 单独调节；pid
//!    换成本进程（pulse 流自带 application.process.id）。
//!
//! 生命周期（硬性规则 1/2 合规）：
//! - 网络与解码全在后台线程组，主线程只读写原子标志与 Mutex，绝不触碰
//!   线程内部；所有阻塞点有界：ureq 8s 超时、队列等待带 500ms 粒度的
//!   停止轮询、pulse 按帧背压（每帧 ≤46ms 巡检一次标志）；
//! - 每次播放一个 [`Session`]，线程只捕获本会话的 Arc——跨会话零共享
//!   可变状态，以「会话私有」落实 BUG-001 的 (id, gen) 代数隔离教训；
//! - stop() 不 join：置停标志即返回，线程有界自灭；pulse Sink 落 drop
//!   时 flush 服务端残余缓冲，停播立即静音。

use std::collections::VecDeque;
use std::io::Read as _;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::fdk;
use crate::legacy;
use crate::legacy::GstEngine;
use crate::pulse;
use crate::stream;
use crate::tsdemux::TsDemux;
use yihu_core::{perror, pinfo, pwarn};

/// 清单/分片请求超时。
const FETCH_TIMEOUT: Duration = Duration::from_secs(8);
/// 单请求体上限（分片 ~75KB、清单 <100KB，4MB 是防失控值）。
const MAX_BODY: u64 = 4 * 1024 * 1024;
/// 分片队列上限：~7s/片 → 最多积压 ~21s，背压天然限内存。
const QUEUE_CAP: usize = 3;
/// 开台到出声的等待上限（与旧引擎 5s 清单口径一致）。
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
/// 清单缺 TARGETDURATION 时的轮询兜底间隔（秒）。
const DEFAULT_TARGET: u64 = 4;
/// 连续失败多少轮判致命（约 3×分片时长 ≈ 20s+，容忍一般网络抖动）。
const MAX_CONSECUTIVE_FAILURES: u32 = 3;

/// 一次播放会话的全部共享状态；线程只持有本会话的 Arc。
struct Session {
    stop: AtomicBool,
    /// is_playing 真值：线程组任一致命退出即置 false。
    alive: AtomicBool,
    /// 解码出首帧并写入 pulse（play 的成功判据，比旧「进程活着」更实）。
    first_frame: AtomicBool,
    queue: Mutex<VecDeque<Vec<u8>>>,
    queue_cv: Condvar,
    /// 清单流结束（ENDLIST/致命），队列不会再有新分片。
    feed_done: AtomicBool,
}

impl Session {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            stop: AtomicBool::new(false),
            alive: AtomicBool::new(true),
            first_frame: AtomicBool::new(false),
            queue: Mutex::new(VecDeque::new()),
            queue_cv: Condvar::new(),
            feed_done: AtomicBool::new(false),
        })
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// 取下一分片；None = 停止或清单流已结束且取空。
    fn pop_segment(&self) -> Option<Vec<u8>> {
        let mut q = self.queue.lock().unwrap();
        loop {
            if let Some(seg) = q.pop_front() {
                return Some(seg);
            }
            if self.feed_done.load(Ordering::Acquire) {
                return None;
            }
            if self.stopped() {
                return None;
            }
            let (g, _) = self.queue_cv.wait_timeout(q, Duration::from_millis(500)).unwrap();
            q = g;
        }
    }

    /// 入队（满则等待背压）；false = 会话已停止。
    fn push_segment(&self, seg: Vec<u8>) -> bool {
        let mut q = self.queue.lock().unwrap();
        loop {
            if self.stopped() {
                return false;
            }
            if q.len() < QUEUE_CAP {
                q.push_back(seg);
                drop(q);
                self.queue_cv.notify_one();
                return true;
            }
            let (g, _) = self.queue_cv.wait_timeout(q, Duration::from_millis(500)).unwrap();
            q = g;
        }
    }
}

/// 播放引擎（Send + Sync：play 在后台线程调用，其余在主线程）。
pub struct Engine {
    legacy: GstEngine,
    session: Mutex<Option<Arc<Session>>>,
    sink_index: Mutex<Option<u32>>,
}

impl Engine {
    pub fn new() -> Self {
        Self {
            legacy: GstEngine::new(),
            session: Mutex::new(None),
            sink_index: Mutex::new(None),
        }
    }

    /// 播放前探测运行环境，缺失时返回可安装的包名提示。
    pub fn probe(&self) -> Result<(), String> {
        if legacy::enabled() {
            return self.legacy.probe();
        }
        fdk::Lib::acquire()?;
        if pulse::Lib::load().is_none() {
            let hint = yihu_core::distro::install_hint(&["libpulse0"]);
            return Err(format!("缺少播放组件：libpulse（{hint}）"));
        }
        Ok(())
    }

    /// 播放 `stream_url`：起清单/解码线程组，等首帧出声（上限 5s）才返回
    /// Ok。**阻塞可达 5s——只能在后台线程调用**（与旧引擎调用面一致）。
    pub fn play(&self, stream_url: &str) -> Result<(), String> {
        if legacy::enabled() {
            return self.legacy.play(stream_url);
        }
        self.stop();
        // 过渡期清理：升级前旧版本可能遗留 gst-play 孤儿（插件被 SIGKILL 时）。
        legacy::kill_orphan_players();

        let lib = fdk::Lib::acquire()?;
        let session = Session::new();
        *self.session.lock().unwrap() = Some(session.clone());

        let url = stream_url.to_owned();
        let s = session.clone();
        std::thread::Builder::new()
            .name("radio-fetch".into())
            .spawn(move || fetch_loop(&url, s))
            .map_err(|e| format!("无法启动清单线程：{e}"))?;
        let s = session.clone();
        std::thread::Builder::new()
            .name("radio-decode".into())
            .spawn(move || decode_loop(s, lib))
            .map_err(|e| format!("无法启动解码线程：{e}"))?;

        let deadline = Instant::now() + STARTUP_TIMEOUT;
        while Instant::now() < deadline {
            if session.first_frame.load(Ordering::Acquire) {
                // 新流新 sink-input，音量待 UI 恢复时重新解析
                *self.sink_index.lock().unwrap() = None;
                return Ok(());
            }
            if !session.alive.load(Ordering::Acquire) {
                self.stop();
                return Err("播放器启动失败，详见 ~/.local/share/yihu/logs/radio.log".to_owned());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        self.stop();
        Err("直播清单获取失败，请检查网络后重试".to_owned())
    }

    /// false 表示播放已中断/停止；原因见插件日志。
    pub fn is_playing(&self) -> bool {
        if legacy::enabled() {
            return self.legacy.is_playing();
        }
        match self.session.lock().unwrap().as_ref() {
            Some(s) => s.alive.load(Ordering::Acquire),
            None => false,
        }
    }

    /// 单独调节本播放流音量（0–100）。音频在本进程内，pid 即自身。
    pub fn set_volume(&self, pct: u32) {
        if legacy::enabled() {
            return self.legacy.set_volume(pct);
        }
        let pid = std::process::id();
        let mut sink = self.sink_index.lock().unwrap();
        if sink.is_none() {
            let resolved = Command::new("pactl")
                .env("LC_ALL", "C")
                .arg("list")
                .args(["sink-inputs"])
                .output()
                .ok()
                .filter(|out| out.status.success())
                .map(|out| legacy::find_sink_input(&String::from_utf8_lossy(&out.stdout), pid))
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

    /// 停播：置停标志 + 唤醒队列即返回（线程有界自灭；Sink drop 时 flush）。
    pub fn stop(&self) {
        if legacy::enabled() {
            return self.legacy.stop();
        }
        *self.sink_index.lock().unwrap() = None;
        if let Some(s) = self.session.lock().unwrap().take() {
            s.stop.store(true, Ordering::Release);
            s.queue_cv.notify_all();
        }
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 线程组致命退出：记日志、清 alive（UI 2s 巡检据此报「播放已中断」）。
fn fatal(s: &Session, why: &str) {
    perror!("电台播放中断：{why}");
    s.alive.store(false, Ordering::Release);
    s.feed_done.store(true, Ordering::Release);
    s.queue_cv.notify_all();
}

/// 可中断睡眠：200ms 粒度响应停止标志。
fn sleep_interruptible(s: &Session, d: Duration) {
    let deadline = Instant::now() + d;
    while Instant::now() < deadline {
        if s.stopped() {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// 阻塞式 GET 文本（清单用）。
fn fetch_text(uri: &str) -> Result<String, String> {
    let resp = ureq::get(uri)
        .timeout(FETCH_TIMEOUT)
        .call()
        .map_err(|e| format!("清单获取失败：{e}"))?;
    let mut text = String::new();
    resp.into_reader()
        .take(MAX_BODY)
        .read_to_string(&mut text)
        .map_err(|e| format!("清单读取失败：{e}"))?;
    Ok(text)
}

/// 阻塞式 GET 二进制（分片用）。
fn fetch_segment(uri: &str) -> Result<Vec<u8>, String> {
    let resp = ureq::get(uri)
        .timeout(FETCH_TIMEOUT)
        .call()
        .map_err(|e| format!("分片获取失败：{uri}：{e}"))?;
    let mut v = Vec::new();
    resp.into_reader()
        .take(MAX_BODY)
        .read_to_end(&mut v)
        .map_err(|e| format!("分片读取失败：{uri}：{e}"))?;
    Ok(v)
}

/// 清单线程：轮询直播清单、追新分片、下载入队（容量背压）。
fn fetch_loop(url: &str, s: Arc<Session>) {
    // 已入队分片的「绝对序号上界」（media_sequence + 已见分片数）。
    let mut emitted_total: Option<u64> = None;
    let mut failures: u32 = 0;
    let mut target = DEFAULT_TARGET;

    while !s.stopped() {
        match fetch_text(url) {
            Ok(text) => {
                failures = 0;
                let pl = stream::parse_playlist(&text, url);
                if pl.target_duration > 0 {
                    target = pl.target_duration;
                }
                // 直播追边：首发从倒数第 2 片起（留 1 片缓冲余量），
                // 之后按绝对序号增量取新片（服务器可能裁掉旧片，夹取合法区间）。
                let total = pl.media_sequence + pl.segments.len() as u64;
                let start_idx = match emitted_total {
                    None => (pl.segments.len() as u64).saturating_sub(2),
                    Some(prev) => prev.saturating_sub(pl.media_sequence).min(pl.segments.len() as u64),
                };
                let mut all_enqueued = true;
                for seg in &pl.segments[start_idx as usize..] {
                    if s.stopped() {
                        return;
                    }
                    match fetch_segment(seg) {
                        Ok(bytes) => {
                            if !s.push_segment(bytes) {
                                return;
                            }
                        }
                        Err(e) => {
                            pwarn!("{e}，等下轮清单重试");
                            all_enqueued = false;
                            break;
                        }
                    }
                }
                if all_enqueued {
                    emitted_total = Some(total);
                } else {
                    failures += 1;
                    if failures >= MAX_CONSECUTIVE_FAILURES {
                        fatal(&s, "连续拉流失败");
                        return;
                    }
                }
                if pl.end_list {
                    s.feed_done.store(true, Ordering::Release);
                    s.queue_cv.notify_all();
                    return;
                }
            }
            Err(e) => {
                pwarn!("{e}");
                failures += 1;
                if failures >= MAX_CONSECUTIVE_FAILURES {
                    fatal(&s, "连续拉流失败");
                    return;
                }
            }
        }
        sleep_interruptible(&s, Duration::from_secs((target / 2).clamp(2, 7)));
    }
}

/// 解码线程：分片出队 → TS 解复用 → fdk 解码 → pulse 推流。
fn decode_loop(s: Arc<Session>, lib: Arc<fdk::Lib>) {
    let Some(mut dec) = fdk::Decoder::new(lib.clone()) else {
        fatal(&s, "解码器初始化失败");
        return;
    };
    let Some(plib) = pulse::Lib::load() else {
        fatal(&s, "缺少 libpulse-simple");
        return;
    };
    let mut demux = TsDemux::new();
    let mut sink: Option<pulse::Sink> = None;
    let mut adts: Vec<u8> = Vec::with_capacity(128 * 1024);
    let mut pcm = vec![0i16; fdk::PCM_CAP];

    'session: while !s.stopped() {
        let Some(seg) = s.pop_segment() else { break };
        demux.push(&seg, &mut adts);

        loop {
            if s.stopped() {
                break 'session;
            }
            let mut view: &[u8] = &adts;
            let outcome = dec.decode(&mut view, &mut pcm);
            adts.drain(..adts.len() - view.len());
            match outcome {
                fdk::Decoded::NeedMore => break,
                fdk::Decoded::Frame => {
                    if dec.take_spec_changed() || sink.is_none() {
                        // 先 drop 旧流（flush 清残余）再按新规格连接
                        sink = None;
                        match pulse::Sink::connect(plib.clone(), dec.sample_rate, dec.channels) {
                            Some(sk) => sink = Some(sk),
                            None => {
                                fatal(&s, "无法连接音频输出（pulse）");
                                break 'session;
                            }
                        }
                    }
                    let n = dec.frame_size * dec.channels as usize;
                    if let Some(sk) = &sink {
                        if !sk.write(&pcm[..n.min(pcm.len())]) {
                            fatal(&s, "音频输出断开");
                            break 'session;
                        }
                    }
                    if !s.first_frame.swap(true, Ordering::AcqRel) {
                        pinfo!(
                            "电台出声：{}Hz {}声道 帧长{}（fdk-aac 内嵌引擎）",
                            dec.sample_rate,
                            dec.channels,
                            dec.frame_size
                        );
                    }
                }
                fdk::Decoded::Conceal => {} // 单帧损坏，跳过继续
                fdk::Decoded::Fatal(e) => {
                    fatal(&s, &format!("解码失败 0x{e:04X}"));
                    break 'session;
                }
            }
        }
    }

    // 正常清单完结（ENDLIST）也按中断上报，让 UI 状态一致。
    if !s.stopped() {
        s.alive.store(false, Ordering::Release);
    }
    drop(sink); // flush：停播立即静音
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实分片的全管线预检（默认忽略）：TS 解复用 → fdk-aac 解码。
    /// 跑法：抓一个直播分片存为 /tmp/probe-seg.ts（收听时从镜像清单取
    /// 首个分片 URL 下载），然后
    /// `cargo test -p radio-player -- --ignored --nocapture`。
    /// 断言的是 2026-10-19 实测的蜻蜓 64k 流形态（HE-AAC v1：LC 核心
    /// 22050Hz，SBR 输出 44100Hz 双声道）；若失败提示重核
    /// docs/电台引擎去GStreamer化.md 的编码结论。
    #[test]
    #[ignore = "需要本地真实分片 /tmp/probe-seg.ts"]
    fn 真实分片_demux_解码对拍() {
        let seg = std::fs::read("/tmp/probe-seg.ts").expect("先抓取分片到 /tmp/probe-seg.ts");

        let mut demux = TsDemux::new();
        let mut adts = Vec::new();
        demux.push(&seg, &mut adts);
        assert!(!adts.is_empty(), "TS 解复用无输出");
        println!("ADTS 载荷 {} 字节，glitches {}", adts.len(), demux.glitches);

        let lib = fdk::Lib::acquire().expect("libfdk-aac（含 SBR）未就绪");
        let mut dec = fdk::Decoder::new(lib).expect("解码器初始化失败");
        let mut pcm = vec![0i16; fdk::PCM_CAP];
        let mut frames = 0u32;
        let mut conceal = 0u32;
        let mut specs = std::collections::BTreeMap::new();
        loop {
            let mut view: &[u8] = &adts;
            let outcome = dec.decode(&mut view, &mut pcm);
            adts.drain(..adts.len() - view.len());
            match outcome {
                fdk::Decoded::Frame => {
                    frames += 1;
                    *specs
                        .entry((dec.sample_rate, dec.channels, dec.frame_size))
                        .or_insert(0u32) += 1;
                    if frames == 1 {
                        println!("首帧规格：{}Hz {}ch 帧长{}", dec.sample_rate, dec.channels, dec.frame_size);
                    }
                }
                fdk::Decoded::NeedMore => break,
                fdk::Decoded::Conceal => conceal += 1,
                fdk::Decoded::Fatal(e) => panic!("解码致命错误 0x{e:04X}"),
            }
        }
        println!("共解码 {frames} 帧，conceal {conceal}，规格分布 {specs:?}");
        assert!(frames > 140, "帧数异常：{frames}");
        assert_eq!(
            specs.len(), 1,
            "规格应全程一致（SBR 激活后 44100/2048）"
        );
        assert_eq!(dec.sample_rate, 44100, "SBR 输出采样率");
        assert_eq!(dec.frame_size, 2048, "HE-AAC 帧长");
        assert_eq!(dec.channels, 2);
    }
}
