//! 蜻蜓 FM API 客户端。
//!
//! 端点契约与实测样例见 `docs/蜻蜓FM-API调研.md`（必须同源维护）：
//! 端点常量集中此处便于上游变更时热修；全部请求带超时（API 无 SLA）。

use std::io::Read as _;
use std::time::Duration;

use serde::Deserialize;

// —— 端点常量（热修只动这里）——
const CATEGORIES_URL: &str = "https://rapi.qtfm.cn/categories?type=channel";
const REGIONS_URL: &str = "https://rapi.qtfm.cn/regions?all=true";
const CHANNELS_URL: &str = "https://rapi.qtfm.cn/categories";
const BILLBOARDS_URL: &str = "https://rapi.qtfm.cn/billboards";
const DETAIL_URL: &str = "https://rapi.qingting.fm/v4/channels";
const SEARCH_URL: &str = "https://search.qingting.fm/v3/search?k=";
const STREAM_BASE: &str = "https://ls.qingting.fm/live";

const TIMEOUT: Duration = Duration::from_secs(10);
/// 分类/地区频道分页大小（实测可用；无 total 字段，翻页靠追加）
pub const PAGE_SIZE: u32 = 50;

pub fn channels_url(cat: i64, page: u32) -> String {
    format!("{CHANNELS_URL}/{cat}/channels?page={page}&pagesize={PAGE_SIZE}")
}
pub fn billboards_url(cat: i64, region: i64) -> String {
    format!("{BILLBOARDS_URL}/{cat}/{region}/channels")
}
pub fn detail_url(cid: i64) -> String {
    format!("{DETAIL_URL}/{cid}")
}
pub fn search_url(kw: &str) -> String {
    format!("{SEARCH_URL}{}", urlencoding_lite(kw))
}
pub fn stream_url(cid: i64) -> String {
    format!("{STREAM_BASE}/{cid}/64k.m3u8")
}

/// 关键词 query 转义（搜索端点唯一需要 encode 的参数；不引依赖手写
/// percent-encode 的保守子集：非保留字符外的全部转义）
fn urlencoding_lite(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(TIMEOUT).build()
}

fn get_json(url: &str) -> Result<serde_json::Value, String> {
    let resp = agent()
        .get(url)
        .call()
        .map_err(|e| format!("请求失败：{e}"))?;
    let mut text = String::new();
    resp.into_reader()
        .take(4 * 1024 * 1024)
        .read_to_string(&mut text)
        .map_err(|e| format!("响应读取失败：{e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("响应解析失败：{e}"))
}

// —— 数据模型（列表条目 / 搜索 docs / v4 详情三源共用，字段宽容缺省）——

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Named {
    pub id: i64,
    #[serde(default)]
    pub title: String,
    /// 数据模型完整性保留（层级推断备用）
    #[serde(default)]
    #[allow(dead_code)]
    pub pid: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct NowPlaying {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub start_time: String,
    #[serde(default)]
    pub end_time: String,
}

impl NowPlaying {
    pub fn display_name(&self) -> &str {
        if self.name.is_empty() {
            &self.title
        } else {
            &self.name
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Channel {
    #[serde(default, alias = "content_id")]
    pub id: i64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub cover: String,
    /// 数据模型完整性保留（详情展开备用）
    #[serde(default)]
    #[allow(dead_code)]
    pub description: String,
    #[serde(default)]
    pub nowplaying: Option<NowPlaying>,
    #[serde(default)]
    pub audience_count: Option<serde_json::Value>,
    #[serde(default)]
    pub top_category_title: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub region: Option<Named>,
    #[serde(default)]
    pub city: Option<Named>,
    #[serde(default)]
    #[allow(dead_code)]
    pub categories: Vec<Named>,
    /// 搜索 docs 条目的类型标记（channel_live = 直播台）
    #[serde(default, rename = "type")]
    #[allow(dead_code)]
    pub kind: String,
}

impl Channel {
    /// 听众数文本（数字/字符串宽容；无则空）
    pub fn audience_str(&self) -> String {
        match &self.audience_count {
            Some(serde_json::Value::Number(n)) => n.to_string(),
            Some(serde_json::Value::String(s)) => s.clone(),
            _ => String::new(),
        }
    }

    /// 正在播展示行：「🔴 节目名 HH:MM-HH:MM」（无节目 = 空）
    pub fn np_line(&self) -> String {
        match &self.nowplaying {
            Some(np) if !np.display_name().is_empty() => {
                let span = if np.start_time.is_empty() {
                    String::new()
                } else {
                    // 只剥一层秒位（"11:00:00"→"11:00"）
                    let s = np.start_time.strip_suffix(":00").unwrap_or(&np.start_time);
                    let e = np.end_time.strip_suffix(":00").unwrap_or(&np.end_time);
                    format!(" {s}-{e}")
                };
                format!("🔴 {}{}", np.display_name(), span)
            }
            _ => String::new(),
        }
    }

    /// 列表行副标题：正在播 · 城市 · 听众
    pub fn sub_line(&self) -> String {
        let mut parts = Vec::new();
        let np = self.np_line();
        if !np.is_empty() {
            parts.push(np);
        }
        if let Some(c) = &self.city {
            if !c.title.is_empty() {
                parts.push(c.title.clone());
            }
        }
        let aud = self.audience_str();
        if !aud.is_empty() {
            parts.push(format!("{aud} 听众"));
        }
        parts.join(" · ")
    }

    /// 播放页元信息行：分类 · 城市 · 听众
    pub fn meta_line(&self) -> String {
        let mut parts = Vec::new();
        if let Some(t) = &self.top_category_title {
            if !t.is_empty() {
                parts.push(t.clone());
            }
        }
        if let Some(c) = &self.city {
            if !c.title.is_empty() {
                parts.push(c.title.clone());
            }
        }
        let aud = self.audience_str();
        if !aud.is_empty() {
            parts.push(format!("{aud} 听众"));
        }
        parts.join(" · ")
    }

    /// 封面地址归一（http:// 与 https:// 都要支持；协议相对补 https）
    pub fn cover_url(&self) -> String {
        let c = &self.cover;
        if c.is_empty() {
            return String::new();
        }
        if let Some(rest) = c.strip_prefix("//") {
            return format!("https://{rest}");
        }
        c.clone()
    }
}

// —— 端点封装 ——

/// 内容分类（13 类，实测 2026-10-16）
pub fn categories() -> Result<Vec<Named>, String> {
    let v = get_json(CATEGORIES_URL)?;
    Ok(v.get("Data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| serde_json::from_value(e.clone()).ok())
                .collect()
        })
        .unwrap_or_default())
}

/// 地区（31 省级行政区 + 407 网络台）
pub fn regions() -> Result<Vec<Named>, String> {
    let v = get_json(REGIONS_URL)?;
    Ok(v.get("Data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| serde_json::from_value(e.clone()).ok())
                .collect()
        })
        .unwrap_or_default())
}

/// 分类下频道（分页；region_id 参数服务端忽略，见调研 §2）
pub fn category_channels(cat: i64, page: u32) -> Result<Vec<Channel>, String> {
    let v = get_json(&channels_url(cat, page))?;
    Ok(parse_data_channels(&v))
}

/// 分类 × 地区组合榜单（地区浏览的正解，见调研 §4；cat=0 全部分类）
pub fn billboards(cat: i64, region: i64) -> Result<Vec<Channel>, String> {
    let v = get_json(&billboards_url(cat, region))?;
    Ok(parse_data_channels(&v))
}

/// 频道详情（正在播/元信息刷新的数据源，2026-10-19 实测）
pub fn detail(cid: i64) -> Result<Channel, String> {
    let v = get_json(&detail_url(cid))?;
    let data = v
        .get("data")
        .ok_or_else(|| format!("详情响应缺 data：{}", v.get("errmsg").and_then(|e| e.as_str()).unwrap_or("?")))?;
    serde_json::from_value(data.clone()).map_err(|e| format!("详情字段解析失败：{e}"))
}

/// 搜索（只取 type == channel_live 的直播台）
pub fn search(kw: &str) -> Result<Vec<Channel>, String> {
    let v = get_json(&search_url(kw))?;
    let docs = v
        .pointer("/data/data/docs")
        .and_then(|d| d.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(docs
        .into_iter()
        .filter(|d| d.get("type").and_then(|t| t.as_str()) == Some("channel_live"))
        .filter_map(|d| serde_json::from_value::<Channel>(d).ok())
        .filter(|c| c.id > 0)
        .collect())
}

fn parse_data_channels(v: &serde_json::Value) -> Vec<Channel> {
    v.get("Data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| serde_json::from_value::<Channel>(e.clone()).ok())
                .filter(|c| c.id > 0)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_urls() {
        assert_eq!(
            channels_url(429, 2),
            "https://rapi.qtfm.cn/categories/429/channels?page=2&pagesize=50"
        );
        assert_eq!(
            billboards_url(0, 99),
            "https://rapi.qtfm.cn/billboards/0/99/channels"
        );
        assert_eq!(
            stream_url(1133),
            "https://ls.qingting.fm/live/1133/64k.m3u8"
        );
        assert_eq!(
            search_url("杭州 91.8"),
            "https://search.qingting.fm/v3/search?k=%E6%9D%AD%E5%B7%9E%2091.8"
        );
    }

    #[test]
    fn np_and_sub_lines() {
        let mut c = Channel::default();
        c.nowplaying = Some(NowPlaying {
            name: "我的汽车有话说".into(),
            title: String::new(),
            start_time: "11:00:00".into(),
            end_time: "12:30:00".into(),
        });
        assert_eq!(c.np_line(), "🔴 我的汽车有话说 11:00-12:30");
        c.city = Some(Named { id: 100, title: "杭州市".into(), pid: Some(99) });
        c.audience_count = Some(serde_json::json!(584054));
        assert_eq!(c.sub_line(), "🔴 我的汽车有话说 11:00-12:30 · 杭州市 · 584054 听众");
        c.meta_line();
    }

    #[test]
    fn cover_url_schemes() {
        let mut c = Channel::default();
        c.cover = "//pic.qtfm.cn/a.jpg".into();
        assert_eq!(c.cover_url(), "https://pic.qtfm.cn/a.jpg");
        c.cover = "http://pic.qtfm.cn/b.png".into();
        assert_eq!(c.cover_url(), "http://pic.qtfm.cn/b.png");
        c.cover = String::new();
        assert_eq!(c.cover_url(), "");
    }

    #[test]
    fn parses_detail_envelope() {
        // 2026-10-19 实测响应的关键字段（截取）
        let raw = serde_json::json!({
            "errcode": 0, "errmsg": "ok",
            "data": {
                "id": 1133, "title": "杭州交通91.8电台",
                "cover": "http://pic.qtfm.cn/2014/0829/x.png",
                "nowplaying": {"name": "我的汽车有话说", "title": "我的汽车有话说",
                               "start_time": "11:00:00", "end_time": "12:30:00"},
                "audience_count": 584054,
                "top_category_title": "交通台",
                "region": {"id": 99, "title": "浙江"},
                "city": {"id": 100, "title": "杭州市", "pid": 99}
            }
        });
        let c: Channel = serde_json::from_value(raw["data"].clone()).unwrap();
        assert_eq!(c.id, 1133);
        assert_eq!(c.top_category_title.as_deref(), Some("交通台"));
        assert!(c.np_line().contains("我的汽车有话说"));
    }

    #[test]
    fn parses_search_doc_shape() {
        let doc = serde_json::json!({
            "id": 249897, "title": "好听音乐广播", "type": "channel_live",
            "cover": "//pic.qtfm.cn/x.jpg", "audience_count": "12000"
        });
        assert_eq!(doc.get("type").and_then(|t| t.as_str()), Some("channel_live"));
        let c: Channel = serde_json::from_value(doc).unwrap();
        assert_eq!(c.id, 249897);
        assert_eq!(c.audience_str(), "12000");
    }
}
