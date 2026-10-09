# uic-poc — UI 插件形态 PoC 实测脚手架（M4 二期③）

> 目的：用实测数据敲定 UI 插件进程形态（受控 WebView vs 原生模板），
> 杜绝拍脑袋。对应 docs/插件基座M4计划.md 二期 6。

## 口径

- `map_ms`：进程启动 → 窗口首帧映射的墙钟（冷启动 + 首帧近似）；
- `main_pss_kb`：主进程 PSS（/proc/self/smaps_rollup）；
- `webkit_children_pss_kb`：WebKitWebProcess/NetworkProcess 子进程 PSS
  之和（webkit 多进程架构的真实占用 = 主进程 + 辅助进程）；
- 本脚手架窗口处于「展示态」，与面板待命（窗口销毁，~20 MB）口径不同，
  只用于 native/web 两腿横向对比。

## 实测步骤

```bash
cargo build --release -p uic-poc
./target/release/uic-poc bench          # 两腿对比表
```

web 腿需要 GTK4 版 WebKitGTK 开发包（本机 2026-10-07 尚未安装）：

```bash
sudo apt install libwebkitgtk-6.0-dev
cargo build --release -p uic-poc --features web
./target/release/uic-poc bench
```

## 决策记录（2026-10-09，数字齐备，规则预冻结→拍板）

**UI 插件形态 = 原生模板**（宿主 GTK 渲染声明式 UI）。WebView 路线关闭。

实测（release，Ubuntu 26.04 / Wayland / GNOME 50，libwebkitgtk 2.52）：

| 模式 | map_ms（冷启动+首帧） | 主进程 PSS | WebKit 辅助 PSS | 总 PSS |
|---|---|---|---|---|
| native | 92–115 | 33–44 MB | 0 | **38–44 MB** |
| web | 160–224 | 71–72 MB | ~15 MB | **87 MB** |

对照 M4 预冻结规则「web 超线直接取原生模板，不再折衷」：web 腿首帧
慢 ~1.7 倍、空载 PSS 为 native 的 ~2 倍、达面板待命红线（~20 MB）的
4 倍以上，双指标超线 → **native**。两次运行数字稳定，无偶然性。

后续含义：
- M5 UI 插件实现走原生模板（插件声明式描述 UI，宿主渲染）；
- WebView 不做公开市场默认运行时；个别重型 UI 插件将来若确需 web，
  作为显式声明 + 审核例外的形态再议（能力代理与沙箱要求不变）。

补充观察项（数字之外，桌面手测）：
- webkit 进程收起杀灭后的残留（应归零）；
- webkit 首帧白屏感 vs 原生行首帧即绘；
- 长列表滚动流畅度与输入焦点行为。
