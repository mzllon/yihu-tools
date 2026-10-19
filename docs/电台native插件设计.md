# 电台 native 官方插件设计（方向 C · 播放器优先）

> 2026-10-19 立项。目标：把电台从 Python stdio 插件升级为 **native 官方插件**
> ——M7 W8「native 形态」的开山验收件。UI 从声明式表单升级为**播放器优先**
> 的自有 GTK 窗口（用户草图方向 C，HTML 草图已确认）。
>
> 上游决策记录：API 范围 7 端点（除「批量正在播」外全用）；UI 草图 C 档；
> 落地形态 native（用户拍板 2026-10-19）。

## 1. 意图与动机

- 电台是能力最全的插件（网络/音频/常驻/收藏），当前 Python 表单形态与
  「封面卡片、播放器」的产品预期差距最大（用户反感表单式，对标 uTools/ZTools）。
- M7 全量插件化需要 **native 形态**落地样例：rich UI + 独立进程 + 插件中心
  纳管。radio 是最合适的开山件（播放引擎天然要独立于面板生命周期）。

## 2. 目标 / 非目标 / 反目标

**目标**
1. native 插件形态首发：manifest 新 kind、插件中心卡片纳管（启停/状态/数据管理）、
   独立 GTK 窗口、面板入口行。
2. 播放器优先 UI（方向 C）：大封面 + 正在播 + 大控制键；列表（收藏/分类/地区/
   搜索）收进抽屉。
3. API 7 端点全接入：分类、分类频道（分页）、地区（all=true）、
   **billboards/{分类}/{地区}/channels 组合榜单**、频道详情（v4）、搜索、播放流。
4. 收藏沿用现有 favorites.json（数据目录不变，老用户无感迁移）。
5. 播放引擎随插件走：面板退出/重启不影响收听。

**非目标**：点播/播客内容播放（搜索结果只取直播台）；账号/登录；定时关闭
（列入后续）；Windows 适配；夜灯/睡眠等周边功能。

**反目标**：回退到表单式 UI；播放依赖面板进程存活。

## 3. 约束

- GTK4 链接冲突：面板 gtk4 0.9，WebKitGTK 需 0.10+ → native 插件独立进程
  （mdworkbench 先例）。封面图用 libsoup3 异步 GET → GdkPixbuf，http:// 与
  https:// 都要支持（pic.qtfm.cn 是 http）。
- 蜻蜓 API 无鉴权无 SLA，且有过域名变更史（rapi/rapl 事件）：全部请求带
  超时 + 失败降级到收藏本地可用；端点常量集中一处便于热修。
- 呼出面板零 IO 红线不涉及（radio 不再走面板查询管道）。
- 音量语义保留「与应用音量生效」（pactl 单流）。

## 4. 已拍板的部分

| 决策 | 内容 |
|---|---|
| UI 方向 | C 播放器优先（HTML 草图已确认）：大封面 + 正在播节目（名称/时段）+ 大播放键 + 收藏 + 更多；列表收抽屉 |
| 抽屉四入口 | 收藏 / 分类浏览 / 地区浏览 / 搜索 |
| API 范围 | 7 端点（除批量正在播）；单频道正在播用列表自带 nowplaying 字段与 v4 详情 |
| 形态 | native 官方插件（开山件） |
| 主题 | 跟随系统深浅色 + 面板同款强调色（theme.rs 口径） |
| 收藏 | 数据目录 favorites.json 沿用，格式不变 |
| 草图附件 | [docs/电台native插件-草图.html](电台native插件-草图.html)（三方向可切换，C 为选定方向） |

## 4.5 API 契约（2026-10-19 实测钉死；端点常量集中 radio-api.rs 一处）

> 完整调研存档（响应样例、失败路径、地区过滤 8 种尝试全记录、防御注意）：
> **[docs/蜻蜓FM-API调研.md](蜻蜓FM-API调研.md)**；
> 其他音频源（radio-browser / 云听 / 央广直连流 / M3U 导入）：
> **[docs/电台音频源调研.md](电台音频源调研.md)**。下表为速查摘要。

| # | 端点 | 响应（实测结构） |
|---|---|---|
| 1 | `GET rapi.qtfm.cn/categories?type=channel` | `{Success, Data:[{id,title}]}` 13 个内容分类 |
| 2 | `GET rapi.qtfm.cn/categories/{cat}/channels?page&pagesize` | `{Success, Data:[{content_id,title,cover,description,nowplaying{id,name,start_time,end_time,broadcasters},audience_count,region{id,title,pid},city{...},clout}]}` 分页实测可用 |
| 3 | `GET rapi.qtfm.cn/regions?all=true` | `{Success, Data:[{id,title}]}` 32 条（31 省 + 407 网络台） |
| 4 | `GET rapi.qtfm.cn/billboards/{分类}/{地区}/channels` | `{Success, Data:[...]}` **分类×地区组合榜单**；分类 0=全部；实测 0/99→20 条、429/99→13 条 |
| 5 | `GET rapi.qingting.fm/v4/channels/{id}` | 频道详情（墨鱼 FM 在用；字段结构计划内排一次实测校验，含正在播/码率信息确认） |
| 6 | `GET search.qingting.fm/v3/search?k={kw}` | `{errcode, data.data.docs:[{id,title,type,cover,...}]}` 只取 `type=="channel_live"` |
| 7 | `ls.qingting.fm/live/{cid}/64k.m3u8` | HLS 播放流（实测 200）；播放引擎入参 |

注意：封面图为 `http://pic.qtfm.cn/...`（非 https），HTTP 客户端两 scheme 都要支持。
「批量正在播」（rapi.qingting.fm/v2/livechannelplaying）按用户决策不用；
单频道正在播用 #2/#5 自带字段。

## 4.6 分发与兼容

- **不走 reinstall.sh**（该通道只管宿主三件套，见其白名单校验）：radio-player
  二进制随插件目录分发（构建产物 cp 进 plugins/radio/；市场包=zip 含二进制，
  走既有 install_from_dir 安全链）。官方市场分发的签名/通道随 W5。
- **旧宿主兼容**：旧 parse_manifest 忽略未知字段 kind，且会把 entry=radio-player
  （ELF）当 stdio 插件 spawn → 协议握手失败 → 失败滑窗运行期禁用该插件
  （fail-safe，仅日志噪音，不影响其他插件）。本件要求插件与宿主同周期升级；
  新 scanner 对 kind=native 正式纳管。

## 4.7 mini 壳选型（builder 评审定案）

- panel.wasm = **面板现有 wasm 运行时**（wasm32 构建链，与 theme-switch 同款，
  W4 能力桥已具备）：query 提供「控制台入口行 / 正在播状态行（回车停止）/
  搜台快捷播放行」。
- status.json 读取：声明 `fs.read@<插件数据目录>/status.json` 走宿主代读能力管线
  （现成机制，不开新协议）。
- **反向命令通道**：GApplication command-line 转发（mdworkbench 惯例）——
  `radio-player --open`（开窗置前）/ `--play <id> [title]` / `--stop`；面板行激活
  时宿主 spawn entry 带参，运行中实例经单实例转发接收。

## 5. 留给实现方的部分（builder 自行决定）

- Rust 侧 HTTP 客户端选型（libsoup3 / reqwest::blocking+async 桥）、JSON 解析
- 封面缓存策略（插件数据目录 cover-cache/，LRU 上限）
- gst-play 子进程管理细节（复用/抽取 player.rs 逻辑的方式）
- GTK 组件布局细节（按 HTML 草图比例）

## 6. 架构定义（native 形态规范，随本件立规矩）

### 6.1 manifest 扩展

```toml
id = "radio"
kind = "native"            # 新字段：native | wasm | stdio（缺省 stdio，向后兼容）
entry = "radio-player"     # native 可执行（插件目录内）
permissions = [ ... ]      # 保留：中心页展示 + 审计对照；native 官方可信直连，
                           # 不经能力代理强制（沙箱不存在），但 deny 仍记录审计
```

- 扫描器：kind=native 时 entry 必须存在且可执行；不参与面板查询管道。
- **面板入口行由 mini 壳提供**：同插件目录内 `panel.wasm`（可选字段
  `panel_entry`），负责面板查询行——「📻 电台控制台」入口行、正在播状态行
  （回车停止）、快捷播放行（搜台名回车即播）。mini 壳经标准 stdio/WASM 协议
  运行，播放状态从插件数据目录 `status.json` 读（native 端写）。

### 6.2 进程与生命周期

- 窗体进程 GApplication 单实例（mdworkbench 惯例）：面板行/中心卡片/命令行
  重复拉起都转发到已有实例并置前。
- **关窗 = 隐藏（用户拍板 2026-10-19）**：点 ✕ 仅 withdraw 窗口，收听不中断；
  首次关闭提示一次「将在后台继续播放，可从面板行或插件中心找回」（提示状态
  记数据目录，只提示一次）。真正退出：窗内「更多 → 退出」/ 插件中心「停止」/
  `radio-player --quit`。实现上 close-request 回调拦截 + g_application_hold。
- 进程独立于面板：面板退出收听不中断；插件中心「停止」= 优雅退出窗体并停播。
- 运行状态上报：中心 PluginStatus 扩展 native 条目（进程实扫沿用 status.rs）。

### 6.3 跨组件状态契约（native ⇄ panel.wasm）

`<插件数据目录>/status.json`：`{ playing: bool, channel: {id,title,cover},
program: {name,start,end}, updated_at }`。native 端每次播放状态变化原子写；
panel.wasm 只读。窗口打开动作：面板行激活 → 宿主 spawn entry（单实例转发）。

## 7. UI 规格（方向 C）

- 窗口 ~420×600（可调），深浅色跟随系统，强调色跟随面板。
- 主视图：大封面（168px 圆角，无图时频道名首字渐变兜底）→ 🔴正在播节目
  （名称+时段）→ 频道名 → 元信息（分类·城市·听众）→ 控制排（收藏 / 大播放
  键 / 更多）。
- 抽屉（左侧滑出或下拉，实现方定）：收藏 / 分类浏览 / 地区浏览 / 搜索 四入口。
  - 分类浏览：分类列表 → 频道列表（分页滚动加载）
  - 地区浏览：地区列表（含网络台）→ 频道列表；「分类 × 地区」交叉 =
    billboards/{分类}/{地区}/channels 榜单
  - 搜索：搜索框 → 结果列表（只取 channel_live）
- 列表行：封面小图 + 频道名 + 🔴正在播 + 听众数 + 播放/收藏。
- 播放中：底部迷你条常显当前台（窗内切台即点即播）。

## 8. 退役与迁移

- radio.py、旧 ui.show 表单路径退役；面板 audio.play 宿主代执行路径中 radio
  部分随播放引擎入插件后清理（player.rs 保留通用播放能力给未来插件）。
- favorites.json 无需迁移（路径不变）；status.json 新增。
- 插件中心卡片随 native 形态自动纳管（kind=native 扫描支持落地时）。

## 9. 排期建议

- W6：本件实现（含 kind=native 扫描/纳管/mini 壳协议的最小落地）；
  W8 的 schedule/native 规范化随本件经验回写。
- **状态（2026-10-19）：已落地。** kind=native/panel_entry 解析与纳管、
  radio-player（方向 C 窗体 + 清单镜像播放引擎 + 收藏沿用 + status.json 契约）、
  panel.wasm mini 壳（入口行/状态行/面板直搜即播）全部就位；宿主 67 +
  radio-player 15 + core 56 测试全过。真机验收项：搜「电台」出行→回车开窗、
  分类/地区/搜索/收藏、关窗后台续播提示、插件中心卡片与「停止」。
