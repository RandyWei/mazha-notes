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
    render_blocks(source, &blocks, &mut out, false, false);
    out
}

/// 把 Markdown 源文本渲染为 **XHTML** 片段（EPUB 内容文档复用）。
///
/// 与 [`to_html`] 的唯一差别是**空元素自闭合**（`<hr/>`/`<br/>`/`<img .../>`/`<input .../>`）
/// 且布尔属性写成 `disabled="disabled"`，以满足 EPUB 内容文档"必须是良构 XML"的约束。
pub(crate) fn to_xhtml_body(source: &str) -> String {
    let blocks = parse(source);
    let mut out = String::new();
    render_blocks(source, &blocks, &mut out, false, true);
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
.math{font-family:"Latin Modern Math","STIX Two Math",Cambria,Georgia,serif}
div.math{margin:1em 0;padding:.5em 0;text-align:center}
span.math{font-style:italic}
.fnref{font-size:.75em;vertical-align:super;color:#57606a;padding:0 .1em}
div.footnote{font-size:.9em;color:#57606a;border-left:2px solid #d0d7de;padding:.2em 0 .2em .8em;margin:.6em 0}
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

/// 用 [`crate::Theme`] 渲染完整 HTML5 文档（等价 [`to_html_document`]，CSS 取自主题）。
pub fn to_html_themed(source: &str, title: &str, theme: &crate::Theme) -> String {
    to_html_document(source, title, Some(theme.css.as_str()))
}

/// 渲染一组块；`tight` 为真时不给段落包 `<p>`（用于列表项），`xhtml` 为真时空元素自闭合。
fn render_blocks(source: &str, blocks: &[Block], out: &mut String, tight: bool, xhtml: bool) {
    for b in blocks {
        render_block(source, b, out, tight, xhtml);
    }
}

/// 渲染单个块。
/// 渲染表格：从块片段按**行/单元格**重组 `<table>/<thead>/<tbody>`。
///
/// 片段模型里竖线（[`SpanRole::TablePipe`]）是可见文本，单元格内容是普通行内片段；
/// 分隔行（[`SpanRole::TableDelim`]）整段隐藏。第一行是表头。
fn render_table(
    source: &str,
    block: &Block,
    align: &[mazha_md::Align],
    out: &mut String,
    xhtml: bool,
) {
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
            render_spans_html(source, cell, out, xhtml);
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

/// 把一组行内片段渲染为 HTML/XHTML（不做块级包装）。
fn render_spans_html(source: &str, spans: &[SyntaxSpan], out: &mut String, xhtml: bool) {
    // 复用行内渲染：包一层空 out，再把结果并入
    let mut inner = String::new();
    render_inline(source, spans, &mut inner, xhtml);
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

fn render_block(source: &str, block: &Block, out: &mut String, tight: bool, xhtml: bool) {
    match &block.kind {
        BlockKind::Paragraph => {
            if tight {
                render_inline(source, &block.spans, out, xhtml);
            } else {
                out.push_str("<p>");
                render_inline(source, &block.spans, out, xhtml);
                out.push_str("</p>");
            }
        }
        BlockKind::Heading { level } => {
            let _ = write!(out, "<h{level}>");
            render_inline(source, &block.spans, out, xhtml);
            let _ = write!(out, "</h{level}>");
        }
        BlockKind::ThematicBreak => out.push_str(if xhtml { "<hr/>" } else { "<hr>" }),
        BlockKind::FrontMatter { .. } => {
            // front-matter 是元数据，导出时不渲染
        }
        BlockKind::Table { align } => render_table(source, block, align, out, xhtml),
        BlockKind::CodeBlock { lang, .. } => {
            out.push_str("<pre><code");
            if let Some(lang) = lang {
                let _ = write!(out, " class=\"language-{}\"", escape_attr(lang));
            }
            out.push('>');
            // 汇总所有 CodeContent 片段（围栏标记不输出）。
            // 片段按行切分且**不含行尾换行**，故段间补一个 `\n`，
            // 否则多行代码会被拼成一行；空白行片段为空，同样占一个换行。
            let mut first = true;
            for sp in &block.spans {
                if sp.role != SpanRole::CodeContent {
                    continue;
                }
                if !first {
                    out.push('\n');
                }
                first = false;
                out.push_str(&escape_text(&source[sp.range.clone()]));
            }
            out.push_str("</code></pre>");
        }
        BlockKind::MathBlock => {
            // 显示公式块（D34）：`$$` 定界符隐藏，只输出公式体（含行尾可见文字）。
            // `pre` 保留源里的换行与空白，避免公式排版被 HTML 折叠。
            out.push_str("<div class=\"math\">");
            out.push_str(&escape_text(&math_body(source, block)));
            out.push_str("</div>");
        }
        BlockKind::FootnoteDef { label } => {
            // 脚注定义（D35 ④）：就地输出（**不**重排到文末、暂不生成回链）；
            // `[^label]:` 标记隐藏，标签（上标）与正文可见。
            let _ = write!(
                out,
                "<div class=\"footnote\" id=\"fn-{}\">",
                escape_attr(label)
            );
            render_inline(source, &block.spans, out, xhtml);
            out.push_str("</div>");
        }
        BlockKind::BlockQuote => {
            out.push_str("<blockquote>");
            render_blocks(source, &block.children, out, false, xhtml);
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
            render_blocks(source, &block.children, out, false, xhtml);
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
                render_list_item(source, item, out, xhtml);
            }
            out.push_str(if *ordered { "</ol>" } else { "</ul>" });
        }
        // 顶层不应出现列表项；兜底按项渲染
        BlockKind::ListItem { task } => render_item_body(source, block, *task, out, xhtml),
    }
}

/// 渲染列表项：`<li>` + 可选复选框 + 子块（紧凑）。
fn render_list_item(source: &str, item: &Block, out: &mut String, xhtml: bool) {
    let task = match item.kind {
        BlockKind::ListItem { task } => task,
        _ => None,
    };
    render_item_body(source, item, task, out, xhtml);
}

/// 列表项主体渲染。
fn render_item_body(source: &str, item: &Block, task: Option<bool>, out: &mut String, xhtml: bool) {
    out.push_str("<li>");
    if let Some(done) = task {
        // XHTML 里布尔属性必须有值且空元素需自闭合
        if xhtml {
            out.push_str(if done {
                "<input type=\"checkbox\" disabled=\"disabled\" checked=\"checked\"/> "
            } else {
                "<input type=\"checkbox\" disabled=\"disabled\"/> "
            });
        } else if done {
            out.push_str("<input type=\"checkbox\" disabled checked> ");
        } else {
            out.push_str("<input type=\"checkbox\" disabled> ");
        }
    }
    render_blocks(source, &item.children, out, true, xhtml);
    out.push_str("</li>");
}

/// 渲染行内片段序列。
fn render_inline(source: &str, spans: &[SyntaxSpan], out: &mut String, xhtml: bool) {
    let mut state = InlineState::default();
    let mut i = 0usize;
    while i < spans.len() {
        let sp = &spans[i];
        // 标签：包一层 <span class="tag">
        if sp.kind == SpanKind::Text && sp.role == SpanRole::Tag {
            let _ = write!(
                out,
                "<span class=\"tag\">{}</span>",
                escape_text(&source[sp.range.clone()])
            );
            i += 1;
            continue;
        }
        // 行内公式体：包一层 <span class="math">（D34 ④）；`$` 定界符是 Marker，落到下面的兜底忽略
        if sp.kind == SpanKind::Text && sp.role == SpanRole::Math {
            let _ = write!(
                out,
                "<span class=\"math\">{}</span>",
                escape_text(&source[sp.range.clone()])
            );
            i += 1;
            continue;
        }
        // 脚注标签（D35 ④）：引用与定义的标签都用上标；`[^`/`]`/`]:` 是 Marker，落到兜底忽略
        if sp.kind == SpanKind::Text && sp.role == SpanRole::FootnoteRef {
            push_footnote_ref(source, sp, out);
            i += 1;
            continue;
        }
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
                    "<img src=\"{}\" alt=\"{}\"{}>",
                    escape_attr(&dest),
                    escape_attr(&alt),
                    if xhtml { "/" } else { "" }
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
            SpanRole::WikiLinkMarker => match source.as_bytes().get(sp.range.start) {
                Some(b'[') => {
                    // `[[`：向前找隐藏的目标页名作为 href
                    let target = spans[i..]
                        .iter()
                        .find(|s| s.role == SpanRole::WikiLinkTarget)
                        .map(|s| source[s.range.clone()].to_string())
                        .unwrap_or_default();
                    let _ = write!(
                        out,
                        "<a href=\"{}\" class=\"wikilink\">",
                        escape_attr(&target)
                    );
                }
                Some(b']') => out.push_str("</a>"),
                // `|` 忽略
                _ => {}
            },
            SpanRole::SoftBreak => out.push('\n'),
            SpanRole::HardBreak => out.push_str(if xhtml { "<br/>" } else { "<br>" }),
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

/// 输出行内脚注标签（D35 ④）：引用与定义的标签都包 `<sup class="fnref">`，标签经转义。
fn push_footnote_ref(source: &str, sp: &SyntaxSpan, out: &mut String) {
    let _ = write!(
        out,
        "<sup class=\"fnref\">{}</sup>",
        escape_text(&source[sp.range.clone()])
    );
}

/// 汇总公式块的可见文本（D34）：跳过 `$$` 定界标记，仅当片段之间的源文本含换行时补 `\n`。
///
/// 与序列化同规则（见 `mazha_md::serialize`）：定界符与容器前缀（如 `> `）都在片段间隙里，
/// 逐个还原源文本中的换行即可保留公式的多行结构，同时不掺入 `$$` 或 `> `。
fn math_body(source: &str, block: &Block) -> String {
    let mut body = String::new();
    let mut prev_end: Option<usize> = None;
    for sp in &block.spans {
        if sp.kind != SpanKind::Text {
            continue;
        }
        if let Some(prev) = prev_end {
            for c in source[prev..sp.range.start].chars() {
                if c == '\n' {
                    body.push('\n');
                }
            }
        }
        body.push_str(&source[sp.range.clone()]);
        prev_end = Some(sp.range.end);
    }
    body
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
///
/// `pub(crate)` 供 EPUB 内容文档复用：XML 文本节点与属性值都适用
/// （多转义的 `"→&quot;` 在文本中同样合法）。
pub(crate) fn escape_attr(s: &str) -> String {
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
    fn multiline_code_block_keeps_newlines() {
        // CodeContent 片段按行切分且不含行尾换行，导出时必须补回换行
        assert_eq!(
            to_html("```rust\nfn a() {}\nfn b() {}\n```\n"),
            "<pre><code class=\"language-rust\">fn a() {}\nfn b() {}</code></pre>"
        );
        // 代码中的空白行同样保留
        assert_eq!(
            to_html("```\na\n\nb\n```\n"),
            "<pre><code>a\n\nb</code></pre>"
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
    fn html_themed_uses_theme_css() {
        let dark = to_html_themed("x\n", "t", &crate::Theme::dark());
        assert!(dark.contains("#0d1117"), "深色主题背景缺失");
        assert!(!dark.contains("#24292f"), "不应含默认浅色主题色");

        let light = to_html_themed("x\n", "t", &crate::Theme::light());
        assert!(light.contains("#24292f"));
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

    #[test]
    fn html_wikilink_no_alias() {
        let html = to_html("[[Page]]\n");
        assert_eq!(html, "<p><a href=\"Page\" class=\"wikilink\">Page</a></p>");
    }

    #[test]
    fn html_wikilink_with_alias() {
        let html = to_html("[[Page|别名]]\n");
        assert_eq!(html, "<p><a href=\"Page\" class=\"wikilink\">别名</a></p>");
    }

    #[test]
    fn html_wikilink_escapes_target() {
        // 目标含 `"`：href 中转义为 &quot;（两个引号都在 [[ ]] 之间）
        let html = to_html("[[Page\"x\"]]\n");
        assert!(html.contains("href=\"Page&quot;x&quot;\""));
        assert!(html.contains("class=\"wikilink\""));
    }

    #[test]
    fn html_frontmatter_hidden() {
        // front-matter 是元数据，导出时不渲染
        let html = to_html("---\ntitle: x\n---\n正文\n");
        assert_eq!(html, "<p>正文</p>");
    }

    #[test]
    fn html_tag() {
        let html = to_html("#标签\n");
        assert_eq!(html, "<p><span class=\"tag\">#标签</span></p>");
    }

    #[test]
    fn html_tag_nested() {
        let html = to_html("#nested/tag\n");
        assert!(html.contains("<span class=\"tag\">#nested/tag</span>"));
    }

    // ---------- 公式（D34） ----------

    #[test]
    fn html_block_math_hides_delimiters() {
        assert_eq!(to_html("$$E=mc^2$$\n"), "<div class=\"math\">E=mc^2</div>");
        // 多行：公式体行间保留换行（定界行不输出）
        assert_eq!(
            to_html("$$\na + b\n$$\n"),
            "<div class=\"math\">a + b</div>"
        );
    }

    #[test]
    fn html_block_math_keeps_trailing_text() {
        // `$$a$$ 后续` 的可见尾巴也要输出（片段间隙无换行则不插换行）
        assert_eq!(to_html("$$a$$ 后续\n"), "<div class=\"math\">a 后续</div>");
    }

    #[test]
    fn html_inline_math_wrapped_not_escaped_as_math() {
        let html = to_html("当 $x^2$ 时\n");
        assert_eq!(html, "<p>当 <span class=\"math\">x^2</span> 时</p>");
        // `$` 定界符隐藏，不残留
        assert!(!html.contains('$'));
    }

    #[test]
    fn html_math_inside_quote_drops_prefix() {
        // 引用内的公式：`> ` 前缀留在片段间隙，不得混入公式体
        let html = to_html("> $$\n> x\n> $$\n");
        assert!(
            html.contains("<blockquote><div class=\"math\">x</div></blockquote>"),
            "{html}"
        );
    }

    // ---------- 脚注（D35） ----------

    #[test]
    fn html_footnote_ref_is_sup() {
        // 引用：`[^`/`]` 隐藏，标签包 `<sup class="fnref">`
        assert_eq!(
            to_html("见[^1]处\n"),
            "<p>见<sup class=\"fnref\">1</sup>处</p>"
        );
    }

    #[test]
    fn html_footnote_def_in_place() {
        // 定义：就地输出，`[^label]:` 隐藏，标签上标、正文可见（不重排文末、无回链）
        let html = to_html("[^1]: note\n");
        assert_eq!(
            html,
            "<div class=\"footnote\" id=\"fn-1\"><sup class=\"fnref\">1</sup> note</div>"
        );
    }

    #[test]
    fn html_footnote_def_label_attr_escaped() {
        // id 属性里的特殊字符要转义，避免属性被截断（标签串不含空白/方括号）
        let html = to_html("[^a&b]: x\n");
        assert!(html.contains("id=\"fn-a&amp;b\""), "{html}");
    }

    #[test]
    fn html_footnote_default_css_has_rules() {
        let doc = to_html_document("见[^1]处\n\n[^1]: note\n", "t", None);
        assert!(doc.contains(".fnref"), "{doc}");
        assert!(doc.contains("div.footnote"), "{doc}");
    }
}
