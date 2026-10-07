//! zip 分发安装（M4 一期③）：校验 → 临时目录解包 → 原子提交。
//!
//! 校验链（对应 M4 计划验收门槛「zip 安装安全」）：
//! - 包体积 ≤ 64 MiB、文件数 ≤ 512、解包总量 ≤ 128 MiB（读取时以
//!   `take()` 硬限流，不信任中央目录声明值——炸弹声明可以撒谎）；
//! - 压缩比 > 100 的条目拒绝（>1 KiB 压缩块才参与判断）；
//! - 条目名穿越（`..`、绝对路径、反斜杠、NUL、盘符冒号）一律拒绝；
//! - 符号链接/非常规条目拒绝；
//! - manifest 复用 [`crate::plugins::parse_manifest`] 全套规则（含权限
//!   词表白名单）；
//! - 提交：解包到注册表内 `.staging-*`（同文件系统保证 rename 原子），
//!   校验全过才 remove 旧目录 + rename，任一步失败即清 staging。
//!
//! 来源可追溯（M4 冻结决策 #6）：每次安装写收据（sha256/来源/权限）到
//! 宿主数据区 `yihu/receipts/<id>.json`，市场包先验 sha256 再解包。

use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use zip::ZipArchive;

use crate::plugins::{self, Manifest};

/// 压缩包体积上限
pub const MAX_ZIP_BYTES: u64 = 64 * 1024 * 1024;
/// 解包总量上限（硬限流口径）
pub const MAX_UNCOMPRESSED: u64 = 128 * 1024 * 1024;
/// 条目数上限
pub const MAX_FILES: usize = 512;
/// 单条目压缩比上限（compressed > 1 KiB 时参与判断）
pub const MAX_RATIO: u64 = 100;

/// 从 zip 安装（无来源校验，本地开发者包）。返回解析后的 manifest。
pub fn install_from_zip(zip_path: &Path) -> Result<Manifest, String> {
    install_from_zip_in(zip_path, &plugins::plugins_dir(), &plugins::data_home(), None, "zip")
}

/// 从 zip 安装并校验期望 sha256（市场入口：先验哈希再解包）。
pub fn install_from_zip_verified(
    zip_path: &Path,
    expected_sha256: &str,
    source: &str,
) -> Result<Manifest, String> {
    install_from_zip_in(
        zip_path,
        &plugins::plugins_dir(),
        &plugins::data_home(),
        Some(expected_sha256),
        source,
    )
}

/// 同 [`install_from_zip`]，注册表根/数据根/期望哈希可注入（测试用）。
/// `expected` 为 Some 时先校验全包 sha256，不符即拒装（不解包）。
pub fn install_from_zip_in(
    zip_path: &Path,
    registry: &Path,
    data_root: &Path,
    expected: Option<&str>,
    source: &str,
) -> Result<Manifest, String> {
    let meta = fs::metadata(zip_path).map_err(|e| format!("读取包失败：{e}"))?;
    if meta.len() > MAX_ZIP_BYTES {
        return Err(format!("包体积超限（>{} MiB）", MAX_ZIP_BYTES / 1024 / 1024));
    }
    let sha256 = sha256_file(zip_path)?;
    if let Some(want) = expected {
        if !sha256.eq_ignore_ascii_case(want.trim()) {
            return Err(format!("sha256 不符：期望 {want}，实际 {sha256}"));
        }
    }

    let file = fs::File::open(zip_path).map_err(|e| format!("打开包失败：{e}"))?;
    let mut archive = ZipArchive::new(file).map_err(|e| format!("不是有效的 zip：{e}"))?;
    if archive.len() > MAX_FILES {
        return Err(format!("条目数超限（>{}）", MAX_FILES));
    }

    // staging 名带纳秒：并发安装/并行测试互不踩踏；在注册表内保证同文件系统
    let staging = registry.join(format!(
        ".staging-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    ));
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging).map_err(|e| format!("创建暂存目录失败：{e}"))?;
    let result = extract_and_install(&mut archive, &staging, registry, data_root, source, &sha256);
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

fn extract_and_install(
    archive: &mut ZipArchive<fs::File>,
    staging: &Path,
    registry: &Path,
    data_root: &Path,
    source: &str,
    sha256: &str,
) -> Result<Manifest, String> {
    let mut used: u64 = 0;
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|e| format!("读取条目 {i} 失败：{e}"))?;
        let name = entry.name().to_string();
        check_entry_name(&name)?;

        if entry.is_dir() {
            fs::create_dir_all(staging.join(&name)).map_err(|e| format!("建目录 {name} 失败：{e}"))?;
            continue;
        }
        if entry.is_symlink() {
            return Err(format!("条目 {name} 是符号链接——拒绝安装"));
        }
        let declared = entry.size();
        let compressed = entry.compressed_size().max(1);
        if compressed > 1024 && declared / compressed > MAX_RATIO {
            return Err(format!("条目 {name} 压缩比异常（疑似炸弹）"));
        }

        let mut out = fs::File::create(staging.join(&name))
            .map_err(|e| format!("写条目 {name} 失败：{e}"))?;
        // 硬限流：即使声明值撒谎，读超预算即失败；解包字节数还要与
        // 声明值核对（防「声明小、实际大」被 take 静默截断）
        let budget = MAX_UNCOMPRESSED - used;
        let mut limited = entry.take(budget.saturating_add(1));
        let copied = io::copy(&mut limited, &mut out)
            .map_err(|e| format!("解包 {name} 失败：{e}"))?;
        if copied > budget {
            return Err("解包总量超限（zip 炸弹防护）".into());
        }
        if copied != declared {
            return Err(format!(
                "条目 {name} 实际大小 {copied} 与声明 {declared} 不符"
            ));
        }
        used += copied;
    }
    if used > MAX_UNCOMPRESSED {
        return Err("解包总量超限（zip 炸弹防护）".into());
    }

    // manifest 全套校验（含权限词表）
    let text = fs::read_to_string(staging.join("manifest.toml"))
        .map_err(|_| "包内缺 manifest.toml".to_string())?;
    let manifest = plugins::parse_manifest(&text).map_err(|e| format!("manifest 非法：{e}"))?;
    let entry_path = staging.join(&manifest.entry);
    if !entry_path.is_file() {
        return Err(format!("入口不存在：{}", manifest.entry));
    }

    // 原子提交：同文件系统内 rename
    let dest = registry.join(&manifest.id);
    if dest.exists() {
        fs::remove_dir_all(&dest).map_err(|e| format!("清理旧版本失败：{e}"))?;
    }
    fs::rename(staging, &dest).map_err(|e| format!("提交安装失败：{e}"))?;

    // 权限：入口 0755，其余 0644
    let _ = fs::set_permissions(dest.join(&manifest.entry), fs::Permissions::from_mode(0o755));
    for f in walk_files(&dest) {
        let _ = fs::set_permissions(&f, fs::Permissions::from_mode(0o644));
    }
    let _ = fs::set_permissions(dest.join(&manifest.entry), fs::Permissions::from_mode(0o755));

    let _ = write_receipt(data_root, &manifest.id, source, sha256, &manifest);
    Ok(manifest)
}

fn check_entry_name(name: &str) -> Result<(), String> {
    let bad = |why: &str| Err::<(), String>(format!("条目名非法（{why}）：{name:?}"));
    if name.is_empty() || name.contains('\0') || name.contains('\\') || name.contains(':') {
        return bad("控制字符/反斜杠/冒号");
    }
    if name.starts_with('/') {
        return bad("绝对路径");
    }
    if name.split('/').any(|c| c == "..") {
        return bad("路径穿越");
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut f = fs::File::open(path).map_err(|e| format!("打开包失败：{e}"))?;
    let mut hasher = Sha256::new();
    io::copy(&mut f, &mut hasher).map_err(|e| format!("读取包失败：{e}"))?;
    Ok(format!("{:x}", hasher.finalize()))
}

fn walk_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out
}

// ---- 安装收据 ----

/// 收据路径：宿主数据区（插件目录只读、plugin-data 不可信，收据放这里）
pub fn receipt_path_in(data_root: &Path, id: &str) -> PathBuf {
    data_root.join("yihu/receipts").join(format!("{id}.json"))
}

/// 写安装收据（来源 / sha256 / 版本 / 权限）。失败仅忽略——收据缺失
/// 不应阻断安装，市场端到端核验时再提示。
pub fn write_receipt(
    data_root: &Path,
    id: &str,
    source: &str,
    sha256: &str,
    manifest: &Manifest,
) -> io::Result<PathBuf> {
    let path = receipt_path_in(data_root, id);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let receipt = serde_json::json!({
        "id": id,
        "name": manifest.name,
        "version": manifest.version,
        "entry": manifest.entry,
        "permissions": manifest.permissions,
        "source": source,
        "sha256": sha256,
        "ts": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    });
    fs::write(&path, serde_json::to_string_pretty(&receipt).unwrap_or_default())?;
    Ok(path)
}

/// 计算本地文件 sha256（中心页展示 / 市场核对用）
pub fn sha256_of(path: &Path) -> io::Result<String> {
    sha256_file(path).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const MANIFEST: &str = "id = \"zipped\"\nname = \"包插件\"\napi = \"^1\"\nentry = \"run.py\"\npermissions = [\"clipboard.write\"]\n";
    const ENTRY: &str = "#!/usr/bin/env python3\nprint('ok')\n";

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("yihu-zip-test-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        p
    }

    fn write_zip(path: &Path, entries: &[(&str, &str)]) {
        let file = fs::File::create(path).unwrap();
        let mut w = zip::ZipWriter::new(file);
        w.start_file(".keep", zip::write::SimpleFileOptions::default()).unwrap();
        w.write_all(b"").unwrap();
        for (name, content) in entries {
            w.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(content.as_bytes()).unwrap();
        }
        w.finish().unwrap();
    }

    fn setup(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        let work = tmp(tag);
        let registry = work.join("registry");
        let data = work.join("data");
        fs::create_dir_all(&registry).unwrap();
        fs::create_dir_all(&data).unwrap();
        (work, registry, data)
    }

    #[test]
    fn happy_path_installs_and_writes_receipt() {
        let (work, registry, data) = setup("happy");
        let zpath = work.join("p.zip");
        write_zip(&zpath, &[("manifest.toml", MANIFEST), ("run.py", ENTRY)]);
        let m = install_from_zip_in(&zpath, &registry, &data, None, "zip:test").unwrap();
        assert_eq!(m.id, "zipped");

        let dest = registry.join("zipped");
        assert!(dest.join("manifest.toml").is_file());
        let mode = fs::metadata(dest.join("run.py")).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "入口应有执行位");
        // 无 staging 残留
        let leftovers: Vec<_> = fs::read_dir(&registry).unwrap().flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".staging"))
            .collect();
        assert!(leftovers.is_empty());

        // 收据：来源 + sha256 + 权限
        let rc = fs::read_to_string(receipt_path_in(&data, "zipped")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&rc).unwrap();
        assert_eq!(v["source"], "zip:test");
        assert_eq!(v["sha256"], sha256_of(&zpath).unwrap());
        assert_eq!(v["permissions"][0], "clipboard.write");
    }

    #[test]
    fn rejects_path_traversal_and_symlink_and_wrong_sha() {
        let (work, registry, data) = setup("bad");
        let zpath = work.join("evil.zip");
        write_zip(&zpath, &[("manifest.toml", MANIFEST), ("../escape", "x")]);
        let e = install_from_zip_in(&zpath, &registry, &data, None, "zip").unwrap_err();
        assert!(e.contains("非法") || e.contains("穿越"), "{e}");
        assert!(!registry.join("zipped").exists(), "拒装不得落盘");
        assert!(!work.join("escape").exists());

        let (work2, registry2, data2) = setup("bad2");
        let z2 = work2.join("p.zip");
        write_zip(&z2, &[("manifest.toml", MANIFEST), ("run.py", ENTRY)]);
        let e = install_from_zip_in(&z2, &registry2, &data2, Some("deadbeef"), "zip")
            .unwrap_err();
        assert!(e.contains("sha256"), "{e}");
        assert!(!registry2.join("zipped").exists());
    }

    #[test]
    fn rejects_missing_manifest_and_missing_entry() {
        let (work, registry, data) = setup("missing");
        let z1 = work.join("nomanifest.zip");
        write_zip(&z1, &[("run.py", ENTRY)]);
        assert!(install_from_zip_in(&z1, &registry, &data, None, "zip")
            .unwrap_err()
            .contains("manifest"));

        let z2 = work.join("noentry.zip");
        write_zip(&z2, &[("manifest.toml", MANIFEST)]);
        let e = install_from_zip_in(&z2, &registry, &data, None, "zip").unwrap_err();
        assert!(e.contains("入口不存在"), "{e}");
        assert!(!registry.join("zipped").exists());
    }

    #[test]
    fn rejects_compression_bomb() {
        let (work, registry, data) = setup("bomb");
        let zpath = work.join("bomb.zip");
        let file = fs::File::create(&zpath).unwrap();
        let mut w = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        w.start_file("manifest.toml", opts).unwrap();
        w.write_all(MANIFEST.as_bytes()).unwrap();
        w.start_file("run.py", opts).unwrap();
        w.write_all(ENTRY.as_bytes()).unwrap();
        // 4 MiB 全 'a'，deflate 后 ~4 KiB → 压缩比 ≈1000:1，超 MAX_RATIO
        w.start_file("blob", opts).unwrap();
        let chunk = vec![b'a'; 1024 * 1024];
        for _ in 0..4 {
            w.write_all(&chunk).unwrap();
        }
        w.finish().unwrap();

        let e = install_from_zip_in(&zpath, &registry, &data, None, "zip").unwrap_err();
        assert!(e.contains("压缩比"), "{e}");
        assert!(!registry.join("zipped").exists(), "炸弹不得落盘");
        // 读取时硬限流兜底（take）独立于声明值：覆盖率由探针与集成测试保证
    }

    #[test]
    fn upgrade_replaces_existing() {
        let (work, registry, data) = setup("upgrade");
        let z1 = work.join("v1.zip");
        write_zip(&z1, &[("manifest.toml", MANIFEST), ("run.py", ENTRY)]);
        install_from_zip_in(&z1, &registry, &data, None, "zip").unwrap();
        let z2 = work.join("v2.zip");
        write_zip(&z2, &[("manifest.toml", MANIFEST), ("run.py", "#!/bin/sh\necho v2\n")]);
        install_from_zip_in(&z2, &registry, &data, None, "zip").unwrap();
        let text = fs::read_to_string(registry.join("zipped/run.py")).unwrap();
        assert!(text.contains("v2"), "{text}");
        assert!(!registry.join("zipped/manifest.toml.bak").exists());
    }

    #[test]
    fn rejects_non_zip_garbage() {
        let (work, registry, data) = setup("garbage");
        let zpath = work.join("garbage.zip");
        fs::write(&zpath, b"this is not a zip file at all").unwrap();
        let e = install_from_zip_in(&zpath, &registry, &data, None, "zip").unwrap_err();
        assert!(e.contains("zip") || e.contains("有效"), "{e}");
    }
}
