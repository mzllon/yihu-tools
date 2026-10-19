# 电台引擎去 GStreamer 化：纯 Rust 内嵌管线设计备忘

> 2026-10-19 实施并实机验收。动机：老机器内存紧张，gst-play-1.0 子进程
> 方案收听时两进程 RSS 合计 ~160MB（任务管理器口径，含共享库重复计数），
> 去重后 PSS 也有 ~79MB——大头全是 Ubuntu GStreamer/ffmpeg 全家桶
> （libavcodec/x265/flite TTS 语音库/libplacebo/librsvg，电台一个都用不上）。

## 结果

| 形态 | 旧引擎（gst-play 子进程） | 新引擎（内嵌） |
|---|---|---|
| 收听中（无窗）PSS | ~79 MB（radio 23 + gst-play 56） | **~17.4 MB（单进程）** |
| 换台 20 轮后 | — | 17.3 MB（mallopt 压 arena，零累积） |
| gst-play 进程 | 收听时存在 | 消失（回退开关除外） |

验收记录：20 轮 stop/play 全部 played ok；音量（pactl sink-input）
语义不变；`YIHU_RADIO_ENGINE=gst` 回退可播；开窗形态 23MB PSS（cairo
软渲染，不引 GL）。

## 流编码实测（决定性事实）

蜻蜓 64k HLS（`ls.qingting.fm/live/{id}/64k.m3u8`）：

- 分片：MPEG-TS，音频 PID stream_type 0x0F（AAC ADTS，MPEG-2 变体
  `ff f9` 同步），**另有一路 H.264 视频 PID（0x1B）**——旧 gst
  `--flags=audio` 收窄的原因；新 tsdemux 只取 PMT 声明的 AAC PID。
- 音频：**HE-AAC v1**——ADTS 头声明 profile=LC、核心采样率 22050Hz
  双声道（SBR 隐式信令，头里看不到 SBR），完整解码器输出 44100Hz、
  帧长 2048。判定方法：ADTS 头采样率索引=7（22.05k）+ gst 的
  avdec_aac 输出 44100（ffmpeg 施用了 SBR）。
- 实测分片形态：7s/片 ~75KB，帧长 283–589B（均值 379）。

## 架构

```
ui.rs（零改动）
  └ engine.rs  Engine：play/stop/is_playing/set_volume/probe 接口不变
      ├ stream.rs    HLS 清单解析 + join_url（内存内改写蜻蜓相对地址，
      │              /tmp 清单镜像文件机制废除）
      ├ tsdemux.rs   audio-only TS 解复用（PAT→PMT→PES 拼接→ADTS 流，
      │              丢包/脏字节容错），~350 行手写不引库
      ├ fdk.rs       libfdk-aac dlopen 封装（含 SBR 能力真帧探针）
      ├ pulse.rs     libpulse-simple dlopen 封装（推模式写 PCM）
      └ legacy.rs    旧 gst-play 引擎整体迁入，YIHU_RADIO_ENGINE=gst 回退
```

生命周期（硬性规则 1/2 合规）：播放=一个 Session，fetch/decode 两个
后台线程只捕获本会话 Arc；主线程只读写原子标志；所有阻塞点有界
（ureq 8s 超时、队列 500ms 粒度停止轮询、pulse 按帧背压）；stop()
不 join。分片队列上限 3（~21s 音频，背压限内存）。音量仍 pactl 定位
sink-input（pid 换成本进程，pulse 流自带 application.process.id）。
Cargo 零新增依赖（libc 原有）。

## 三个关键坑（均为 2026-10-19 实测第一手结论）

### 坑一：Ubuntu 的 libfdk-aac 是被剥掉 SBR 的 free 版

- 现象：fdk 解蜻蜓流「正常出声」但采样率 22050Hz（核心率），音质发闷，
  且无任何报错；ffmpeg（avdec_aac）同一段流输出 44100Hz。
- 根因：Ubuntu/Debian 源码包叫 **fdk-aac-free**，SBR/PS 解码器因专利
  授权被整体剥除（`nm -D` 查不到任何 sbrDecoder 符号）。fdk 静默丢弃
  SBR 载荷、只解 LC 核心。
- 对策：`scripts/build-fdk-aac.sh` 从 fdk-aac-sys crate 内嵌的上游完整
  源码（2.0.2，与系统 ABI 一致）编译 ~1MB 的 .so，装到插件
  `lib/` 目录；fdk.rs 加载优先级 = 自带完整版 → 系统库。系统库必须过
  **真帧探针**（内嵌 4 个真实 HE-AAC ADTS 帧，解出 44100Hz 才收），
  free 版被拒并给出构建指引。探针比查符号可靠：ABI 剥除与否、
  未来发行版怎么变，解码行为说了算。

### 坑二：fdk 内部 32KB 运输缓冲导致「输入耗尽即早停」

- 现象：57KB 的 ADTS 只解出 66 帧（应 151 帧），无任何错误码。
- 根因：`aacDecoder_Fill` 把外部字节拷进解码器内部运输缓冲（实测
  32KB），消费领先于 `aacDecoder_DecodeFrame`；喂入循环「外部输入吃
  完就停」时，内部还压着 ~85 帧没解。C 对照实验确认与喂法无关。
- 对策：decode() 空输入时**继续调用 DecodeFrame 消化内部缓冲**，
  直到它真正返回 NOT_ENOUGH_BITS（0x1002）才判定餍足。
- 附：TT_MP4_ADTS 枚举值是 **2 不是 0**（对拍 fdk-aac-sys 绑定发现
  记忆有误）；PES 载荷必须按 packet_length 截断，TS 填充字节（0xFF）
  会被 ADTS 解析误认成同步字。

### 坑三：GTK4 落到 X11 后端时，gtk_init 无条件预初始化 EGL

- 现象：无窗收听 PSS 44MB，映射着 NVIDIA eglcore/LLVM/gallium 全家
  （~20MB）；同机 yihu-panel 则完全没有。
- 根因（gdb 抓栈）：`gtk_init → gdk_x11_display_open →
  gdk_display_prepare_gl → eglInitialize`，GLVND 随之加载 vendor ICD
  全家。Wayland 后端的 display open 无此路径。Ubuntu 的 libgtk-4.so
  还直接链接 libgstplay/libgstgl/libgstreamer（media 后端），GL/EGL
  是它的 NEEDED——所以任何 GTK4 应用在 X11 后端都有此税。
  触发条件很阴：shell 环境残留 `GDK_BACKEND=x11` 即中招
  （GSK_RENDERER=cairo 拦不住 prepare_gl）。
- 对策：main.rs 里 `WAYLAND_DISPLAY` 存在即强制 `GDK_BACKEND=wayland`
  + `GSK_RENDERER=cairo`（电台 UI 静态，软渲染足够，开窗也不引 GL）。
  X11 会话机器（无 WAYLAND_DISPLAY）行为不变。
- 附带坑：glibc 默认每线程一个 malloc arena，换台起停线程组会让
  touched 页缓慢累积（20 轮 +11MB）——main 最前
  `mallopt(M_ARENA_MAX, 2)` 压掉，20 轮后 17.3MB 零累积。

## 回退与退役

- `YIHU_RADIO_ENGINE=gst` 整体走旧引擎（legacy.rs），过渡期保险，
  一个版本后删除。
- `kill_orphan_players`（清理被 SIGKILL 插件遗留的 gst-play 孤儿）
  保留一个版本，native 引擎 play 前也执行一次，之后随 legacy 删。
- gst 系统包暂不要求卸载：面板侧 `tools/yihu-panel/src/player.rs`
  仍是 gst 实现（面板搜索直播路径），radio 引擎实战稳定后下一轮
  抽共享 crate 复用换装。
- probe_missing 不再探测 gst 三件套（gst-tools/plugins-bad/libav）；
  pulse 与 libfdk-aac(SBR) 由运行时探针覆盖。

## 测试

- 单测 26 个：清单推进/URL 拼接、TS 解复用（合成 TS：PAT/PMT/PES
  分片/丢包断裂/脏字节重同步/视频 PID 忽略/填充截断）、引擎纯函数。
- 真实分片预检（`#[ignore]`，抓分片到 /tmp/probe-seg.ts 后
  `cargo test -p radio-player -- --ignored --nocapture`）：断言
  151 帧 / 44100Hz / 2048 帧长全程一致——流形态变化的第一报警器。

## 远期候选

- `fdk-aac-rs`（crates.io，fdk-aac 的纯 Rust 移植，2026-10 时 0.1.0）：
  成熟后可去掉唯一的 C 库依赖，达成字面意义的纯 Rust。
- symphonia 若日后支持 SBR（HE-AAC），LC 台可再省 fdk。
