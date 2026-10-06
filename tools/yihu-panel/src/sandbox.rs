//! 插件沙箱启动器：宿主托管的外部插件统一经 bwrap 启动（M4 安全收口）。
//!
//! 原则（见 docs/插件基座安全模型与发布策略.md §3）：
//! - **fail-closed**：bwrap 不可用时拒绝拉起插件，绝不静默降级为直接执行；
//! - **默认拒绝**：`--unshare-all`（含网络/PID/IPC），FS 白名单 =
//!   插件目录只读 + 独立 data_dir 唯一可写 + 运行必需系统路径只读；
//! - **环境收敛**：`--clearenv` 后仅注入 PATH/HOME/LANG，不泄露宿主路径；
//!   插件在沙箱内看到的 data_dir 固定为 `/data`（见 [`SANDBOX_DATA_DIR`]）。
//!
//! 不用 `--new-session`：它与宿主的 `process_group(0)` 冲突——bwrap 已是
//! 进程组组长时 `setsid()` 返回 EPERM，bwrap 会报错退出。组级杀灭由
//! `process_group(0)` 承担（sessions.rs），宿主异常退出的孤儿兜底由
//! `--die-with-parent`（PR_SET_PDEATHSIG）承担。

use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

/// 插件目录在沙箱内的只读挂载点
pub const SANDBOX_PLUGIN_DIR: &str = "/plugin";
/// 数据目录在沙箱内的唯一可写挂载点（init 消息里的 data_dir 即此值）
pub const SANDBOX_DATA_DIR: &str = "/data";

/// bwrap 是否可用（首次调用探测 `bwrap --version`，结果进程内缓存）。
/// 呼出时 ensure_sessions 调用一次；探测是毫秒级 exec，与注册表扫描
/// 同属会话建立路径，不在按键零 IO 热路径上。
pub fn available() -> bool {
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        Command::new("bwrap")
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

/// 构造沙箱启动命令：返回的 Command 尚未 spawn，由调用方接 stdin/stdout
/// 管道并 `process_group(0)`。entry 必须是插件目录下的单段文件名
/// （与 manifest 校验一致），挂载为 `<SANDBOX_PLUGIN_DIR>/<entry>` 执行。
pub fn command(entry: &str, plugin_dir: &Path, data_dir: &Path) -> io::Result<Command> {
    if entry.is_empty()
        || entry.contains('/')
        || entry.contains('\\')
        || entry.starts_with('.')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("entry 非法（须为插件目录下的单段文件名）：{entry:?}"),
        ));
    }
    if !available() {
        return Err(io::Error::other(
            "bwrap 不可用：拒绝在沙箱外直接执行插件（fail-closed），请安装 bubblewrap",
        ));
    }

    let mut c = Command::new("bwrap");
    let mut args: Vec<String> = Vec::new();
    // —— 生命周期：宿主退出则内核杀灭 bwrap（插件进程随之失父链终结）——
    args.push("--die-with-parent".into());
    // —— 全量隔离：新 mount/PID/IPC/network/UTS/cgroup 命名空间 ——
    args.push("--unshare-all".into());
    // —— 基础伪文件系统：独立 /proc（不泄露宿主进程表）、最小 /dev、私有 /tmp ——
    args.extend(["--proc".into(), "/proc".into()]);
    args.extend(["--dev".into(), "/dev".into()]);
    args.extend(["--tmpfs".into(), "/tmp".into()]);
    // —— FS 白名单：插件目录只读，data_dir 唯一可写 ——
    args.extend(["--ro-bind".into(), pl(plugin_dir), SANDBOX_PLUGIN_DIR.into()]);
    args.extend(["--bind".into(), pl(data_dir), SANDBOX_DATA_DIR.into()]);
    // —— 运行必需系统路径（只读）：动态库与解释器运行时 ——
    args.extend(["--ro-bind".into(), "/usr".into(), "/usr".into()]);
    push_system_ro_binds(&mut args);
    // —— 环境收敛：仅注入白名单变量；管道 fd 经继承传递，不受 clearenv 影响 ——
    args.push("--clearenv".into());
    for (k, v) in [
        ("PATH", "/usr/bin:/bin"),
        ("HOME", SANDBOX_DATA_DIR),
        ("LANG", "C.UTF-8"),
    ] {
        args.extend(["--setenv".into(), k.into(), v.into()]);
    }
    // —— 入口：沙箱内绝对路径，绕过 PATH 查找 ——
    args.push(format!("{SANDBOX_PLUGIN_DIR}/{entry}"));
    c.args(&args);
    Ok(c)
}

fn pl(p: &Path) -> String {
    p.display().to_string()
}

/// 重建根级动态库/壳入口：usrmerge 布局（Ubuntu 22.04+，/bin /lib /lib64
/// 是指向 usr/* 的符号链接）下，只 bind /usr 时沙箱内没有这些入口链接，
/// `/lib64/ld-linux` 不可达、动态链接程序无法执行；旧式真实目录布局则
/// 退回只读绑定。
fn push_system_ro_binds(args: &mut Vec<String>) {
    for name in ["/bin", "/lib", "/lib64"] {
        match std::fs::symlink_metadata(name) {
            Ok(m) if m.file_type().is_symlink() => {
                // 符号链接以相对目标重建（如 "usr/bin"），bind 目标 /usr 已就位
                if let Ok(target) = std::fs::read_link(name) {
                    args.extend([
                        "--symlink".into(),
                        target.display().to_string(),
                        name.to_string(),
                    ]);
                }
            }
            Ok(m) if m.is_dir() => {
                args.extend(["--ro-bind-try".into(), name.to_string(), name.to_string()]);
            }
            _ => {}
        }
    }
    // /etc 不整体暴露（含宿主敏感配置），只挑运行时必需项
    for name in ["/etc/ld.so.cache", "/etc/localtime", "/etc/alternatives"] {
        args.extend(["--ro-bind-try".into(), name.to_string(), name.to_string()]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(cmd: &Command) -> Vec<String> {
        std::iter::once(cmd.get_program().to_string_lossy().into_owned())
            .chain(cmd.get_args().map(|a| a.to_string_lossy().into_owned()))
            .collect()
    }

    #[test]
    fn rejects_unsafe_entry() {
        let r = command("../evil", Path::new("/tmp/p"), Path::new("/tmp/d"));
        assert!(r.is_err());
        let r = command("sub/dir", Path::new("/tmp/p"), Path::new("/tmp/d"));
        assert!(r.is_err());
        let r = command(".hidden", Path::new("/tmp/p"), Path::new("/tmp/d"));
        assert!(r.is_err());
    }

    #[test]
    fn builds_sandbox_argv() {
        // fail-closed：无 bwrap 的环境下 command() 返回 Err（而非直接执行）
        if !available() {
            let err = command("x", Path::new("/tmp/p"), Path::new("/tmp/d")).unwrap_err();
            assert!(err.to_string().contains("bwrap"));
            return;
        }
        let cmd = command("probe.py", Path::new("/host/plugin"), Path::new("/host/data"))
            .expect("bwrap 存在时必须能构造命令");
        let a = argv(&cmd);

        assert_eq!(a[0], "bwrap");
        assert!(a.contains(&"--die-with-parent".to_string()));
        assert!(a.contains(&"--unshare-all".to_string()));
        // 不用 --new-session（与 process_group(0) 的 setsid 冲突）
        assert!(!a.contains(&"--new-session".to_string()));

        // FS 白名单：插件只读、data 可写
        let ro = |s: &str| a.iter().position(|x| x == s).unwrap();
        assert_eq!(a[ro("--ro-bind") + 1], "/host/plugin");
        assert_eq!(a[ro("--ro-bind") + 2], SANDBOX_PLUGIN_DIR);
        assert_eq!(a[ro("--bind") + 1], "/host/data");
        assert_eq!(a[ro("--bind") + 2], SANDBOX_DATA_DIR);

        // 入口是沙箱内绝对路径，且是最后一个参数
        assert_eq!(a.last().unwrap(), "/plugin/probe.py");

        // 环境收敛：clearenv 后只有 PATH/HOME/LANG
        assert!(a.contains(&"--clearenv".to_string()));
        let setenvs: Vec<&[String]> = a
            .windows(3)
            .filter(|w| w[0] == "--setenv")
            .map(|w| &w[1..])
            .collect();
        assert_eq!(setenvs.len(), 3);
        let has = |k: &str, v: &str| setenvs.iter().any(|s| s[0] == k && s[1] == v);
        assert!(has("PATH", "/usr/bin:/bin"));
        assert!(has("HOME", SANDBOX_DATA_DIR));
        assert!(has("LANG", "C.UTF-8"));

        // 基础伪文件系统
        for flag in ["--proc", "--dev", "--tmpfs"] {
            assert!(a.contains(&flag.to_string()), "缺 {flag}");
        }
    }
}
