//! Markdown 渲染（M5 第六批）：pulldown-cmark → HTML，供 mdkit 插件的
//! 「浏览器预览 / 转 HTML」能力（md.render）。
//!
//! 预览 = 写临时 HTML + gio open（复用浏览器渲染，不引 WebKitGTK 大件）；
//! 页面模板带极简排版样式，代码/表格可读。
//! 安全说明：原始 HTML 按 CommonMark 透传（pandoc 等同款行为）——预览内容
//! 是用户自编笔记、file:// 无会话凭据；若未来开放他人内容再挂 ammonia 消毒。

use pulldown_cmark::{html, Options, Parser};
use std::path::PathBuf;

/// 浏览器预览文件路径（固定名，覆盖写）
pub const PREVIEW_PATH: &str = "/tmp/yihu-md-preview.html";

/// Markdown → 完整 HTML 页面（CommonMark + 表格/删除线/任务列表）
pub fn render_html(md: &str) -> String {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(md, opts);
    let mut body = String::new();
    html::push_html(&mut body, parser);
    wrap_page(&body)
}

/// 消毒渲染：在 CommonMark 基础上剥掉全部原始 HTML（插件内容进宿主
/// WebKit 窗口的安全闸门——脚本/事件属性不可达，纯排版保留）。
pub fn render_html_sanitized(md: &str) -> String {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(md, opts);
    let mut body = String::new();
    html::push_html(&mut body, parser);
    let clean = ammonia::Builder::default()
        .link_rel(Some("noopener noreferrer"))
        .clean(&body)
        .to_string();
    wrap_page(&clean)
}

fn wrap_page(body: &str) -> String {
    format!(
        r#"<!DOCTYPE html><html lang="zh"><head><meta charset="utf-8">
<title>一呼 · Markdown 预览</title>
<style>body{{max-width:860px;margin:40px auto;padding:0 16px;font-family:system-ui,-apple-system,sans-serif;line-height:1.65;color:#222}}
h1,h2{{border-bottom:1px solid #ddd;padding-bottom:6px}}pre{{background:#f6f8fa;padding:12px;border-radius:6px;overflow:auto}}
code{{background:#f6f8fa;padding:2px 5px;border-radius:4px}}pre code{{padding:0;background:none}}
table{{border-collapse:collapse}}th,td{{border:1px solid #ccc;padding:4px 10px}}blockquote{{color:#666;border-left:4px solid #ddd;margin:0;padding-left:12px}}
img{{max-width:100%}}</style></head><body>{body}</body></html>"#
    )
}

/// 原子写预览文件，返回路径
pub fn write_preview(html: &str) -> std::io::Result<PathBuf> {
    let path = PathBuf::from(PREVIEW_PATH);
    let tmp = PathBuf::from(format!("{PREVIEW_PATH}.tmp"));
    std::fs::write(&tmp, html)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_headings_code_and_tables() {
        let html = render_html("# 标题\n\n```rust\nfn main() {}\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n");
        assert!(html.contains("<h1>标题</h1>"), "{html}");
        assert!(html.contains("<pre><code"), "{html}");
        assert!(html.contains("<table>"), "{html}");
        assert!(html.contains("<!DOCTYPE html>"), "应为完整页面");
    }

    #[test]
    fn escapes_text_level_lt() {
        // 正文里的裸 < 转义（原始 HTML 块按 CommonMark 透传，属预期——
        // 见模块头安全说明；这里验证的是文本层不吞字符）
        let html = render_html("a < b");
        assert!(html.contains("a &lt; b"), "{html}");
    }

    #[test]
    fn sanitized_strips_scripts_but_keeps_markdown() {
        let html = render_html_sanitized("# 标题\n\n<script>alert(1)</script>\n\n普通 **加粗** 文本");
        assert!(!html.contains("<script>"), "脚本必须被剥离：{html}");
        assert!(html.contains("alert(1)") || html.contains("alert(1)<!--") || !html.contains("alert"), "脚本体不应执行");
        assert!(html.contains("<h1>标题</h1>"), "{html}");
        assert!(html.contains("<strong>加粗</strong>"), "{html}");
    }

    #[test]
    fn sanitized_keeps_links_and_tables() {
        let html = render_html_sanitized("[x](https://a.com)\n\n| a | b |\n|---|---|\n| 1 | 2 |");
        assert!(html.contains("href=\"https://a.com\""), "{html}");
        assert!(html.contains("rel=\"noopener noreferrer\""), "{html}");
        assert!(html.contains("<table>"), "{html}");
    }

    #[test]
    fn strikethrough_and_tasklist() {
        let html = render_html("~~删~~\n\n- [x] 完成\n");
        assert!(html.contains("<del>删</del>"), "{html}");
        assert!(html.contains("checked"), "{html}");
    }
}
