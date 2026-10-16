//! Markdown 源文本 → DOCX（OOXML，基于 `docx-rs`；SPEC §8、§9）。
//!
//! 与 `html` / `typst` 两个导出器一致，渲染直接基于 `mazha-md` 的
//! `Block` / `SyntaxSpan` 模型：块级结构决定段落、表格、列表，行内 `SpanRole`
//! 决定粗体、斜体、删除线、代码与链接样式。语法标记（Marker）在 DOCX 里**不输出**，
//! 等价于 WYSIWYG 的"隐藏标记"；链接目标（`SpanRole::LinkDest`）虽在编辑器里隐藏，
//! 但导出时用于在链接文字后附上 URL。
//!
//! MVP 取舍：
//! - 标题用**直接格式**（加粗 + 字号）而非段落样式，避免依赖未定义的样式表。
//! - 链接渲染为蓝色下划线文字，并在其后以 ` (URL)` 附上目标——`docx-rs` 的
//!   `Relationship` 不支持 `TargetMode="External"`，无法生成可点击外链。
//! - 图片仅输出占位文本 `[图片: alt]`（不内嵌二进制，Phase 3 再补）。
//! - front-matter 作为元数据不渲染；嵌套列表用空格缩进（无段落缩进 API）。

use docx::Docx;
use docx::document::{
    BodyContent, Break, BreakType, Paragraph, ParagraphContent, Run, RunContent, Table, TableCell,
    TableRow, Text, TextSpace,
};
use docx::formatting::{
    BorderStyle, Borders, BottomBorder, CharacterProperty, JustificationVal, LeftBorder,
    ParagraphProperty, TableBorders, TopBorder, Underline,
};
use mazha_md::{Align, Block, BlockKind, SpanKind, SpanRole, SyntaxSpan, parse};
use std::io::Cursor;
use std::ops::Range;

/// DOCX 导出错误类型。
#[derive(Debug, thiserror::Error)]
pub enum DocxError {
    /// 打包 OOXML（写入 ZIP）失败。
    #[error("docx 打包失败: {0}")]
    Pack(String),
}

/// 把 Markdown 源文本导出为 DOCX 字节。
///
/// # Errors
/// 当 `docx-rs` 写入 ZIP 包失败时返回 [`DocxError::Pack`]。
pub fn to_docx(markdown: &str) -> Result<Vec<u8>, DocxError> {
    let blocks = parse(markdown);
    let mut docx: Docx<'static> = Docx::default();
    let mut body: Vec<BodyContent<'static>> = Vec::new();
    render_blocks(markdown, &blocks, 0, &mut body);
    docx.document.body.content = body;
    let cursor = docx
        .write(Cursor::new(Vec::new()))
        .map_err(|e| DocxError::Pack(format!("{e:?}")))?;
    Ok(cursor.into_inner())
}

/// 段落默认字体间距基准样式：标题用它携带"加粗 + 字号"的直接格式。
#[derive(Debug, Default, Clone, Copy)]
struct BaseStyle {
    /// 是否强制加粗（标题为真）。
    bold: bool,
    /// 字号（半磅；`None` 用文档默认）。
    size: Option<usize>,
}

/// 行内样式的开合状态（由成对 Marker 切换）。
///
/// `bold/italic/strike/code` 分别对应强调标记；`link` 表示当前处于链接文字内，
/// `link_url` 缓存最近一次链接目标，`autolink` 区分自动链接（文字即目标，无需再附 URL）。
#[allow(clippy::struct_excessive_bools)] // 多个独立样式开关，语义清晰
#[derive(Debug, Default)]
struct InlineStyle {
    bold: bool,
    italic: bool,
    strike: bool,
    code: bool,
    link: bool,
    autolink: bool,
    link_url: String,
}

/// 渲染一组块，把结果追加到 `out`。
///
/// `depth` 为列表嵌套层级，用于生成缩进前缀。
fn render_blocks(
    source: &str,
    blocks: &[Block],
    depth: usize,
    out: &mut Vec<BodyContent<'static>>,
) {
    for b in blocks {
        render_block(source, b, depth, out);
    }
}

/// 渲染单个块。
fn render_block(source: &str, block: &Block, depth: usize, out: &mut Vec<BodyContent<'static>>) {
    match &block.kind {
        BlockKind::Paragraph => {
            out.push(text_paragraph(source, &block.spans, &BaseStyle::default()));
        }
        BlockKind::Heading { level } => out.push(heading_paragraph(source, &block.spans, *level)),
        BlockKind::ThematicBreak => out.push(thematic_break_paragraph()),
        // front-matter 是元数据，导出时不渲染
        BlockKind::FrontMatter { .. } => {}
        BlockKind::CodeBlock { .. } => out.push(code_block_paragraph(source, block)),
        BlockKind::BlockQuote => render_block_quote(source, block, depth, out),
        BlockKind::Callout { kind, title } => {
            render_callout(source, block, kind.as_str(), title.as_deref(), depth, out);
        }
        BlockKind::List { ordered, start } => {
            render_list(source, block, *ordered, *start, depth, out);
        }
        // 顶层不应出现列表项；兜底按项渲染（无序号前缀）
        BlockKind::ListItem { task } => render_list_item(source, block, *task, depth, out, ""),
        BlockKind::Table { align } => out.push(table(source, block, align)),
    }
}

/// 构建一段普通段落（可指定基准样式）。
fn text_paragraph(source: &str, spans: &[SyntaxSpan], base: &BaseStyle) -> BodyContent<'static> {
    BodyContent::Paragraph(Paragraph {
        property: ParagraphProperty::default(),
        content: inline_content(source, spans, base),
    })
}

/// 构建标题段落：按级别取字号并加粗（直接格式）。
fn heading_paragraph(source: &str, spans: &[SyntaxSpan], level: u8) -> BodyContent<'static> {
    // Word 字号单位是半磅：H1=18pt、H2=16pt、H3=14pt…
    let size = match level {
        1 => 36,
        2 => 32,
        3 => 28,
        4 => 24,
        5 => 22,
        _ => 20,
    };
    let base = BaseStyle {
        bold: true,
        size: Some(size),
    };
    text_paragraph(source, spans, &base)
}

/// 分割线：用段落的下边框画一条水平线（比输出字符线更接近原生效果）。
fn thematic_break_paragraph() -> BodyContent<'static> {
    let border = Borders::default().bottom(
        BottomBorder::default()
            .style(BorderStyle::Single)
            .size(6usize)
            .color("999999"),
    );
    BodyContent::Paragraph(Paragraph {
        property: ParagraphProperty::default().border(border),
        content: Vec::new(),
    })
}

/// 代码块：每个 `CodeContent` 片段即源码一行，行间插入软换行以保留原始换行。
fn code_block_paragraph(source: &str, block: &Block) -> BodyContent<'static> {
    let mut content: Vec<ParagraphContent<'static>> = Vec::new();
    let mut first = true;
    for sp in &block.spans {
        if sp.role != SpanRole::CodeContent {
            continue;
        }
        if !first {
            content.push(line_break());
        }
        first = false;
        let line = &source[sp.range.clone()];
        if !line.is_empty() {
            content.push(text_run(line, code_property()));
        }
    }
    BodyContent::Paragraph(Paragraph {
        property: ParagraphProperty::default(),
        content,
    })
}

/// 引用块：渲染子块后，为其中段落追加左侧竖线边框。
fn render_block_quote(
    source: &str,
    block: &Block,
    depth: usize,
    out: &mut Vec<BodyContent<'static>>,
) {
    let mut inner = Vec::new();
    render_blocks(source, &block.children, depth, &mut inner);
    for b in &mut inner {
        if let BodyContent::Paragraph(p) = b {
            p.property.border = Some(
                Borders::default().left(
                    LeftBorder::default()
                        .style(BorderStyle::Single)
                        .size(12usize)
                        .space(8usize)
                        .color("CCCCCC"),
                ),
            );
        }
    }
    out.extend(inner);
}

/// 提示块：加粗标题段落 + 子内容。
fn render_callout(
    source: &str,
    block: &Block,
    kind: &str,
    title: Option<&str>,
    depth: usize,
    out: &mut Vec<BodyContent<'static>>,
) {
    let heading = title.unwrap_or(kind);
    let base = BaseStyle {
        bold: true,
        size: None,
    };
    let title_content = vec![text_run(
        heading,
        run_property(&base, &InlineStyle::default(), false),
    )];
    out.push(BodyContent::Paragraph(Paragraph {
        property: ParagraphProperty::default(),
        content: title_content,
    }));
    render_blocks(source, &block.children, depth, out);
}

/// 列表：为每个列表项生成 `序号/项目符号` 前缀。
fn render_list(
    source: &str,
    list: &Block,
    ordered: bool,
    start: u64,
    depth: usize,
    out: &mut Vec<BodyContent<'static>>,
) {
    for (n, item) in (start..).zip(list.children.iter()) {
        let task = match item.kind {
            BlockKind::ListItem { task } => task,
            _ => None,
        };
        let marker = if ordered {
            format!("{n}. ")
        } else {
            "• ".to_string()
        };
        render_list_item(source, item, task, depth, out, &marker);
    }
}

/// 单个列表项：缩进 + 任务框 + 序号前缀，拼到首个段落之前。
fn render_list_item(
    source: &str,
    item: &Block,
    task: Option<bool>,
    depth: usize,
    out: &mut Vec<BodyContent<'static>>,
    marker: &str,
) {
    let mut prefix = "  ".repeat(depth);
    prefix.push_str(marker);
    match task {
        Some(true) => prefix.push_str("☑ "),
        Some(false) => prefix.push_str("☐ "),
        None => {}
    }
    let mut inner = Vec::new();
    render_blocks(source, &item.children, depth + 1, &mut inner);
    if inner.is_empty() {
        out.push(plain_paragraph(&prefix));
        return;
    }
    prepend_prefix(&mut inner, &prefix);
    out.extend(inner);
}

/// 在首个段落开头插入一个文本 run（用于列表前缀）；无段落则忽略。
fn prepend_prefix(blocks: &mut [BodyContent<'static>], prefix: &str) {
    for b in blocks.iter_mut() {
        if let BodyContent::Paragraph(p) = b {
            p.content
                .insert(0, text_run(prefix, CharacterProperty::default()));
            return;
        }
    }
}

/// 仅含一段文本的简单段落。
fn plain_paragraph(text: &str) -> BodyContent<'static> {
    BodyContent::Paragraph(Paragraph {
        property: ParagraphProperty::default(),
        content: vec![text_run(text, CharacterProperty::default())],
    })
}

/// GFM 表格：首行加粗作表头，按列对齐，上下加边框。
fn table(source: &str, block: &Block, align: &[Align]) -> BodyContent<'static> {
    let rows = table_rows(source, block);
    let mut tbl = Table::default().property(
        docx::formatting::TableProperty::default().borders(
            TableBorders::default()
                .top(TopBorder::default().style(BorderStyle::Single).size(4usize))
                .bottom(
                    BottomBorder::default()
                        .style(BorderStyle::Single)
                        .size(4usize),
                ),
        ),
    );
    for (ri, row) in rows.iter().enumerate() {
        let mut tr = TableRow::default();
        for (ci, cell) in row.iter().enumerate() {
            let base = BaseStyle {
                bold: ri == 0,
                size: None,
            };
            let mut prop = ParagraphProperty::default();
            if let Some(a) = align.get(ci) {
                prop = prop.justification(align_val(*a));
            }
            let para = Paragraph {
                property: prop,
                content: inline_content(source, cell, &base),
            };
            tr = tr.push_cell(TableCell::pargraph(para));
        }
        tbl = tbl.push_row(tr);
    }
    BodyContent::Table(tbl)
}

/// 对齐方式 → Word 段落对齐。
fn align_val(a: Align) -> JustificationVal {
    match a {
        Align::Left => JustificationVal::Left,
        Align::Center => JustificationVal::Center,
        Align::Right => JustificationVal::Right,
    }
}

/// 把表格块的片段按**源文本行边界**切成"行的单元格片段组"（首行为表头）。
///
/// 与 `html` 导出器同构：分组依据源文本行边界而非片段顺序，因此不受片段是否
/// 严格排序影响；分隔行（`TableDelim`）整行跳过。
fn table_rows(source: &str, block: &Block) -> Vec<Vec<Vec<SyntaxSpan>>> {
    let bytes = source.as_bytes();
    let end = block.range.end.min(bytes.len());
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
        // 只取起点落在本行内的非空片段
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

/// 把一组行内片段渲染为段落内容序列。
///
/// 采用与 `html` 导出器一致的"成对标记开合"状态机：遇到 Marker 按角色切换样式，
/// 遇到可见文本即按当前样式产出一个 run。
fn inline_content(
    source: &str,
    spans: &[SyntaxSpan],
    base: &BaseStyle,
) -> Vec<ParagraphContent<'static>> {
    let mut out: Vec<ParagraphContent<'static>> = Vec::new();
    let mut style = InlineStyle::default();
    let mut i = 0usize;
    while i < spans.len() {
        let sp = &spans[i];
        match (sp.kind, sp.role) {
            // 标签：单独着色（须在通用 Text 之前）
            (SpanKind::Text, SpanRole::Tag) => {
                push_text(&mut out, &source[sp.range.clone()], base, &style, true);
            }
            // 可见文本一律按当前样式产出
            (SpanKind::Text, _) => {
                push_text(&mut out, &source[sp.range.clone()], base, &style, false);
            }
            // Widget（图片占位）：输出 `[图片: alt]`
            (SpanKind::Widget, SpanRole::ImageMarker) => {
                let placeholder = image_placeholder(&source[sp.range.clone()]);
                push_text(&mut out, &placeholder, base, &style, false);
            }
            (SpanKind::Marker, SpanRole::Strong) => style.bold = !style.bold,
            (SpanKind::Marker, SpanRole::Emphasis) => style.italic = !style.italic,
            (SpanKind::Marker, SpanRole::Strikethrough) => style.strike = !style.strike,
            (SpanKind::Marker, SpanRole::InlineCode) => style.code = !style.code,
            // 软换行视为空格；硬换行输出一个行内换行
            (SpanKind::Marker, SpanRole::SoftBreak) => {
                push_text(&mut out, " ", base, &style, false);
            }
            (SpanKind::Marker, SpanRole::HardBreak) => out.push(line_break()),
            (SpanKind::Marker, SpanRole::LinkMarker) => {
                apply_link_marker(source, spans, i, &mut style, base, &mut out);
            }
            // 其余标记（标题/引用/列表/围栏/转义/链接目标/维基标记等）导出时隐藏
            _ => {}
        }
        i += 1;
    }
    out
}

/// 处理链接标记：根据起始字节区分"普通链接 / 自动链接 / 关闭"。
fn apply_link_marker(
    source: &str,
    spans: &[SyntaxSpan],
    i: usize,
    style: &mut InlineStyle,
    base: &BaseStyle,
    out: &mut Vec<ParagraphContent<'static>>,
) {
    match source.as_bytes().get(spans[i].range.start) {
        Some(b'[') => {
            style.link = true;
            style.autolink = false;
            style.link_url = find_span_text(spans, i, source, SpanRole::LinkDest);
        }
        Some(b'<') => {
            // 自动链接：文字即目标，关闭时无需再附 URL
            style.link = true;
            style.autolink = true;
            style.link_url = next_text(spans, i, source);
        }
        Some(b')') => {
            style.link = false;
            let url = std::mem::take(&mut style.link_url);
            if !style.autolink && !url.is_empty() {
                push_text(out, &format!(" ({url})"), base, style, false);
            }
        }
        // 自动链接收尾 `>`，或 `](`：仅关闭/忽略
        Some(b'>') => {
            style.link = false;
            style.autolink = false;
        }
        _ => {}
    }
}

/// 从 `i` 起查找指定角色的片段并返回其源文本（找不到返回空串）。
fn find_span_text(spans: &[SyntaxSpan], i: usize, source: &str, role: SpanRole) -> String {
    spans[i..]
        .iter()
        .find(|s| s.role == role)
        .map(|s| source[s.range.clone()].to_string())
        .unwrap_or_default()
}

/// 返回紧邻的下一个可见文本片段（用于自动链接的目标）。
fn next_text(spans: &[SyntaxSpan], i: usize, source: &str) -> String {
    spans
        .get(i + 1)
        .filter(|s| s.kind == SpanKind::Text)
        .map(|s| source[s.range.clone()].to_string())
        .unwrap_or_default()
}

/// 追加一个可见文本 run（跳过空串）。
fn push_text(
    out: &mut Vec<ParagraphContent<'static>>,
    text: &str,
    base: &BaseStyle,
    style: &InlineStyle,
    tag: bool,
) {
    if text.is_empty() {
        return;
    }
    out.push(text_run(text, run_property(base, style, tag)));
}

/// 根据基准样式与行内状态构造字符属性。
fn run_property(base: &BaseStyle, style: &InlineStyle, tag: bool) -> CharacterProperty<'static> {
    let mut prop = CharacterProperty::default();
    if base.bold || style.bold {
        prop = prop.bold(true);
    }
    if style.italic {
        prop = prop.italics(true);
    }
    if style.strike {
        prop = prop.strike(true);
    }
    if let Some(size) = base.size {
        prop = prop.size(size);
    }
    if tag {
        prop = prop.color("008000");
    }
    if style.code {
        prop = prop.color("A31515");
    }
    if style.link {
        prop = prop.color("0563C1").underline(Underline::default());
    }
    prop
}

/// 代码 run 的字符属性（暗色）。
fn code_property() -> CharacterProperty<'static> {
    CharacterProperty::default().color("333333")
}

/// 构造一个文本 run（空白保留，避免 Word 吞掉首尾空格）。
fn text_run(text: &str, property: CharacterProperty<'static>) -> ParagraphContent<'static> {
    ParagraphContent::Run(Run {
        property,
        content: vec![RunContent::Text(Text::from((
            text.to_string(),
            TextSpace::Preserve,
        )))],
    })
}

/// 构造一个行内换行（硬换行 / 代码块行间隔）。
fn line_break() -> ParagraphContent<'static> {
    ParagraphContent::Run(Run {
        property: CharacterProperty::default(),
        content: vec![RunContent::Break(Break::from(BreakType::TextWrapping))],
    })
}

/// 从图片源码 `![alt](dest)` 生成占位文本。
fn image_placeholder(raw: &str) -> String {
    match parse_image(raw) {
        Some((alt, _)) if !alt.is_empty() => format!("[图片: {alt}]"),
        _ => "[图片]".to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use docx::document::TableCellContent;

    /// 取出段落内所有 run 的可见文本（拼接）。
    fn paragraph_text(p: &Paragraph<'_>) -> String {
        let mut s = String::new();
        for c in &p.content {
            if let ParagraphContent::Run(r) = c {
                for rc in &r.content {
                    if let RunContent::Text(t) = rc {
                        s.push_str(&t.text);
                    }
                }
            }
        }
        s
    }

    /// 渲染并返回顶层块。
    fn render(source: &str) -> Vec<BodyContent<'static>> {
        let blocks = parse(source);
        let mut out = Vec::new();
        render_blocks(source, &blocks, 0, &mut out);
        out
    }

    #[test]
    fn docx_bytes_are_a_zip() {
        let bytes = to_docx("# 标题\n\n段落\n").unwrap();
        assert!(bytes.len() > 100);
        assert_eq!(&bytes[0..4], &[0x50, 0x4B, 0x03, 0x04]);
    }

    #[test]
    fn empty_source_still_valid() {
        let bytes = to_docx("").unwrap();
        assert_eq!(&bytes[0..4], &[0x50, 0x4B, 0x03, 0x04]);
    }

    #[test]
    fn paragraph_and_heading() {
        let out = render("# 标题\n");
        let BodyContent::Paragraph(p) = &out[0] else {
            panic!("应为段落");
        };
        assert_eq!(paragraph_text(p), "标题");
        // 标题：加粗 + 有字号
        let ParagraphContent::Run(r) = &p.content[0] else {
            panic!("应为 run");
        };
        assert!(r.property.bold.is_some());
        assert!(r.property.size.is_some());

        let out = render("普通段落\n");
        let BodyContent::Paragraph(p) = &out[0] else {
            panic!("应为段落");
        };
        assert_eq!(paragraph_text(p), "普通段落");
    }

    #[test]
    fn bold_italic_strike_code() {
        let out = render("**粗** *斜* ~~删~~ `码`\n");
        let BodyContent::Paragraph(p) = &out[0] else {
            panic!("应为段落");
        };
        assert_eq!(paragraph_text(p), "粗 斜 删 码");
        let flags: Vec<(bool, bool, bool)> = p
            .content
            .iter()
            .filter_map(|c| match c {
                ParagraphContent::Run(r) => Some((
                    r.property.bold.is_some(),
                    r.property.italics.is_some(),
                    r.property.strike.is_some(),
                )),
                _ => None,
            })
            .collect();
        assert!(flags.iter().any(|&(b, _, _)| b), "应有粗体 run");
        assert!(flags.iter().any(|&(_, i, _)| i), "应有斜体 run");
        assert!(flags.iter().any(|&(_, _, s)| s), "应有删除线 run");
    }

    #[test]
    fn link_appends_url() {
        let out = render("[文字](https://x.com)\n");
        let BodyContent::Paragraph(p) = &out[0] else {
            panic!("应为段落");
        };
        assert_eq!(paragraph_text(p), "文字 (https://x.com)");
    }

    #[test]
    fn autolink_keeps_text_only() {
        let out = render("<https://x.com>\n");
        let BodyContent::Paragraph(p) = &out[0] else {
            panic!("应为段落");
        };
        assert_eq!(paragraph_text(p), "https://x.com");
    }

    #[test]
    fn image_placeholder_text() {
        let out = render("![图](a.png)\n");
        let BodyContent::Paragraph(p) = &out[0] else {
            panic!("应为段落");
        };
        assert_eq!(paragraph_text(p), "[图片: 图]");
    }

    #[test]
    fn thematic_break_has_border() {
        let out = render("---\n");
        let BodyContent::Paragraph(p) = &out[0] else {
            panic!("应为段落");
        };
        assert!(p.property.border.is_some());
        assert!(p.content.is_empty());
    }

    #[test]
    fn table_shape() {
        let out = render("| a | b |\n| --- | --- |\n| 1 | 2 |\n");
        let BodyContent::Table(t) = &out[0] else {
            panic!("应为表格");
        };
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows[0].cells.len(), 2);
        let TableCellContent::Paragraph(p) = &t.rows[0].cells[0].content[0];
        assert_eq!(paragraph_text(p), "a");
    }

    #[test]
    fn list_markers() {
        let out = render("- 甲\n- 乙\n");
        assert_eq!(out.len(), 2);
        let BodyContent::Paragraph(p) = &out[0] else {
            panic!("应为段落");
        };
        assert_eq!(paragraph_text(p), "• 甲");
    }

    #[test]
    fn task_list_markers() {
        let out = render("- [x] 完成\n- [ ] 未完成\n");
        let BodyContent::Paragraph(p) = &out[0] else {
            panic!("应为段落");
        };
        assert_eq!(paragraph_text(p), "• ☑ 完成");
    }

    #[test]
    fn frontmatter_hidden() {
        let out = render("---\ntitle: x\n---\n正文\n");
        assert_eq!(out.len(), 1);
        let BodyContent::Paragraph(p) = &out[0] else {
            panic!("应为段落");
        };
        assert_eq!(paragraph_text(p), "正文");
    }

    #[test]
    fn code_block_preserves_lines() {
        let out = render("```rust\nfn f() {}\nlet x = 1;\n```\n");
        let BodyContent::Paragraph(p) = &out[0] else {
            panic!("应为段落");
        };
        assert_eq!(paragraph_text(p), "fn f() {}let x = 1;");
        // 两行之间应有一个换行 run
        assert!(p.content.iter().any(|c| matches!(
            c,
            ParagraphContent::Run(r) if r.content.iter().any(|rc| matches!(rc, RunContent::Break(_)))
        )));
    }
}
