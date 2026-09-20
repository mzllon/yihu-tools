# AGENTS.md — 一呼（yihu-tools）开发约定

面向在本仓库工作的编码代理与维护者。改代码前先读「硬性规则」；修完 bug 必须回填 `docs/bugs.md`。

## 项目概览

「一呼」是一组 Linux/GNOME 桌面工具，Rust workspace：

| 路径 | 说明 |
|---|---|
| `tools/yihu/` | 中心应用（GTK4/libadwaita 设置中心，托盘） |
| `tools/yihu-panel/` | 呼出面板：单二进制双角色（无参=常驻守护进程，`toggle/show/hide/quit/status`=薄 CLI） |
| `tools/autodark-agent/` | AutoDark 定时主题切换（systemd user timer） |
| `crates/yihu-core/` | 共享库：配置、历史、插件注册表、主题切换 |
| `plugins/` | 外部插件（协议 v0：stdio 行式 JSON，呼出拉起/收起杀灭） |
| `packaging/` | desktop 文件、GNOME Shell 定位扩展 `yihu-panel-placer@tools.yihu`、Nautilus 扩展 |
| `scripts/reinstall.sh` | 官方安装/升级通道（校验严格，勿绕过其假设） |
| `docs/` | 里程碑计划与调研；**`docs/bugs.md` = bug 根因记录（修 bug 必读必写）** |

历史沿革：曾名 `ubuntu-mini-tools`/`minitools`/`mt-core`，M3 起统一为 yihu 命名空间；用户数据迁移脚本 `scripts/migrate-yihu-data.sh`。

## 常用命令

```bash
cargo build --release            # 全量构建
cargo test -p yihu-panel -p yihu-core
# 手工验证呼出面板（面板装在 ~/.local/lib/yihu/）：
~/.local/lib/yihu/yihu-panel toggle
YIHU_PANEL_DEBUG=1 yihu-panel    # 调试日志（呼出尺寸/淡入时机/插件会话）
busctl --user call tools.yihu.Panel /tools/yihu/Panel tools.yihu.Panel Toggle
busctl --user call tools.yihu.ShellPlacer /tools/yihu/ShellPlacer tools.yihu.ShellPlacer Where  # 窗口实际位置
```

## 硬性规则（违反即事故，根因见 `docs/bugs.md`）

1. **GTK 主循环回调里禁止无界阻塞**——`wait()`、`join()`、读管道、同步 IO 一律不许（BUG-001：插件退出事件误配导致 `wait()` 卡死主循环，热键全失效且无 panic 日志）。
2. **跨生命周期重建的资源必须带代数标识**——会话、窗口、定时器回调匹配用 `(id, gen)`，不能只用业务 id（BUG-001 根因、BUG-002 的 FadeIn 防误伤同此）。
3. **呼出面板的定位不许用固定延时猜时序**——必须轮询扩展 `Where` 闭环：尺寸变化→重摆→稳定→才淡入；mutter 50 会重置映射前/尺寸变化前的摆放，且行为非确定（BUG-002）。
4. **首帧透明度用 0.01，不用 0.0**——opacity=0 的 GTK4 窗口不提交缓冲、永远无法映射（BUG-002）。
5. **GNOME Shell 扩展禁用 `global.log`/`log()`**，用 `console.*`（GNOME 50 已移除，BUG-002 导火索）；改扩展 JS 后 Wayland 下需注销重登才加载，验证时警惕旧模块仍在运行。
6. **呼出路径零 IO**——应用枚举/历史/配置只在启动或后台线程做，呼出（toggle→显示）路径保持零 IO。
7. **安装目录与打包通道**——用户机器上的二进制在 `~/.local/lib/yihu/`，正式更新走 `scripts/reinstall.sh` 的打包校验；开发迭代可直接替换该目录二进制后重启守护进程验证。
8. **重命名/迁移数据路径**必须提供迁移脚本并保留回滚（见 `scripts/migrate-yihu-data.sh` 先例：默认 dry-run、sha256 校验、时间戳备份）。

## 提交与文档约定

- 修复 bug 后：在 `docs/bugs.md` 追加条目（现象/根因/修复/教训），若与既有教训相关在代码注释里引用条目号。
- 面板/扩展联调类改动，提交信息写明验证方式（如「实测 Where 时间线：117ms 映射、166ms 落位、~300ms 淡入」）。
- 中文注释与文档为主，保持既有风格。
