//! HLS media playlist 解析与 URL 拼接。
//!
//! 蜻蜓清单里的分片是协议相对（`//host/…`）或根相对（`/path`）地址，
//! GStreamer 的 HLS 解析器会错误拼接导致 404 无声（M5 实测）。旧 gst
//! 路线靠「镜像线程改写清单落 /tmp」绕过；内嵌引擎直接在内存里拼出
//! 绝对 URL，镜像文件机制整个消失。

/// media playlist 里我们关心的全部内容。
#[derive(Debug, Default, PartialEq)]
pub struct Playlist {
    /// EXT-X-TARGETDURATION（秒）；缺失为 0，调用方回退固定间隔。
    pub target_duration: u64,
    /// EXT-X-MEDIA-SEQUENCE：清单内首分片的全局序号，缺失为 0。
    pub media_sequence: u64,
    /// 分片绝对 URL（按清单顺序）。
    pub segments: Vec<String>,
    /// EXT-X-ENDLIST：点播/直播结束标志。
    pub end_list: bool,
}

/// 把 base（清单 URL）上的相对引用拼成绝对 URL：
/// 带 `scheme://` 原样；`//host/…` 补 scheme；`/path` 补 scheme+host；
/// 其余按 base 所在目录拼接。base 无 scheme 时按 https 处理（蜻蜓口径）。
pub fn join_url(base: &str, reference: &str) -> String {
    if reference.contains("://") {
        return reference.to_owned();
    }
    let (scheme, authority) = match base.split_once("://") {
        Some((scheme, rest)) => (
            scheme.to_owned(),
            rest.split('/').next().unwrap_or_default().to_owned(),
        ),
        None => ("https".to_owned(), String::new()),
    };
    if let Some(rest) = reference.strip_prefix("//") {
        return format!("{scheme}://{rest}");
    }
    if reference.starts_with('/') {
        return format!("{scheme}://{authority}{reference}");
    }
    let dir = match base.rfind('/') {
        Some(i) if base.contains("://") => &base[..=i],
        _ => "",
    };
    format!("{dir}{reference}")
}

/// 解析 media playlist。非 `#` 行即分片 URI（media playlist 的约定），
/// 只识别直播推进需要的 4 类 tag，其余忽略。
pub fn parse_playlist(text: &str, base_url: &str) -> Playlist {
    let mut pl = Playlist::default();
    for line in text.lines() {
        let line = line.trim_end_matches('\r').trim();
        if line.is_empty() {
            continue;
        }
        if let Some(tag) = line.strip_prefix('#') {
            if let Some(v) = tag.strip_prefix("EXT-X-TARGETDURATION:") {
                pl.target_duration = v.trim().parse().unwrap_or(0);
            } else if let Some(v) = tag.strip_prefix("EXT-X-MEDIA-SEQUENCE:") {
                pl.media_sequence = v.trim().parse().unwrap_or(0);
            } else if tag.starts_with("EXT-X-ENDLIST") {
                pl.end_list = true;
            }
            continue;
        }
        pl.segments.push(join_url(base_url, line));
    }
    pl
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 协议相对_根相对_绝对地址() {
        let base = "https://ls.qingting.fm/live/1133/64k.m3u8";
        assert_eq!(
            join_url(base, "//ls-hw-ot.qtfm.cn/live/1133/64k/a.ts"),
            "https://ls-hw-ot.qtfm.cn/live/1133/64k/a.ts"
        );
        assert_eq!(
            join_url(base, "/live/1133/64k/a.ts"),
            "https://ls.qingting.fm/live/1133/64k/a.ts"
        );
        assert_eq!(
            join_url(base, "https://cdn.example.com/b.ts"),
            "https://cdn.example.com/b.ts"
        );
    }

    #[test]
    fn 目录相对拼接() {
        let base = "https://ls-hw-ot.qtfm.cn/live/1133/64k/20261019/19/list.m3u8";
        assert_eq!(
            join_url(base, "2340-779-560102.ts"),
            "https://ls-hw-ot.qtfm.cn/live/1133/64k/20261019/19/2340-779-560102.ts"
        );
    }

    #[test]
    fn 解析直播清单推进要素() {
        let text = "#EXTM3U\n\
                    #EXT-X-VERSION:3\n\
                    #EXT-X-TARGETDURATION:7\n\
                    #EXT-X-MEDIA-SEQUENCE:100\n\
                    #EXTINF:7.01,\n//h.cn/a.ts\n\
                    #EXTINF:7.0,\n//h.cn/b.ts\n";
        let pl = parse_playlist(text, "https://ls.qingting.fm/live/1/64k.m3u8");
        assert_eq!(pl.target_duration, 7);
        assert_eq!(pl.media_sequence, 100);
        assert_eq!(pl.segments, vec!["https://h.cn/a.ts", "https://h.cn/b.ts"]);
        assert!(!pl.end_list);
    }

    #[test]
    fn 解析点播结束标志与缺失字段() {
        let text = "#EXTM3U\r\n#EXTINF:7.0,\r\n/a.ts\r\n#EXT-X-ENDLIST\r\n";
        let pl = parse_playlist(text, "https://h.cn/live/x.m3u8");
        assert!(pl.end_list);
        assert_eq!(pl.target_duration, 0);
        assert_eq!(pl.media_sequence, 0);
        assert_eq!(pl.segments, vec!["https://h.cn/a.ts"]);
    }
}
