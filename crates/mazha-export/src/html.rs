//! Markdown 源文本 → HTML（SPEC §8、§9）。
//!
//! 渲染直接基于 `mazha-md` 的 [`Block`]/[`SyntaxSpan`] 模型：块级结构决定标签嵌套，
//! 行内 [`SpanRole`] 决定 `<em>/<strong>/<del>/<code>/<a>` 等。语法标记（Marker）在
//! HTML 里**不输出**，等价于 WYSIWYG 的"隐藏标记"；链接目标（[`SpanRole::LinkDest`]）
//! 虽在编辑器里隐藏，但导出时用于 `<a href>`。
//!
//! MVP 取舍：列表一律紧凑渲染（列表项内的段落不加 `<p>`）；引用/列表可嵌套；
//! 代码块用 `<pre><code class="language-x">`；图片解析 `![alt](dest)` 输出 `<img>`。

use mazha_md::{Block, BlockKind, SpanKind, SpanRole, SyntaxSpan, parse};
use std::fmt::Write as _;
use std::ops::Range;

/// 把 Markdown 源文本渲染为 HTML 片段（不含 `<html>` 外壳）。
pub fn to_html(source: &str) -> String {
    let blocks = parse(source);
    let mut out = String::new();
    render_blocks(source, &blocks, &mut out, false);
    out
}

/// 默认内联 CSS（极简排版）。
pub const DEFAULT_CSS: &str = r#"body{max-width:42em;margin:2em auto;padding:0 1em;
font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Helvetica,Arial,"PingFang SC","Hiragino Sans GB","Microsoft YaHei",sans-serif;
line-height:1.6;color:#24292f}
pre{background:#f6f8fa;padding:1em;overflow:auto}
code,pre{font-family:ui-monospace,SFMono-Regular,Consolas,"Liberation Mono",monospace;font-size:.9em}
code{background:#f6f8fa;padding:.2em .4em;border-radius:4px}
pre code{background:none;padding:0}
blockquote{margin:1em 0;padding:.5em 1em;color:#57606a;border-left:.25em solid #d0d7de;background:#f6f8fa}
img{max-width:100%}
table{border-collapse:collapse}
th,td{border:1px solid #d0d7de;padding:.4em .8em}
a{color:#0969da;text-decoration:none}
a:hover{text-decoration:underline}
"#;

/// 把 Markdown 源文本渲染为完整 HTML5 文档（含 `<html>/<head>/<body>` 外壳）。
///
/// `title` 用于 `<title>` 标签（经 HTML 转义）；`css` 为 `None` 时使用
/// [`DEFAULT_CSS`]，传 `Some("")` 可完全省略 `<style>`。
pub fn to_html_document(source: &str, title: &str, css: Option<&str>) -> String {
    let body = to_html(source);
    let css = css.unwrap_or(DEFAULT_CSS);
    let mut out = String::new();
    let _ = write!(
        out,
        "<!DOCTYPE html>\n<html lang=\"zh-CN\">\n<head>\n\
         <meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n\
         <title>{title}</title>",
        title = escape_attr(title)
    );
    if !css.is_empty() {
        let _ = write!(out, "\n<style>\n{css}\n</style>");
    }
    let _ = write!(out, "\n</head>\n<body>\n{body}\n</body>\n</html>");
    out
}

/// 渲染一组块；`tight` 为真时不给段落包 `<p>`（用于列表项）。
fn render_blocks(source: &str, blocks: &[Block], out: &mut String, tight: bool) {
    for b in blocks {
        render_block(source, b, out, tight);
    }
}

/// 渲染单个块。
/// 渲染表格：从块片段按**行/单元格**重组 `<table>/<thead>/<tbody>`。
///
/// 片段模型里竖线（[`SpanRole::TablePipe`]）是可见文本，单元格内容是普通行内片段；
/// 分隔行（[`SpanRole::TableDelim`]）整段隐藏。第一行是表头。
fn render_table(source: &str, block: &Block, align: &[mazha_md::Align], out: &mut String) {
    let rows = table_rows(source, block);
    if rows.is_empty() {
        return;
    }
    out.push_str("<table>");
    for (ri, cells) in rows.iter().enumerate() {
        if ri == 0 {
            out.push_str("<thead>");
        }
        out.push_str("<tr>");
        for (ci, cell) in cells.iter().enumerate() {
            let tag = if ri == 0 { "th" } else { "td" };
            let _ = write!(out, "<{tag}");
            match align.get(ci) {
                Some(mazha_md::Align::Center) => out.push_str(" style=\"text-align:center\""),
                Some(mazha_md::Align::Right) => out.push_str(" style=\"text-align:right\""),
                _ => {}
            }
            out.push('>');
            // 单元格内已按行内语法解析过，直接渲染其片段
            render_spans_html(source, cell, out);
            let _ = write!(out, "</{tag}>");
        }
        out.push_str("</tr>");
        if ri == 0 {
            out.push_str("</thead>");
            // 没有数据行时不开 <tbody>，避免空标签
            if rows.len() > 1 {
                out.push_str("<tbody>");
            }
        }
    }
    if rows.len() > 1 {
        out.push_str("</tbody>");
    }
    out.push_str("</table>");
}

/// 把一组行内片段渲染为 HTML（不做块级包装）。
fn render_spans_html(source: &str, spans: &[SyntaxSpan], out: &mut String) {
    // 复用行内渲染：包一层空 out，再把结果并入
    let mut inner = String::new();
    render_inline(source, spans, &mut inner);
    out.push_str(&inner);
}

/// 把表格块的片段按行切成"单元格片段组"（首行为表头）。
///
/// 分组依据**源文本的行边界**（而非片段顺序），因此不受片段是否严格排序影响。
fn table_rows(source: &str, block: &Block) -> Vec<Vec<Vec<SyntaxSpan>>> {
    let bytes = source.as_bytes();
    let end = block.range.end.min(bytes.len());
    // 1. 逐行求 [start, end)
    let mut bounds: Vec<Range<usize>> = Vec::new();
    let mut start = block.range.start;
    while start < end {
        let e = bytes[start..end]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(end, |i| start + i);
        bounds.push(start..e);
        start = e + 1;
    }
    // 2. 每行按竖线切单元格（分隔行整行跳过 → 得到空行组，需过滤）
    bounds
        .iter()
        .map(|line| cells_of_line(&block.spans, line))
        .filter(|cells| !cells.is_empty())
        .collect()
}

/// 取落在行 `line` 内的片段，按竖线切成若干单元格（去首尾空单元格）。
fn cells_of_line(spans: &[SyntaxSpan], line: &Range<usize>) -> Vec<Vec<SyntaxSpan>> {
    let mut cells: Vec<Vec<SyntaxSpan>> = vec![Vec::new()];
    for sp in spans {
        if sp.role == SpanRole::TableDelim {
            continue; // 分隔行不渲染
        }
        // 只取起点落在本行内的片段（空片段忽略）
        if sp.is_empty() || sp.range.start < line.start || sp.range.start >= line.end {
            continue;
        }
        if sp.role == SpanRole::TablePipe {
            cells.push(Vec::new());
        } else {
            cells.last_mut().expect("至少一个单元格").push(sp.clone());
        }
    }
    if cells.first().is_some_and(Vec::is_empty) {
        cells.remove(0);
    }
    if cells.last().is_some_and(Vec::is_empty) {
        cells.pop();
    }
    cells
}

fn render_block(source: &str, block: &Block, out: &mut String, tight: bool) {
    match &block.kind {
        BlockKind::Paragraph => {
            if tight {
                render_inline(source, &block.spans, out);
            } else {
                out.push_str("<p>");
                render_inline(source, &block.spans, out);
                out.push_str("</p>");
            }
        }
        BlockKind::Heading { level } => {
            let _ = write!(out, "<h{level}>");
            render_inline(source, &block.spans, out);
            let _ = write!(out, "</h{level}>");
        }
        BlockKind::ThematicBreak => out.push_str("<hr>"),
        BlockKind::Table { align } => render_table(source, block, align, out),
        BlockKind::CodeBlock { lang, .. } => {
            out.push_str("<pre><code");
            if let Some(lang) = lang {
                let _ = write!(out, " class=\"language-{}\"", escape_attr(lang));
            }
            out.push('>');
            // 汇总所有 CodeContent 片段（围栏标记不输出）
            for sp in &block.spans {
                if sp.role == SpanRole::CodeContent {
                    out.push_str(&escape_text(&source[sp.range.clone()]));
                }
            }
            out.push_str("</code></pre>");
        }
        BlockKind::BlockQuote => {
            out.push_str("<blockquote>");
            render_blocks(source, &block.children, out, false);
            out.push_str("</blockquote>");
        }
        BlockKind::Callout { kind, title } => {
            // 提示块：GitHub 风格 <div class="callout callout-note">
            let type_name = kind.as_str().to_ascii_lowercase();
            let _ = write!(out, "<div class=\"callout callout-{type_name}\">");
            // 标题：自定义优先，否则用类型默认名
            let heading = title.as_deref().unwrap_or_else(|| kind.as_str());
            let _ = write!(
                out,
                "<div class=\"callout-title\">{}</div>",
                escape_text(heading)
            );
            out.push_str("<div class=\"callout-content\">");
            render_blocks(source, &block.children, out, false);
            out.push_str("</div></div>");
        }
        BlockKind::List { ordered, start } => {
            if *ordered {
                if *start == 1 {
                    out.push_str("<ol>");
                } else {
                    let _ = write!(out, "<ol start=\"{start}\">");
                }
            } else {
                out.push_str("<ul>");
            }
            for item in &block.children {
                render_list_item(source, item, out);
            }
            out.push_str(if *ordered { "</ol>" } else { "</ul>" });
        }
        // 顶层不应出现列表项；兜底按项渲染
        BlockKind::ListItem { task } => render_item_body(source, block, *task, out),
    }
}

/// 渲染列表项：`<li>` + 可选复选框 + 子块（紧凑）。
fn render_list_item(source: &str, item: &Block, out: &mut String) {
    let task = match item.kind {
        BlockKind::ListItem { task } => task,
        _ => None,
    };
    render_item_body(source, item, task, out);
}

/// 列表项主体渲染。
fn render_item_body(source: &str, item: &Block, task: Option<bool>, out: &mut String) {
    out.push_str("<li>");
    if let Some(done) = task {
        if done {
            out.push_str("<input type=\"checkbox\" disabled checked> ");
        } else {
            out.push_str("<input type=\"checkbox\" disabled> ");
        }
    }
    render_blocks(source, &item.children, out, true);
    out.push_str("</li>");
}

/// 渲染行内片段序列。
fn render_inline(source: &str, spans: &[SyntaxSpan], out: &mut String) {
    let mut state = InlineState::default();
    let mut i = 0usize;
    while i < spans.len() {
        let sp = &spans[i];
        // 可见文本一律直接转义输出（样式由成对 Marker 开合）
        if sp.kind == SpanKind::Text {
            out.push_str(&escape_text(&source[sp.range.clone()]));
            i += 1;
            continue;
        }
        // Widget（图片占位）
        if sp.kind == SpanKind::Widget {
            if sp.role == SpanRole::ImageMarker
                && let Some((alt, dest)) = parse_image(&source[sp.range.clone()])
            {
                let _ = write!(
                    out,
                    "<img src=\"{}\" alt=\"{}\">",
                    escape_attr(&dest),
                    escape_attr(&alt)
                );
            }
            i += 1;
            continue;
        }
        // 语法标记：按角色决定开合标签 / 链接 / 隐藏
        match sp.role {
            SpanRole::Emphasis => state.toggle(out, "em"),
            SpanRole::Strong => state.toggle(out, "strong"),
            SpanRole::Strikethrough => state.toggle(out, "del"),
            SpanRole::InlineCode => state.toggle(out, "code"),
            SpanRole::LinkMarker => match source.as_bytes().get(sp.range.start) {
                Some(b'[') => {
                    // 普通链接：向前找到隐藏的目标 URL
                    let href = spans[i..]
                        .iter()
                        .find(|s| s.role == SpanRole::LinkDest)
                        .map(|s| source[s.range.clone()].to_string())
                        .unwrap_or_default();
                    let _ = write!(out, "<a href=\"{}\">", escape_attr(&href));
                }
                Some(b')') => out.push_str("</a>"),
                Some(b'<') => {
                    // 自动链接：`<` 文本 `>`，目标即文本本身
                    let text = spans
                        .get(i + 1)
                        .map(|s| source[s.range.clone()].to_string())
                        .unwrap_or_default();
                    let _ = write!(
                        out,
                        "<a href=\"{}\">{}</a>",
                        escape_attr(&text),
                        escape_text(&text)
                    );
                    i += 3; // 跳过 `<`、文本、`>`
                    continue;
                }
                // `](` 中间标记、`>` 收尾：忽略
                _ => {}
            },
            SpanRole::SoftBreak => out.push('\n'),
            SpanRole::HardBreak => out.push_str("<br>"),
            // 其余标记（标题/引用/列表/围栏/转义/链接目标等）导出时隐藏
            _ => {}
        }
        i += 1;
    }
}

/// 行内样式开关状态（成对标记的第一个为开、第二个为闭）。
#[allow(clippy::struct_excessive_bools)] // 四个独立样式开关，语义清晰
#[derive(Default)]
struct InlineState {
    em: bool,
    strong: bool,
    strike: bool,
    code: bool,
}

impl InlineState {
    /// 按标记出现顺序在"开/闭"间切换，写入对应标签。
    fn toggle(&mut self, out: &mut String, name: &str) {
        let flag = match name {
            "em" => &mut self.em,
            "strong" => &mut self.strong,
            "del" => &mut self.strike,
            _ => &mut self.code,
        };
        if *flag {
            out.push_str("</");
            out.push_str(name);
            out.push('>');
        } else {
            out.push('<');
            out.push_str(name);
            out.push('>');
        }
        *flag = !*flag;
    }
}

/// 解析图片源码 `![alt](dest)`（MVP：不处理 alt 内的 `]`）。
fn parse_image(src: &str) -> Option<(String, String)> {
    let s = src.strip_prefix("![")?;
    let close = s.find("](")?;
    let alt = &s[..close];
    let dest = s[close + 2..].strip_suffix(')')?;
    Some((alt.to_string(), dest.to_string()))
}

/// 转义文本节点中的 `& < >`。
fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// 转义属性值（在文本转义基础上再加 `"`）。
fn escape_attr(s: &str) -> String {
    escape_text(s).replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paragraph_and_heading() {
        assert_eq!(to_html("hello\n"), "<p>hello</p>");
        assert_eq!(to_html("# 标题\n"), "<h1>标题</h1>");
        assert_eq!(to_html("### 三级\n"), "<h3>三级</h3>");
    }

    #[test]
    fn emphasis_strong_strike_code() {
        assert_eq!(to_html("**粗**体\n"), "<p><strong>粗</strong>体</p>");
        assert_eq!(to_html("*斜*\n"), "<p><em>斜</em></p>");
        assert_eq!(to_html("~~删~~\n"), "<p><del>删</del></p>");
        assert_eq!(to_html("`x*y`\n"), "<p><code>x*y</code></p>");
    }

    #[test]
    fn nested_strong_emphasis() {
        assert_eq!(
            to_html("**a *b* c**\n"),
            "<p><strong>a <em>b</em> c</strong></p>"
        );
    }

    #[test]
    fn link_hides_markers_keeps_href() {
        assert_eq!(
            to_html("[文字](https://x.com)\n"),
            "<p><a href=\"https://x.com\">文字</a></p>"
        );
    }

    #[test]
    fn autolink() {
        assert_eq!(
            to_html("<https://x.com>\n"),
            "<p><a href=\"https://x.com\">https://x.com</a></p>"
        );
    }

    #[test]
    fn image_widget() {
        assert_eq!(
            to_html("![图](a.png)\n"),
            "<p><img src=\"a.png\" alt=\"图\"></p>"
        );
    }

    #[test]
    fn thematic_break_and_code_block() {
        assert_eq!(to_html("---\n"), "<hr>");
        assert_eq!(
            to_html("```rust\nfn f() {}\n```\n"),
            "<pre><code class=\"language-rust\">fn f() {}</code></pre>"
        );
    }

    #[test]
    fn blockquote_and_list() {
        assert_eq!(to_html("> 引用\n"), "<blockquote><p>引用</p></blockquote>");
        assert_eq!(to_html("- a\n- b\n"), "<ul><li>a</li><li>b</li></ul>");
        assert_eq!(to_html("1. a\n2. b\n"), "<ol><li>a</li><li>b</li></ol>");
    }

    #[test]
    fn task_list() {
        assert_eq!(
            to_html("- [x] done\n- [ ] todo\n"),
            "<ul><li><input type=\"checkbox\" disabled checked> done</li>\
             <li><input type=\"checkbox\" disabled> todo</li></ul>"
        );
    }

    #[test]
    fn escapes_html() {
        assert_eq!(to_html("a < b & c\n"), "<p>a &lt; b &amp; c</p>");
    }

    #[test]
    fn html_document_has_shell() {
        let doc = to_html_document("# 标题\n", "测试", None);
        assert!(doc.starts_with("<!DOCTYPE html>"));
        assert!(doc.contains("<html lang=\"zh-CN\">"));
        assert!(doc.contains("<meta charset=\"utf-8\">"));
        assert!(doc.contains("<title>测试</title>"));
        assert!(doc.contains("<style>"));
        assert!(doc.contains("<h1>标题</h1>"));
        assert!(doc.ends_with("</html>"));
    }

    #[test]
    fn html_document_escapes_title() {
        let doc = to_html_document("x\n", "<b>&", None);
        assert!(doc.contains("<title>&lt;b&gt;&amp;</title>"));
    }

    #[test]
    fn html_document_empty_css_omits_style() {
        let doc = to_html_document("x\n", "t", Some(""));
        assert!(!doc.contains("<style>"));
    }

    #[test]
    fn html_table() {
        let html = to_html("| a | b |\n| --- | --- |\n| 1 | 2 |\n");
        assert_eq!(
            html,
            "<table><thead><tr><th>a</th><th>b</th></tr></thead><tbody>\
             <tr><td>1</td><td>2</td></tr></tbody></table>"
        );
    }

    #[test]
    fn html_table_alignment() {
        let html = to_html("| a | b | c |\n| :- | :-: | -: |\n| 1 | 2 | 3 |\n");
        assert!(html.contains("<th style=\"text-align:center\">b</th>"));
        assert!(html.contains("<th style=\"text-align:right\">c</th>"));
        // 左对齐不加样式
        assert!(html.contains("<th>a</th>"));
    }

    #[test]
    fn html_table_inline_marks_in_cells() {
        let html = to_html("| a |\n| - |\n| **粗** |\n");
        assert!(html.contains("<td><strong>粗</strong></td>"));
    }

    #[test]
    fn html_table_escapes_cells() {
        let html = to_html("| a |\n| - |\n| <b> |\n");
        assert!(html.contains("<td>&lt;b&gt;</td>"));
    }

    #[test]
    fn html_header_only_table() {
        // 只有表头（无数据行）：仍应产出合法 table/thead
        let html = to_html("| a |\n| --- |\n");
        assert_eq!(html, "<table><thead><tr><th>a</th></tr></thead></table>");
    }

    #[test]
    fn html_callout_note() {
        let html = to_html("> [!NOTE]\n> 内容\n");
        assert_eq!(
            html,
            "<div class=\"callout callout-note\">\
             <div class=\"callout-title\">NOTE</div>\
             <div class=\"callout-content\"><p>内容</p></div></div>"
        );
    }

    #[test]
    fn html_callout_custom_title() {
        let html = to_html("> [!TIP] 自定义\n> 内容\n");
        assert!(html.contains("callout-tip"));
        assert!(html.contains("<div class=\"callout-title\">自定义</div>"));
        assert!(html.contains("<p>内容</p>"));
    }

    #[test]
    fn html_callout_custom_type() {
        let html = to_html("> [!BUG]\n> 内容\n");
        assert!(html.contains("callout-bug"));
    }

    #[test]
    fn html_callout_escapes_title() {
        let html = to_html("> [!NOTE] <b>\n> 内容\n");
        assert!(html.contains("<div class=\"callout-title\">&lt;b&gt;</div>"));
    }

    #[test]
    fn html_quote_without_marker_is_blockquote() {
        let html = to_html("> 普通引用\n");
        assert!(html.contains("<blockquote>"));
    }
}
