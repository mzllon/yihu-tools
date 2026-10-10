//! 插件数据目录的统计 / 导出 / 导入 / 清空（M5，对标 uTools/Rubick 的
//! 「按插件管理数据」形态：数据是用户可见、可带走的文件，不是黑盒）。
//!
//! 数据目录（`yihu/plugin-data/<id>`，0700）是插件沙箱内唯一可写的
//! `/data`；本模块是宿主侧的管理入口，供中心「插件」页调用：
//! - [`data_stats`]：文件数与字节数（后台线程调用，UI 不阻塞）；
//! - [`export_data_zip`]：打 zip（含来源元数据条目），供跨机迁移/备份；
//! - [`import_data_zip`]：解包替换。安全校验与插件包安装同强度
//!   （穿越/符号链接/压缩比炸弹/条目数/解包总量，见 [`crate::zipfile`]），
//!   另校验包内元数据的插件 id 与目标一致——数据包装错插件直接拒绝；
//! - 提交统一「staging + rename 原子替换」，失败不留半套数据。

use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::zipfile::{check_entry_name, MAX_FILES, MAX_RATIO, MAX_UNCOMPRESSED, MAX_ZIP_BYTES};

/// 导出包内元数据条目名（记录插件 id 与导出时间；导入时校验 id 匹配）
const META_ENTRY: &str = "yihu-data.json";

/// 数据目录统计：文件数与总字节数。目录不存在返回 None（插件尚未写过数据）。
pub fn data_stats(dir: &Path) -> Option<(u64, u64)> {
    if !dir.is_dir() {
        return None;
    }
    let mut files = 0u64;
    let mut bytes = 0u64;
    for p in walk_files(dir) {
        if let Ok(m) = fs::metadata(&p) {
            files += 1;
            bytes += m.len();
        }
    }
    Some((files, bytes))
}

/// 导出数据目录为 zip。返回打包的文件数；dest 已存在则覆盖。
pub fn export_data_zip(dir: &Path, plugin_id: &str, dest: &Path) -> Result<u64, String> {
    if !dir.is_dir() {
        return Err("插件还没有数据可导出".into());
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建目标目录失败：{e}"))?;
    }
    let file = fs::File::create(dest).map_err(|e| format!("创建包失败：{e}"))?;
    let mut w = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let mut count = 0u64;
    let result = (|| -> Result<(), String> {
        w.start_file(META_ENTRY, opts)
            .map_err(|e| format!("写元数据失败：{e}"))?;
        let meta = serde_json::json!({
            "id": plugin_id,
            "exported_at": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        });
        use std::io::Write;
        w.write_all(serde_json::to_string_pretty(&meta).unwrap_or_default().as_bytes())
            .map_err(|e| format!("写元数据失败：{e}"))?;
        let root = dir.canonicalize().map_err(|e| format!("解析数据目录失败：{e}"))?;
        for p in walk_files(&root) {
            let rel = p
                .strip_prefix(&root)
                .map_err(|_| "相对路径解析失败".to_string())?
                .to_string_lossy()
                .replace('\\', "/");
            if rel.is_empty() {
                continue;
            }
            check_entry_name(&rel)?;
            w.start_file(rel, opts).map_err(|e| format!("写条目失败：{e}"))?;
            let mut f = fs::File::open(&p).map_err(|e| format!("读取 {} 失败：{e}", p.display()))?;
            io::copy(&mut f, &mut w).map_err(|e| format!("写条目失败：{e}"))?;
            count += 1;
        }
        Ok(())
    })();
    if let Err(e) = result {
        drop(w);
        let _ = fs::remove_file(dest);
        return Err(e);
    }
    w.finish().map_err(|e| format!("收尾包失败：{e}"))?;
    Ok(count)
}

/// 从 zip 导入数据（整目录替换）。校验链：包体积/条目数/条目名/压缩比/
/// 解包总量与插件包安装同强度；包内元数据 id 必须与目标插件一致。
/// 返回导入的数据文件数（不含元数据条目）。
pub fn import_data_zip(zip_path: &Path, plugin_id: &str, data_dir: &Path) -> Result<u64, String> {
    let meta = fs::metadata(zip_path).map_err(|e| format!("读取包失败：{e}"))?;
    if meta.len() > MAX_ZIP_BYTES {
        return Err(format!("包体积超限（>{} MiB）", MAX_ZIP_BYTES / 1024 / 1024));
    }
    let file = fs::File::open(zip_path).map_err(|e| format!("打开包失败：{e}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("不是有效的 zip：{e}"))?;
    if archive.len() > MAX_FILES {
        return Err(format!("条目数超限（>{MAX_FILES}）"));
    }

    // 先校验元数据（不落盘）：数据包必须声明归属，防止装错插件
    let meta_text = read_meta(&mut archive)?;
    let want = plugin_id.to_string();
    let got = meta_text
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if got != want {
        return Err(format!(
            "数据包归属不符：包内是 {got:?}，目标是 {want:?}（拒绝导入）"
        ));
    }

    // staging 同文件系统：解包全过才原子替换
    let staging = staging_dir(data_dir);
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging).map_err(|e| format!("创建暂存目录失败：{e}"))?;
    let result = extract_data(&mut archive, &staging);
    if let Err(e) = result {
        let _ = fs::remove_dir_all(&staging);
        return Err(e);
    }

    if data_dir.exists() {
        fs::remove_dir_all(data_dir).map_err(|e| format!("清理旧数据失败：{e}"))?;
    }
    if let Some(parent) = data_dir.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建数据根失败：{e}"))?;
    }
    fs::rename(&staging, data_dir).map_err(|e| format!("提交导入失败：{e}"))?;
    let _ = fs::set_permissions(data_dir, fs::Permissions::from_mode(0o700));
    Ok(archive.len() as u64 - 1) // 减去元数据条目
}

fn read_meta(archive: &mut zip::ZipArchive<fs::File>) -> Result<serde_json::Value, String> {
    let mut entry = archive
        .by_name(META_ENTRY)
        .map_err(|_| "数据包缺 yihu-data.json 元数据（不是一呼导出的数据包）".to_string())?;
    let mut text = String::new();
    entry
        .read_to_string(&mut text)
        .map_err(|e| format!("读取元数据失败：{e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("元数据非法：{e}"))
}

fn extract_data(
    archive: &mut zip::ZipArchive<fs::File>,
    staging: &Path,
) -> Result<(), String> {
    let mut used: u64 = 0;
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|e| format!("读取条目 {i} 失败：{e}"))?;
        let name = entry.name().to_string();
        if name == META_ENTRY {
            continue;
        }
        check_entry_name(&name)?;
        if entry.is_dir() {
            fs::create_dir_all(staging.join(&name))
                .map_err(|e| format!("建目录 {name} 失败：{e}"))?;
            continue;
        }
        if entry.is_symlink() {
            return Err(format!("条目 {name} 是符号链接——拒绝导入"));
        }
        let declared = entry.size();
        let compressed = entry.compressed_size().max(1);
        if compressed > 1024 && declared / compressed > MAX_RATIO {
            return Err(format!("条目 {name} 压缩比异常（疑似炸弹）"));
        }
        // 父目录显式建（导出包不写目录条目）
        if let Some(parent) = Path::new(&name).parent() {
            fs::create_dir_all(staging.join(parent))
                .map_err(|e| format!("建目录 {name} 失败：{e}"))?;
        }
        let mut out = fs::File::create(staging.join(&name))
            .map_err(|e| format!("写条目 {name} 失败：{e}"))?;
        let budget = MAX_UNCOMPRESSED - used;
        let mut limited = entry.take(budget.saturating_add(1));
        let copied = io::copy(&mut limited, &mut out)
            .map_err(|e| format!("解包 {name} 失败：{e}"))?;
        if copied > budget {
            return Err("解包总量超限（zip 炸弹防护）".into());
        }
        if copied != declared {
            return Err(format!("条目 {name} 实际大小 {copied} 与声明 {declared} 不符"));
        }
        used += copied;
    }
    if used > MAX_UNCOMPRESSED {
        return Err("解包总量超限（zip 炸弹防护）".into());
    }
    Ok(())
}

/// staging 目录：数据目录同级的点开头目录（不进注册表扫描，互不干扰）
fn staging_dir(data_dir: &Path) -> PathBuf {
    let name = data_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "data".into());
    data_dir
        .parent()
        .unwrap_or_else(|| Path::new("/tmp"))
        .join(format!(
            ".staging-data-{}-{}-{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos()
        ))
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

/// 清空数据目录（重建空目录并保持 0700）。
pub fn clear_data(dir: &Path) -> io::Result<()> {
    if dir.exists() {
        fs::remove_dir_all(dir)?;
    }
    fs::create_dir_all(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("yihu-datactl-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn write_zip(path: &Path, entries: &[(&str, &str)], meta_id: &str) {
        let file = fs::File::create(path).unwrap();
        let mut w = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        w.start_file(META_ENTRY, opts).unwrap();
        w.write_all(format!("{{\"id\":\"{meta_id}\"}}").as_bytes()).unwrap();
        for (name, content) in entries {
            w.start_file(*name, opts).unwrap();
            w.write_all(content.as_bytes()).unwrap();
        }
        w.finish().unwrap();
    }

    #[test]
    fn stats_counts_files_and_bytes() {
        let base = tmp("stats");
        let dir = base.join("plugin-data/a");
        assert!(data_stats(&dir).is_none(), "不存在时 None");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("x.json"), "0123456789").unwrap();
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("sub/y.json"), "abc").unwrap();
        let (files, bytes) = data_stats(&dir).unwrap();
        assert_eq!((files, bytes), (2, 13));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn export_import_roundtrip() {
        let base = tmp("roundtrip");
        let dir = base.join("plugin-data/demo");
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("notes.json"), "{\"k\":1}").unwrap();
        fs::write(dir.join("sub/deep.bin"), "payload").unwrap();

        let zpath = base.join("demo-data.zip");
        let n = export_data_zip(&dir, "demo", &zpath).unwrap();
        assert_eq!(n, 2);

        // 导入到另一台「机器」的目录
        let dest = base.join("other/plugin-data/demo");
        let n = import_data_zip(&zpath, "demo", &dest).unwrap();
        assert_eq!(n, 2);
        let text = fs::read_to_string(dest.join("notes.json")).unwrap();
        assert_eq!(text, "{\"k\":1}");
        assert!(dest.join("sub/deep.bin").is_file(), "父目录条目缺省也能还原");
        let mode = fs::metadata(&dest).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        // 无 staging 残留
        let leftovers: Vec<_> = fs::read_dir(dest.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".staging"))
            .collect();
        assert!(leftovers.is_empty());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn import_rejects_wrong_id_and_foreign_meta() {
        let base = tmp("idmatch");
        let zpath = base.join("x.zip");
        write_zip(&zpath, &[("a.txt", "hi")], "other-plugin");
        let dest = base.join("plugin-data/demo");
        let e = import_data_zip(&zpath, "demo", &dest).unwrap_err();
        assert!(e.contains("归属不符"), "{e}");
        assert!(!dest.exists(), "拒绝导入不得落盘");

        write_zip(&zpath, &[("a.txt", "hi")], "demo");
        // 缺元数据的包（外部手工打的 zip）同样拒绝
        let file = fs::File::create(base.join("nometa.zip")).unwrap();
        let mut w = zip::ZipWriter::new(file);
        w.start_file("a.txt", zip::write::SimpleFileOptions::default()).unwrap();
        w.write_all(b"hi").unwrap();
        w.finish().unwrap();
        let e = import_data_zip(&base.join("nometa.zip"), "demo", &dest).unwrap_err();
        assert!(e.contains("元数据") || e.contains("yihu-data"), "{e}");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn import_rejects_traversal_and_replaces_existing() {
        let base = tmp("traversal");
        let zpath = base.join("evil.zip");
        write_zip(&zpath, &[("../escape.txt", "x")], "demo");
        let dest = base.join("plugin-data/demo");
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join("old.txt"), "old").unwrap();
        let e = import_data_zip(&zpath, "demo", &dest).unwrap_err();
        assert!(e.contains("非法") || e.contains("穿越"), "{e}");
        assert!(!base.join("escape.txt").exists());

        // 合法包导入 = 整目录替换，旧数据不残留
        let zpath2 = base.join("good.zip");
        write_zip(&zpath2, &[("new.txt", "new")], "demo");
        import_data_zip(&zpath2, "demo", &dest).unwrap();
        assert!(dest.join("new.txt").is_file());
        assert!(!dest.join("old.txt").exists());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn clear_keeps_0700_dir() {
        let base = tmp("clear");
        let dir = base.join("plugin-data/a");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("x"), "x").unwrap();
        clear_data(&dir).unwrap();
        assert!(dir.is_dir() && fs::read_dir(&dir).unwrap().count() == 0);
        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        let _ = fs::remove_dir_all(&base);
    }
}
