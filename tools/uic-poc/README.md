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

## 已测基线（release，Ubuntu 26.04 / Wayland / GNOME 50，2026-10-07）

| 模式 | map_ms | 主进程 PSS | 辅助进程 PSS |
|---|---|---|---|
| native（原生模板） | 92–101 | ~33 MB | 0 |

web 腿数字留待 `--features web` 构建后回填。**决策红线**（M4 计划）：
空载 PSS 与呼出延迟不得明显劣化面板整体指标（待命 PSS ~20 MB、
暖 toggle < 100 ms）；web 腿超线直接取原生模板，不折衷。

补充观察项（数字之外，桌面手测）：
- webkit 进程收起杀灭后的残留（应归零）；
- webkit 首帧白屏感 vs 原生行首帧即绘；
- 长列表滚动流畅度与输入焦点行为。
