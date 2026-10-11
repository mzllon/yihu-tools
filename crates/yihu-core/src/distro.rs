//! 发行版探测与包管理命令提示（跨发行版支持 Stage 1）。
//!
//! 依赖安装提示目前散在 radio（gst 组件）/qrtool（zbar-tools）/
//! mdworkbench（pandoc）等处，此前写死 `apt install`。本模块按
//! /etc/os-release 的 ID 探测发行版，给出对应包管理器命令；未识别
//! 的发行版回退为"请用你的包管理器安装 <包名>"。

use std::collections::HashMap;
use std::sync::OnceLock;

/// 发行版形态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PkgBackend {
    Apt,
    Dnf,
    Pacman,
    Zypper,
    Apk,
    Unknown,
}

impl PkgBackend {
    /// 安装命令前缀（不带包名）
    pub fn install_cmd(&self) -> Option<&'static str> {
        match self {
            PkgBackend::Apt => Some("sudo apt install"),
            PkgBackend::Dnf => Some("sudo dnf install"),
            PkgBackend::Pacman => Some("sudo pacman -S"),
            PkgBackend::Zypper => Some("sudo zypper install"),
            PkgBackend::Apk => Some("sudo apk add"),
            PkgBackend::Unknown => None,
        }
    }

    /// 面向用户的提示文案；未识别发行版给出通用文案
    pub fn install_hint(&self, packages: &[&str]) -> String {
        match self.install_cmd() {
            Some(cmd) => format!("{cmd} {}", packages.join(" ")),
            None => format!(
                "请用你的包管理器安装：{}",
                packages.join(" ")
            ),
        }
    }
}

/// /etc/os-release 的 ID 字段（小写；容器/测试可注入文本）
pub fn detect_from(os_release: &str) -> PkgBackend {
    // 键统一小写：os-release 规范用大写，但防御非规范文件（测试/衍生品）
    let mut map = HashMap::new();
    for line in os_release.lines() {
        if let Some((k, v)) = line.split_once('=') {
            map.insert(k.trim().to_lowercase(), v.trim().trim_matches('"').to_string());
        }
    }
    let id = map.get("id").map(|s| s.as_str()).unwrap_or("");
    let id_like = map.get("id_like").map(|s| s.as_str()).unwrap_or("");
    match id {
        "ubuntu" | "debian" | "linuxmint" | "pop" | "deepin" | "uos" => PkgBackend::Apt,
        "fedora" | "rhel" | "centos" | "rocky" | "alma" => PkgBackend::Dnf,
        "arch" | "manjaro" | "endeavouros" => PkgBackend::Pacman,
        "opensuse" | "opensuse-leap" | "suse" => PkgBackend::Zypper,
        "alpine" => PkgBackend::Apk,
        _ => {
            // ID 未识别时看 ID_LIKE（deepin 老版本 ID=Deepin 大写等场景）
            if id_like.contains("debian") {
                PkgBackend::Apt
            } else if id_like.contains("fedora") || id_like.contains("rhel") {
                PkgBackend::Dnf
            } else if id_like.contains("arch") {
                PkgBackend::Pacman
            } else if id_like.contains("suse") {
                PkgBackend::Zypper
            } else {
                PkgBackend::Unknown
            }
        }
    }
}

fn cached() -> &'static PkgBackend {
    static CACHE: OnceLock<PkgBackend> = OnceLock::new();
    CACHE.get_or_init(|| {
        match std::fs::read_to_string("/etc/os-release") {
            Ok(text) => detect_from(&text),
            Err(_) => PkgBackend::Unknown,
        }
    })
}

/// 生成"安装 X"的用户提示（按当前发行版）
pub fn install_hint(packages: &[&str]) -> String {
    cached().install_hint(packages)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_by_id_and_id_like() {
        let ubuntu = "ID=ubuntu\nID_LIKE=debian\n";
        assert_eq!(detect_from(ubuntu), PkgBackend::Apt);
        let fedora = "ID=fedora\n";
        assert_eq!(detect_from(fedora), PkgBackend::Dnf);
        let deepin = "ID=Deepin\nID_LIKE=\"Debian\"\n"; // 大写 + 带引号
        assert_eq!(detect_from(deepin.to_lowercase().as_str()), PkgBackend::Apt);
        let arch_like = "ID=foo\nID_LIKE=arch\n";
        assert_eq!(detect_from(arch_like), PkgBackend::Pacman);
        let unknown = "ID=mystery\n";
        assert_eq!(detect_from(unknown), PkgBackend::Unknown);
    }

    #[test]
    fn hints_per_backend() {
        let pkgs = ["zbar-tools", "qrencode"];
        assert_eq!(
            PkgBackend::Apt.install_hint(&pkgs),
            "sudo apt install zbar-tools qrencode"
        );
        assert_eq!(
            PkgBackend::Unknown.install_hint(&pkgs),
            "请用你的包管理器安装：zbar-tools qrencode"
        );
    }
}
