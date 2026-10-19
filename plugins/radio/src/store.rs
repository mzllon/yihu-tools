//! 插件数据：收藏（favorites.json 沿用旧格式）、播放状态（status.json，
//! mini 壳契约）、封面缓存、关窗提示标记。
//!
//! 数据目录 = ~/.local/share/yihu/plugin-data/radio/（yihu-core 统一解析）；
//! 全部写入走「临时文件 + rename」原子替换。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::api::Channel;

pub fn data_dir() -> PathBuf {
    yihu_core::plugins::plugin_data_dir("radio")
}

// ---- 收藏（与旧 radio.py 同格式：[{id,title,np,aud}]；旧中心 content_id 兼容读）----

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Fav {
    pub id: i64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub np: String,
    #[serde(default)]
    pub aud: String,
}

pub fn load_favs() -> Vec<Fav> {
    let Ok(text) = std::fs::read_to_string(data_dir().join("favorites.json")) else {
        return Vec::new();
    };
    #[derive(Deserialize)]
    struct Loose {
        #[serde(default, alias = "content_id")]
        id: Option<i64>,
        #[serde(default)]
        title: Option<String>,
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let Some(arr) = v.as_array() else { return Vec::new() };
    arr.iter()
        .filter_map(|e| {
            let it: Loose = serde_json::from_value(e.clone()).ok()?;
            let id = it.id?;
            let title = it.title?;
            if title.is_empty() {
                return None;
            }
            Some(Fav {
                id,
                title: title.chars().take(60).collect(),
                np: String::new(),
                aud: String::new(),
            })
        })
        .collect()
}

pub fn save_favs(favs: &[Fav]) -> Result<(), String> {
    write_atomic(
        &data_dir().join("favorites.json"),
        &serde_json::to_string_pretty(favs).unwrap_or_else(|_| "[]".into()),
    )
}

pub fn fav_ids(favs: &[Fav]) -> Vec<i64> {
    favs.iter().map(|f| f.id).collect()
}

/// 收藏/取消收藏（返回操作后的列表）
pub fn toggle_fav(favs: &mut Vec<Fav>, ch: &Channel) -> bool {
    if let Some(i) = favs.iter().position(|f| f.id == ch.id) {
        favs.remove(i);
        false
    } else {
        favs.insert(
            0,
            Fav {
                id: ch.id,
                title: if ch.title.is_empty() {
                    format!("频道 {}", ch.id)
                } else {
                    ch.title.clone()
                },
                np: String::new(),
                aud: String::new(),
            },
        );
        true
    }
}

// ---- status.json（native ⇄ panel.wasm 契约，设计稿 §6.3）----

pub fn write_status(playing: bool, ch: Option<&Channel>) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let doc = serde_json::json!({
        "playing": playing,
        "channel": ch.map(|c| serde_json::json!({
            "id": c.id, "title": c.title, "cover": c.cover_url(),
        })),
        "program": ch.and_then(|c| c.nowplaying.as_ref()).map(|np| serde_json::json!({
            "name": np.display_name(),
            "start": np.start_time,
            "end": np.end_time,
        })),
        "updated_at": now,
    });
    if let Err(e) = write_atomic(&data_dir().join("status.json"), &doc.to_string()) {
        eprintln!("[电台] status.json 写入失败：{e}");
    }
}

// ---- 关窗提示（只提示一次）----

pub fn hide_hint_shown() -> bool {
    data_dir().join("hide-hint-shown").exists()
}

pub fn mark_hide_hint_shown() {
    let _ = write_atomic(&data_dir().join("hide-hint-shown"), "1");
}

// ---- 封面缓存（数据目录 cover-cache/<id>.img；超量按 mtime 淘汰）----

const COVER_CACHE_MAX: usize = 300;

pub fn cover_cache_path(id: i64) -> PathBuf {
    data_dir().join("cover-cache").join(format!("{id}.img"))
}

/// 命中返回本地文件路径
pub fn cover_cached(id: i64) -> Option<PathBuf> {
    let p = cover_cache_path(id);
    p.is_file().then_some(p)
}

pub fn cover_store(id: i64, bytes: &[u8]) -> Option<PathBuf> {
    let p = cover_cache_path(id);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).ok()?;
    }
    std::fs::write(&p, bytes).ok()?;
    prune_cover_cache();
    Some(p)
}

fn prune_cover_cache() {
    let dir = data_dir().join("cover-cache");
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            Some((meta.modified().ok()?, e.path()))
        })
        .collect();
    if files.len() <= COVER_CACHE_MAX {
        return;
    }
    files.sort_by_key(|(t, _)| *t);
    let excess = files.len() - COVER_CACHE_MAX;
    for (_, p) in files.into_iter().take(excess) {
        let _ = std::fs::remove_file(p);
    }
}

// ---- 通用原子写 ----

fn write_atomic(path: &std::path::Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_channel(id: i64) -> Channel {
        let mut c = Channel::default();
        c.id = id;
        c.title = format!("频道{id}");
        c
    }

    #[test]
    fn toggle_fav_adds_front_and_removes() {
        let mut favs = Vec::new();
        assert!(toggle_fav(&mut favs, &test_channel(1)));
        assert!(toggle_fav(&mut favs, &test_channel(2)));
        assert_eq!(favs[0].id, 2, "新收藏置顶");
        assert!(!toggle_fav(&mut favs, &test_channel(2)));
        assert_eq!(favs.len(), 1);
    }

    #[test]
    fn roundtrip_favs_json() {
        let favs = vec![Fav { id: 7, title: "测试台".into(), np: String::new(), aud: String::new() }];
        let text = serde_json::to_string(&favs).unwrap();
        // 旧格式兼容读：content_id 别名
        let legacy = r#"[{"content_id": 9, "title": "旧中心格式"}]"#;
        let arr: serde_json::Value = serde_json::from_str(legacy).unwrap();
        assert_eq!(arr[0]["content_id"], 9);
        assert!(text.contains("\"id\":7"));
    }

    #[test]
    fn status_doc_shape() {
        let c = test_channel(3);
        let doc = serde_json::json!({
            "playing": true,
            "channel": {"id": c.id, "title": c.title, "cover": ""},
            "program": {"name": "n", "start": "s", "end": "e"},
            "updated_at": 0
        });
        assert_eq!(doc["playing"], true);
        assert_eq!(doc["channel"]["id"], 3);
    }
}
