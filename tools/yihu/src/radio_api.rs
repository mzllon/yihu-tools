//! 中心「广播」页数据层：蜻蜓FM 公开 Web 接口的只读访问与收藏持久化。
//!
//! 该接口为第三方客户端通行用法（无官方协议保障），仅限个人收听；
//! 字段解析失败只影响在线列表，收藏与播放不依赖接口保持稳定。
//! HTTP 走 gio::File（TLS 由 glib-networking 提供），不引入新依赖。

use gtk::gio;
use gtk::gio::prelude::FileExtManual;
use yihu_core::paths;
use serde_json::Value;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// 「我的收藏」伪分类 id，仅存在于界面层。
pub const FAVORITES_ID: u64 = 0;
/// 单页频道数；返回条数等于该值时按“可能还有下一页”处理。
pub const PAGE_SIZE: u32 = 50;

#[derive(Clone, Debug, PartialEq)]
pub struct Category {
    pub id: u64,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Channel {
    pub content_id: u64,
    pub title: String,
    pub description: String,
    pub now_playing: Option<String>,
    pub cover: String,
    pub audience_count: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChannelPage {
    pub channels: Vec<Channel>,
    pub has_more: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Favorite {
    pub content_id: u64,
    pub title: String,
    /// 封面地址；旧收藏文件可能没有该字段，缺省为空。
    pub cover: String,
}

pub fn categories_url() -> String {
    "https://rapi.qtfm.cn/categories?type=channel".into()
}

pub fn channels_url(category: u64, page: u32) -> String {
    format!("https://rapi.qtfm.cn/categories/{category}/channels?page={page}&pagesize={PAGE_SIZE}")
}

/// 搜索接口返回混合文档（点播节目/电台等），界面只取直播电台。
pub fn search_url(keyword: &str) -> String {
    format!("https://search.qingting.fm/v3/search?k={}", urlencode(keyword))
}

pub fn stream_url(content_id: u64) -> String {
    format!("https://ls.qingting.fm/live/{content_id}/64k.m3u8")
}

/// 非 URL 保留字符的百分号编码（关键词为任意用户输入）。
pub fn urlencode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// 阻塞式 GET（原始字节，供封面下载），供后台线程调用。
pub fn fetch_bytes(uri: &str) -> Result<Vec<u8>, String> {
    let (bytes, _) = gio::File::for_uri(uri)
        .load_contents(gio::Cancellable::NONE)
        .map_err(|e| e.to_string())?;
    Ok(bytes.to_vec())
}

/// 阻塞式 GET（文本），供后台线程调用；错误统一转 String 便于界面行内提示。
pub fn fetch_text(uri: &str) -> Result<String, String> {
    String::from_utf8(fetch_bytes(uri)?).map_err(|_| "响应不是有效 UTF-8".to_owned())
}

pub fn parse_categories(json: &str) -> Result<Vec<Category>, String> {
    let root: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let items = root.get("Data").and_then(Value::as_array).ok_or("分类响应缺少 Data")?;
    Ok(items
        .iter()
        .filter_map(|it| {
            Some(Category {
                id: it.get("id")?.as_u64()?,
                title: it.get("title")?.as_str()?.to_owned(),
            })
        })
        .collect())
}

fn parse_channel_item(it: &Value) -> Option<Channel> {
    Some(Channel {
        content_id: it.get("content_id").or_else(|| it.get("id"))?.as_u64()?,
        title: it.get("title")?.as_str()?.to_owned(),
        description: it
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        now_playing: it
            .get("nowplaying")
            .and_then(|np| np.as_object())
            .and_then(|np| {
                np.get("name")
                    .or_else(|| np.get("title"))
                    .and_then(Value::as_str)
            })
            .map(str::to_owned),
        cover: it
            .get("cover")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        audience_count: it
            .get("audience_count")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    })
}

pub fn parse_channels(json: &str) -> Result<ChannelPage, String> {
    let root: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let items = root.get("Data").and_then(Value::as_array).ok_or("频道响应缺少 Data")?;
    let channels = items.iter().filter_map(parse_channel_item).collect();
    Ok(ChannelPage {
        has_more: items.len() == PAGE_SIZE as usize,
        channels,
    })
}

pub fn parse_search(json: &str) -> Result<Vec<Channel>, String> {
    let root: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let docs = root
        .get("data")
        .and_then(|d| d.get("data"))
        .and_then(|d| d.get("docs"))
        .and_then(Value::as_array)
        .ok_or("搜索响应缺少 docs")?;
    Ok(docs
        .iter()
        .filter(|it| it.get("type").and_then(Value::as_str) == Some("channel_live"))
        .filter_map(parse_channel_item)
        .collect())
}

// ---- 收藏（~/.config/yihu/radio_favorites.json；旧版路径自动迁移）----

pub fn favorites_path() -> PathBuf {
    paths::config_file("radio_favorites.json")
}

pub fn load_favorites_from(path: &Path) -> Vec<Favorite> {
    favorites_from_text(&fs::read_to_string(path).unwrap_or_default())
}

pub fn load_favorites() -> Vec<Favorite> {
    let current = favorites_path();
    if current.exists() {
        return load_favorites_from(&current);
    }
    let legacy = paths::legacy_config_file("radio_favorites.json");
    let list = load_favorites_from(&legacy);
    let _ = paths::migrate_one("radio_favorites.json");
    list
}

pub fn save_favorites_to(path: &Path, list: &[Favorite]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string(
        &list
            .iter()
            .map(|f| {
                serde_json::json!({
                    "content_id": f.content_id,
                    "title": f.title,
                    "cover": f.cover,
                })
            })
            .collect::<Vec<_>>(),
    )
    .expect("收藏序列化必不失败");
    fs::write(path, text)
}

pub fn save_favorites(list: &[Favorite]) -> io::Result<()> {
    save_favorites_to(&favorites_path(), list)
}

pub fn favorites_from_text(text: &str) -> Vec<Favorite> {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .map(|arr| {
            arr.iter()
                .filter_map(|it| {
                    Some(Favorite {
                        content_id: it.get("content_id")?.as_u64()?,
                        title: it.get("title")?.as_str()?.to_owned(),
                        cover: it
                            .get("cover")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_categories_fixture() {
        let cats = parse_categories(
            r#"{"Success":"ok","Data":[{"id":433,"title":"资讯台"},{"id":442,"title":"音乐台"}]}"#,
        )
        .unwrap();
        assert_eq!(cats.len(), 2);
        assert_eq!(cats[0], Category { id: 433, title: "资讯台".into() });
    }

    #[test]
    fn parses_channels_fixture_with_optional_fields() {
        let json = r#"{"Success":"ok","Data":[
            {"content_id":20500149,"title":"两广之声音乐台","description":"简介",
             "cover":"http://pic.qtfm.cn/a.jpeg","audience_count":"61.4万",
             "nowplaying":{"id":15029197,"name":"讲东讲西"}},
            {"content_id":20500150,"title":"无节目台","nowplaying":null},
            {"content_id":20500151,"title":"缺字段台"}
        ]}"#;
        let page = parse_channels(json).unwrap();
        assert_eq!(page.channels.len(), 3);
        assert_eq!(page.channels[0].now_playing.as_deref(), Some("讲东讲西"));
        assert_eq!(page.channels[0].audience_count, "61.4万");
        assert_eq!(page.channels[1].now_playing, None);
        assert_eq!(page.channels[2].cover, "");
        assert!(!page.has_more);
    }

    #[test]
    fn parses_search_fixture_live_channels_only() {
        let json = r#"{"errcode":0,"data":{"data":{"docs":[
            {"id":1133,"title":"杭州交通91.8电台","type":"channel_live",
             "cover":"http://pic.qingting.fm/a.png","description":" desc ",
             "audience_count":"61.4万"},
            {"id":95832,"title":"爱车91.8","type":"channel_ondemand"},
            {"id":25594497,"title":"节目","type":"program_ondemand"}
        ]}}}"#;
        let list = parse_search(json).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].content_id, 1133);
        assert_eq!(list[0].audience_count, "61.4万");
        assert_eq!(list[0].now_playing, None);
    }

    #[test]
    fn full_page_marks_has_more() {
        let items: Vec<String> = (0..PAGE_SIZE)
            .map(|i| format!(r#"{{"content_id":{i},"title":"t{i}"}}"#))
            .collect();
        let json = format!(r#"{{"Data":[{}]}}"#, items.join(","));
        assert!(parse_channels(&json).unwrap().has_more);
    }

    #[test]
    fn parses_garbage_as_empty() {
        assert!(parse_channels("not json").is_err());
        assert!(parse_search("not json").is_err());
        assert!(favorites_from_text("not json").is_empty());
        assert!(favorites_from_text(r#"{"no":"array"}"#).is_empty());
    }

    #[test]
    fn builds_stream_and_api_urls() {
        assert_eq!(stream_url(20500149), "https://ls.qingting.fm/live/20500149/64k.m3u8");
        assert_eq!(
            channels_url(442, 3),
            format!("https://rapi.qtfm.cn/categories/442/channels?page=3&pagesize={PAGE_SIZE}")
        );
        assert!(categories_url().starts_with("https://"));
        assert_eq!(
            search_url("交通91.8"),
            "https://search.qingting.fm/v3/search?k=%E4%BA%A4%E9%80%9A91.8"
        );
        assert_eq!(urlencode("aB1-_.~"), "aB1-_.~");
    }

    #[test]
    fn favorites_roundtrip_in_temp_dir() {
        let path = std::env::temp_dir().join(format!("yihu-radio-test-{}.json", std::process::id()));
        let list = vec![
            Favorite { content_id: 1, title: "电台甲".into(), cover: "http://c/1.png".into() },
            Favorite { content_id: 2, title: "电台乙".into(), cover: String::new() },
        ];
        save_favorites_to(&path, &list).unwrap();
        assert_eq!(load_favorites_from(&path), list);
        // 旧格式（无 cover 字段）兼容读取
        std::fs::write(&path, r#"[{"content_id":9,"title":"旧收藏"}]"#).unwrap();
        assert_eq!(
            load_favorites_from(&path),
            vec![Favorite { content_id: 9, title: "旧收藏".into(), cover: String::new() }]
        );
        fs::remove_file(&path).ok();
    }
}
