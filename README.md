# 一呼 · Yihu

**一呼即出，百应随行** —— 一个性能优先的跨平台工具百宝箱。

`yihu-tools` 的长期形态：对标 uTools / ZTools 的呼出式工具箱——
全局快捷键一呼即出，搜索式命令面板直达能力，工具以插件形式逐步生长。
当前版本已是一个可日常使用的 Linux/GNOME 工具集（能力见下表），
呼出面板与插件系统在路线图中。

选型立场：**跨平台向低资源占用倾斜，软件性能永远优先**。
因此以 Rust + GTK4/libadwaita 原生实现、拒绝 WebView UI，
并坚持「统一管理中心 + 独立执行端 + 逐步生长的插件式能力集」的分层架构。

## 原则（按优先级）

1. **性能优先**：中心内存占用 50–100 MB 以内（RSS 口径）是硬约束，
   源自实测教训；后台功能零常驻或近零常驻（能用定时器就不驻留进程）；
2. **跨平台**：Linux/GNOME 先行，能力层与执行端保持平台无关，
   逐步覆盖 Windows / macOS；凡与低开销冲突的选型一律让位；
3. **可扩展**：多工具共享核心逻辑（`mt-core`），能力向统一插件描述
   （清单 + 入口 + 权限）演进，单语言 Rust；
4. **UI 美观**：libadwaita 原生观感，跟随系统深浅色，CSS 自定义样式。

> **为什么不是 Tauri**：项目最初采用 Rust + Tauri 2（系统 WebView 方案），
> 实测 Linux 上 WebKitGTK 的固定开销就达 ~158 MB（PSS），无论页面多简单
> 都省不下来。**WebView UI 与「百 MB 以内」的内存预算在 Linux 上不兼容，
> 勿再回头。** 切换只动 UI 层，共享库 `mt-core` 原样保留——分层设计的价值。

## 能力一览（统一入口：「一呼」中心）

| 中心页面 | 执行端 | 说明 |
|---|---|---|
| 主题切换 | `autodark-agent` + systemd 用户定时器 | 按自定义时间或日出日落自动切换 GNOME 深浅色（对标 Auto Dark Mode） |
| 右键菜单 | Nautilus 扩展（python3-nautilus） | 文件管理器右键顶级菜单：复制绝对路径（多选/当前目录） |
| 应用跟随 | 适配器（改写应用自身配置） | 让不跟随系统主题的应用自动切换：VS Code 系（CodeBuddy/ZCode）已支持 |
| 广播 | `gst-launch-1.0` 子进程 | 在线电台分类收听与收藏（蜻蜓FM 公开接口 64k 流）；播放时才有子进程，停止即退 |
| 设置 | 中心自身 | 开机启动开关；各组件（中心/agent/扩展）资源占用总览 |
| 关于 | — | 版本与架构说明 |

原则：**中心只负责配置与状态**，各功能的执行端（agent、扩展）保持独立，
中心关闭后一切照常工作。

## AutoDark 架构

```
┌─ 一呼中心 · 主题切换页 ────────┐      ┌─ systemd --user ─────────┐
│ 写 ~/.config/minitools/autodark.conf │ ──▶ │ yihu-autodark.timer      │
│ 启停定时器、显示状态/下次切换        │      │ 每分钟 → autodark-agent  │
└──────────────────────────────┘      └──────────────────────────┘
                                              │ 读配置 → 判定目标主题
                                              ▼
                                    gsettings set color-scheme/gtk-theme
```

- **切换判定**：自定义时间（支持跨零点）或日出至日落（NOAA 天文算法，
  `mt-core::sun`，纯计算无网络；坐标手动配置）；
- **常驻成本为零**：agent 无 GTK 依赖，按需启动、即退即走。

## 「复制绝对路径」

Nautilus 右键顶级菜单（python3-nautilus 扩展，复制动作在 Nautilus
进程内完成——Wayland 下无焦点进程无法写剪贴板，mutter 实测拒绝）。
中心「右键菜单」页可一键安装依赖（pkexec 授权）与启停。注意：复制内容
由 Nautilus 进程持有，Nautilus 完全退出后剪贴板自然清空。

## 「应用跟随」适配器

针对「有深浅主题但不跟随系统」的应用，改写其自身配置实现跟随。
三类模式按效果排序：**A 配置热改写**、**B 一次性修复**、**C 启动参数**。

- 已适配：VS Code 系（CodeBuddy/ZCode）——开启 `autoDetectColorScheme`
  + 浅/深主题偏好，settings.json 热重载即时生效；已有配置与自定义主题
  偏好完整保留；
- 判定不可适配：飞书（主题存于 Chromium LevelDB，运行时改写有损坏
  风险；请在飞书 设置→通用→外观 中找「跟随系统」）。

新增适配器：在 `tools/yihu/src/adapters.rs` 清单中加一行并扩展逻辑。

## 「广播」在线电台

搜索/分类收听网络电台（参考墨鱼FM 的实现思路与布局）：顶部搜索框、
分类瓷贴、带封面与收听数的频道列表、底部迷你播放条。分类与频道列表
来自蜻蜓FM 公开 Web 接口（`rapi.qtfm.cn`，搜索为 `search.qingting.fm`），
播放为 `ls.qingting.fm` 的 64k HLS 流，由 `gst-launch-1.0 playbin`
**子进程**完成——中心 RSS 不因播放增长，停止或退出一呼即结束子进程；
关窗驻留托盘时声音继续。收藏保存在 `~/.config/minitools/radio_favorites.json`。

- 接口无官方协议保障（第三方客户端通行用法），解析失败只影响在线列表，收藏仍可播；
- 播放依赖（gst-play、TS 解复用、AAC 解码、pulsesink）缺失时，广播页出现
  「播放依赖」卡片，**一键安装**（pkexec 系统授权，同「右键菜单」页惯例）；
  也可手动：`sudo apt install -y gstreamer1.0-tools gstreamer1.0-plugins-bad gstreamer1.0-libav gstreamer1.0-plugins-good`
- 仅限个人收听；高码率与搜索/排行榜暂未支持。

## 系统托盘

- 启动中心后显示一呼托盘图标，支持点击唤起及「打开一呼」「退出」菜单。
- 托盘菜单首行为「正在播放」合并行：圆形封面悬浮 ⏸/▶ 角标 + 电台与节目名，
  **点击整行即暂停/继续**；未播放时显示引导文案，点击直达广播页。
  悬停托盘图标亦可查看当前播放状态。
- 注册 MPRIS 媒体接口：顶栏通知中心（点击日期时间）出现媒体卡片，支持暂停与媒体键。
- 桌面支持托盘时，关闭窗口只隐藏窗口，中心继续驻留；再次启动一呼会恢复原窗口。
- 未检测到托盘宿主时，关闭窗口正常退出；托盘宿主消失或服务异常时，隐藏窗口会重新显示。
- Ubuntu/GNOME 需要启用 AppIndicator / Ubuntu AppIndicators 扩展。图标内嵌在程序中，无需先安装图标主题。
- 「退出」仅退出中心，不影响独立的主题定时器和 Nautilus 扩展。开机启动仍默认显示窗口。

## 实测数据（加入托盘前的 release 构建）

| 指标 | 一呼中心 | （对比）Tauri 版 |
|---|---|---|
| RSS | ~80 MB | ~215 MB |
| PSS | ~31 MB | ~158 MB |
| 二进制体积 | 0.55 MB（agent 0.43 MB） | 6.7 MB |
| 常驻后台 | 无（定时器按需拉起 agent） | 无 |

关键教训：① GL 渲染器会把 Mesa/NVIDIA 驱动栈映射进进程，
低频刷新工具在 `main` 开头设 `GSK_RENDERER=cairo`（183 → 85 MB）；
② gtk4-rs 的 API 有 `v4_x` feature 门控（已统一启用 `v4_12`）。

## 目录结构

```
yihu-tools/
├── Cargo.toml                  # workspace 根
├── crates/mt-core/             # 共享核心库
│   ├── src/lib.rs              #   /proc、statvfs 系统信息读取
│   ├── src/autodark.rs         #   主题配置/调度/gsettings 应用
│   └── src/sun.rs              #   日出日落天文算法（NOAA 简化）
├── tools/
│   ├── yihu/                   # 中心应用（统一管理入口）
│   │   └── src/
│   │       ├── main.rs         #   侧边栏 + 页面框架 + 关于页
│   │       ├── page_autodark.rs    # 主题切换页
│   │       ├── page_clipboard.rs   # 右键菜单页
│   │       ├── page_apps.rs        # 应用跟随页
│   │       ├── page_radio.rs       # 广播页（分类/收藏/播放）
│   │       ├── radio_api.rs        #   蜻蜓FM 公开接口与收藏持久化
│   │       ├── radio_player.rs     #   gst-launch 子进程播放管理
│   │       ├── page_settings.rs   # 设置页（开机启动/资源占用）
│   │       ├── adapters.rs     #   应用适配器清单与逻辑
│   │       └── nautilus.rs     #   扩展安装/状态（内容编译期内嵌）
│   ├── sysdash/                # 系统仪表盘（已卸载退役，源码保留，不在构建清单）
│   └── autodark-agent/         # 切换执行器（无 UI，systemd 调用）
├── packaging/                  # desktop 文件 + Nautilus 扩展源
└── scripts/                    # gen_icon.py / install.sh
```

## 环境要求

要求 GTK >= 4.12、libadwaita >= 1.4（推荐 Ubuntu 24.04+；Ubuntu 22.04 默认库版本不足）。源码构建还需 Rust/Cargo、C 编译器及以下开发库：

```bash
sudo apt install -y libgtk-4-dev libadwaita-1-dev libdbus-1-dev pkg-config
# 可选（复制绝对路径需要）：
sudo apt install -y python3-nautilus
# 可选（广播页运行需要，桌面版通常已自带）：
sudo apt install -y gstreamer1.0-tools gstreamer1.0-plugins-good gstreamer1.0-plugins-bad gstreamer1.0-libav
```

## 构建与运行

```bash
cargo build --release                # 全部工具
cargo run -p yihu                    # 中心（推荐从「活动」启动 一呼）
cargo test -p mt-core                # 共享库单元测试
./scripts/build-package.sh           # locked release 构建并生成 dist/*.tar.gz
python3 scripts/gen_icon.py yihu    # 重新生成图标（yihu/ping/sysdash/autodark）
```

## 安装 / 重装发布包

```bash
tar -xzf dist/yihu-0.1.0-linux-x86_64.tar.gz
cd yihu-0.1.0-linux-x86_64
./reinstall.sh --dry-run
./reinstall.sh
```

包内包含相邻的 `yihu` / `autodark-agent`、图标、可选 Nautilus 扩展与独立安装器；
目标机不需要源码或 Rust，但需 Bash、Python 3（运行中进程的安全退出要求 pidfd 支持）、
procps、ldd、systemd 用户登录会话及 GTK/libadwaita/DBus 运行库。
这是本机构建的动态链接 ELF，**不是通用静态包**：架构、glibc 和所有共享库须与构建机兼容；
仅向可信来源的包运行安装器（SHA256SUMS 是完整性检查，不是签名）。

默认安装到 `~/.local/lib/yihu`；自定义绝对 `XDG_DATA_HOME` 时安装到
`$XDG_DATA_HOME/yihu/lib`，图标/菜单也遵循该目录，systemd 遵循 `XDG_CONFIG_HOME`。
不使用 sudo，不自动安装系统依赖。`--dry-run` 完成载荷、依赖、现有文件、服务与进程检查而不修改状态。
重装仅对验证过路径和 UID 的进程发送 SIGTERM，等待 15 秒，拒绝强杀；先停 timer，
等待 oneshot service 自然结束，以避免 systemd stop 超时后的强杀。
保留服务启用状态并恢复原活动状态，重写现有 service/autostart 的旧 checkout 路径。
程序界面不会自动重启；恢复原活动的 AutoDark 会继续原来的主题切换行为。

配置、第三方应用设置、扩展开关不删除；扩展不存在时不会自动启用，也不会重启 Nautilus，
除非显式加 `--restart-nautilus`。旧 checkout 二进制和退役工具文件保持原样。
未知文件、定制服务/drop-in、修改过的图标/扩展、符号链接或不安全权限会中止，需先人工检查；
不做通配符卸载。支持含空格路径，保守拒绝特殊转义字符路径。
失败会尽力恢复被替换的文件及服务活动状态；不能恢复已退出的 GUI 进程，
断电/SIGKILL 不保证自动回滚。回滚自身失败时输出保留的临时备份目录。
旧 `scripts/install.sh` 现在仅显示发布包安装步骤，不再修改桌面或退出 Nautilus。

## 新增一个能力的步骤

1. 执行端视形态新增（中心页面 / 无 UI agent / Nautilus 扩展）；
2. 复用 `mt-core`；新系统数据逻辑优先沉淀进 `mt-core`；
3. 后台型功能优先「中心配置 + systemd 用户定时器 + 无 UI agent」架构；
4. 低频刷新 GUI 一律 `GSK_RENDERER=cairo`；
5. 图标样式在 `scripts/gen_icon.py` 中扩展一个绘制函数。

## 路线图（呼出式工具箱方向）

- **呼出面板**：全局快捷键唤起搜索式命令面板（对标 ZTools 的核心交互），
  各能力注册为面板命令；
- **插件生态**：把适配器与工具抽象为统一插件描述（清单 + 入口 + 权限）；
- **跨平台**：中心与执行端保持平台抽象，逐步支持 Windows / macOS
  （选型仍以低开销为先，必要时为平台更换执行端而非引入重运行时）；
- AutoDark：推迟控制、位置服务自动定位（配置结构已预留）；
- 更多应用适配器：微信开发者工具等（配置键需逐个现场调查）；
- sysdash 收编为中心的仪表盘页。
