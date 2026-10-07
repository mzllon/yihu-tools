//! 插件市场 v0（M4 一期④）：registry 拉取 + 下载 + sha256 核验。
//!
//! 边界（M4 冻结决策 #6 + 安全模型 §5）：市场功能只存在于中心应用，
//! 呼出路径零网络零 IO；registry 是 GitHub PR 审核维护的静态 JSON，
//! 每个包绑定 sha256 与体积，安装前核验；ed25519 签名信任根列 M5
//! 正式市场前（v0 没有信任根，先签名是无根之木）。
//! 网络只在中心应用的后台线程发生（GTK 主线程禁阻塞，与安装同惯例）。

use serde::Deserialize;
use std::io::Write;
use std::path::Path;

/// registry.json 单条记录
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct MarketPlugin {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub desc: String,
    pub url: String,
    pub sha256: String,
    /// 期望体积（字节），与下载实际值核对
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub permissions: Vec<String>,
}

/// 解析 registry 文本。缺 id/name/url/sha256 的条目报错（严格模式：
/// 一条坏记录整表拒绝，避免半成品索引被静默降级）。
pub fn parse_registry(text: &str) -> Result<Vec<MarketPlugin>, String> {
    #[derive(Deserialize)]
    struct Root {
        #[serde(default)]
        plugins: Vec<MarketPlugin>,
    }
    let root: Root =
        serde_json::from_str(text).map_err(|e| format!("registry 解析失败：{e}"))?;
    for p in &root.plugins {
        if p.id.is_empty() || p.name.is_empty() || p.url.is_empty() || p.sha256.is_empty() {
            return Err(format!("registry 条目缺关键字段：{}", p.id));
        }
        if p.sha256.len() != 64 || !p.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("registry 条目 {} sha256 非法", p.id));
        }
    }
    Ok(root.plugins)
}

/// 拉取并解析 registry（阻塞调用，只准后台线程用）
pub fn fetch_registry(url: &str) -> Result<Vec<MarketPlugin>, String> {
    let resp = ureq::get(url)
        .timeout(std::time::Duration::from_secs(15))
        .call()
        .map_err(|e| format!("registry 拉取失败：{e}"))?;
    let text = resp.into_string().map_err(|e| format!("registry 读取失败：{e}"))?;
    if text.len() > 4 * 1024 * 1024 {
        return Err("registry 超限（>4 MiB）".into());
    }
    parse_registry(&text)
}

/// 下载 zip 到目标路径（阻塞调用，只准后台线程用）。体积上限 64 MiB
/// 与 zipfile::MAX_ZIP_BYTES 一致，超限即中止。
pub fn download_to(url: &str, dest: &Path) -> Result<u64, String> {
    let resp = ureq::get(url)
        .timeout(std::time::Duration::from_secs(60))
        .call()
        .map_err(|e| format!("下载失败：{e}"))?;
    // into_reader 是 trait object（Read::take 要求 Sized），手动限流读取
    let mut reader = resp.into_reader();
    let limit: u64 = 64 * 1024 * 1024;
    let mut n: u64 = 0;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let got = reader.read(&mut buf).map_err(|e| format!("下载中断：{e}"))?;
        if got == 0 {
            break;
        }
        n += got as u64;
        if n > limit {
            return Err("包体积超限（>64 MiB）".into());
        }
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(dest)
            .map_err(|e| format!("建临时文件失败：{e}"))?;
        file.write_all(&buf[..got]).map_err(|e| format!("写入失败：{e}"))?;
    }
    if n == 0 {
        return Err("下载内容为空".into());
    }
    Ok(n)
}

/// 默认市场地址（市场仓库建立前 404，页面会显示拉取失败提示）
pub const DEFAULT_REGISTRY_URL: &str =
    "https://raw.githubusercontent.com/mzllon/yihu-market/main/registry.json";

/// 市场条目声明权限与包内 manifest 声明权限是否一致（集合相等，序无关）。
/// 「安装时权限明示 = 授权时点」依赖它闭环：registry 展示面必须与
/// 实际生效面一致，不一致 = 拒装（审查 I-3）。
pub fn permissions_match(declared: &[String], manifest: &[String]) -> bool {
    let mut a: Vec<&str> = declared.iter().map(|s| s.as_str()).collect();
    let mut b: Vec<&str> = manifest.iter().map(|s| s.as_str()).collect();
    a.sort_unstable();
    b.sort_unstable();
    a == b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_registry() {
        let text = r#"{"plugins":[
            {"id":"ts-convert","name":"时间戳","version":"0.2.0","desc":"d",
             "url":"https://x/a.zip","sha256":"abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789",
             "size":1024,"permissions":["clipboard.write"]},
            {"id":"b","name":"B","url":"https://x/b.zip","sha256":"ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789"}
        ]}"#;
        let list = parse_registry(text).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "ts-convert");
        assert_eq!(list[1].permissions.len(), 0);
    }

    #[test]
    fn rejects_incomplete_entries() {
        let missing_sha = r#"{"plugins":[{"id":"a","name":"A","url":"https://x/a.zip"}]}"#;
        assert!(parse_registry(missing_sha).is_err());
        let bad_sha = r#"{"plugins":[{"id":"a","name":"A","url":"https://x/a.zip","sha256":"xyz"}]}"#;
        assert!(parse_registry(bad_sha).unwrap_err().contains("sha256 非法"));
        let garbage = "not json";
        assert!(parse_registry(garbage).unwrap_err().contains("解析失败"));
        let empty = "{}";
        assert!(parse_registry(empty).unwrap().is_empty());
    }

    #[test]
    fn permissions_match_is_set_equality() {
        let s = |v: &[&str]| -> Vec<String> { v.iter().map(|x| x.to_string()).collect() };
        assert!(permissions_match(&s(&["a", "b"]), &s(&["b", "a"])));
        assert!(permissions_match(&s(&[]), &s(&[])));
        assert!(!permissions_match(&s(&[]), &s(&["clipboard.write"])));
        assert!(!permissions_match(&s(&["a"]), &s(&["a", "b"])));
        assert!(!permissions_match(&s(&["a", "a"]), &s(&["a"])));
    }
}
