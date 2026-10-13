//! Markdown → Typst 标记转换（PDF 导出管线的第一步）。
//!
//! 把 [`mazha-md`] 的块/片段模型转换为 Typst 标记文本。编译为 PDF 字节
//! 需要 Typst 编译器（`World` 实现），作为后续步骤；本模块只做纯文本转换，
//! 不引入编译器依赖。见 `SPEC.md` §8、D21。

use mazha_md::{Block, BlockKind, SpanKind, SpanRole, SyntaxSpan, parse};
use std::fmt::Write as _;

/// 把 Markdown 源文本转换为 Typst 标记。
pub fn to_typst(source: &str) -> String {
    let blocks = parse(source);
    let mut out = String::new();
    blocks_to_typst(source, &blocks, &mut out);
    out
}

/// 递归转换一组块。
fn blocks_to_typst(source: &str, blocks: &[Block], out: &mut String) {
    for b in blocks {
        block_to_typst(source, b, out);
        out.push('\n');
    }
}

/// 转换单个块。
fn block_to_typst(source: &str, b: &Block, out: &mut String) {
    match &b.kind {
        BlockKind::FrontMatter { .. } => {
            // front-matter 是元数据，Typst 里用 #metadata 嵌入（不渲染）
            let _ = write!(out, "#metadata(none)");
        }
        BlockKind::Heading { level } => {
            // ATX 标题 → Typst `=` 系列
            for _ in 0..*level {
                out.push('=');
            }
            out.push(' ');
            inline_to_typst(source, &b.spans, out);
        }
        BlockKind::Paragraph => {
            inline_to_typst(source, &b.spans, out);
        }
        BlockKind::ThematicBreak => {
            out.push_str("---");
        }
        BlockKind::BlockQuote => {
            out.push_str("#quote[");
            blocks_to_typst(source, &b.children, out);
            out.push(']');
        }
        BlockKind::Callout { kind, title } => {
            // 提示块 → Typst block + 醒目标题
            let heading = title.as_deref().unwrap_or_else(|| kind.as_str());
            let _ = write!(out, "#block(fill: luma(240), inset: 10pt, radius: 4pt)[");
            let _ = write!(out, "*{heading}*\n\n");
            blocks_to_typst(source, &b.children, out);
            out.push(']');
        }
        BlockKind::List { ordered, start } => {
            for (i, item) in b.children.iter().enumerate() {
                let marker = if *ordered {
                    "+ ".to_string()
                } else {
                    "- ".to_string()
                };
                let _ = write!(out, "{marker}");
                let _ = start; // 起始序号：Typst 用 enum 计数器，MVP 忽略自定义起始
                let _ = i;
                list_item_to_typst(source, item, out);
                out.push('\n');
            }
        }
        BlockKind::ListItem { .. } => {
            // 单独的列表项（非列表容器直接子项）：按段落处理
            blocks_to_typst(source, &b.children, out);
        }
        BlockKind::CodeBlock { lang, .. } => {
            let lang_tag = lang.as_deref().unwrap_or("");
            let _ = writeln!(out, "```{lang_tag}");
            for sp in &b.spans {
                if sp.role == SpanRole::CodeContent {
                    out.push_str(&source[sp.range.clone()]);
                }
            }
            out.push_str("\n```");
        }
        BlockKind::Table { align } => {
            table_to_typst(source, b, align, out);
        }
    }
}

/// 列表项：任务标记 + 内容。
fn list_item_to_typst(source: &str, item: &Block, out: &mut String) {
    if let BlockKind::ListItem { task } = &item.kind {
        match task {
            Some(true) => out.push_str("✔ "),
            Some(false) => out.push_str("☐ "),
            None => {}
        }
    }
    // 列表项内容：直接子块（通常是段落）
    for (ci, child) in item.children.iter().enumerate() {
        if ci > 0 {
            out.push('\n');
        }
        match &child.kind {
            BlockKind::Paragraph => inline_to_typst(source, &child.spans, out),
            _ => block_to_typst(source, child, out),
        }
    }
}

/// 表格 → Typst `#table`。
fn table_to_typst(source: &str, b: &Block, align: &[mazha_md::Align], out: &mut String) {
    let rows = table_rows_typst(source, b);
    if rows.is_empty() {
        return;
    }
    let cols = align.len().max(rows.first().map_or(0, Vec::len));
    out.push_str("#table(\n");
    let _ = writeln!(out, "  columns: {cols},");
    for row in &rows {
        for (ci, cell) in row.iter().enumerate() {
            let a = match align.get(ci) {
                Some(mazha_md::Align::Center) => "center",
                Some(mazha_md::Align::Right) => "right",
                _ => "left",
            };
            let _ = write!(out, "  table.cell(align: {a})[{cell}]");
        }
        out.push('\n');
    }
    out.push(']');
}

/// 把表格块按行切成"单元格可见文本"（跳过分隔行）。
///
/// 每行按竖线切成单元格，单元格文本为可见片段拼接（trim 后）。
fn table_rows_typst(source: &str, b: &Block) -> Vec<Vec<String>> {
    let bytes = source.as_bytes();
    let end = b.range.end.min(bytes.len());
    let mut bounds: Vec<std::ops::Range<usize>> = Vec::new();
    let mut start = b.range.start;
    while start < end {
        let e = bytes[start..end]
            .iter()
            .position(|&c| c == b'\n')
            .map_or(end, |i| start + i);
        bounds.push(start..e);
        start = e + 1;
    }
    let mut rows = Vec::new();
    for line in &bounds {
        // 本行内的片段按竖线切成单元格（分隔行整行跳过）
        let mut cells: Vec<String> = vec![String::new()];
        for sp in &b.spans {
            if sp.role == SpanRole::TableDelim {
                continue;
            }
            if sp.is_empty() || sp.range.start < line.start || sp.range.start >= line.end {
                continue;
            }
            if sp.role == SpanRole::TablePipe {
                cells.push(String::new());
            } else {
                cells
                    .last_mut()
                    .expect("至少一个单元格")
                    .push_str(&source[sp.range.clone()]);
            }
        }
        // 去首尾空单元格（行首/行尾竖线产生）
        if cells.first().is_some_and(String::is_empty) {
            cells.remove(0);
        }
        if cells.last().is_some_and(String::is_empty) {
            cells.pop();
        }
        // 单元格文本 trim
        for c in &mut cells {
            *c = c.trim().to_string();
        }
        if !cells.is_empty() {
            rows.push(cells);
        }
    }
    rows
}

/// 把一组行内片段转换为 Typst 标记。
fn inline_to_typst(source: &str, spans: &[SyntaxSpan], out: &mut String) {
    let mut i = 0usize;
    let mut in_strike = false;
    while i < spans.len() {
        let sp = &spans[i];
        match (sp.kind, sp.role) {
            // 标签：单独样式（须在通用 Text 之前）
            (SpanKind::Text, SpanRole::Tag) => {
                let _ = write!(out, "#text(fill: blue)[{}]", &source[sp.range.clone()]);
                i += 1;
            }
            // 可见文本一律输出内容（样式由成对 Marker 开合）
            (SpanKind::Text, _) => {
                out.push_str(&source[sp.range.clone()]);
                i += 1;
            }
            (SpanKind::Marker, SpanRole::Strong) => {
                out.push('*');
                i += 1;
            }
            (SpanKind::Marker, SpanRole::Emphasis) => {
                out.push('_');
                i += 1;
            }
            (SpanKind::Marker, SpanRole::Strikethrough) => {
                if in_strike {
                    out.push(']');
                } else {
                    out.push_str("#strike[");
                }
                in_strike = !in_strike;
                i += 1;
            }
            (SpanKind::Marker, SpanRole::InlineCode) => {
                out.push('`');
                i += 1;
            }
            (SpanKind::Marker, SpanRole::LinkMarker) => {
                let ch = source.as_bytes().get(sp.range.start).copied();
                if ch == Some(b'[') {
                    // 找目标 URL
                    let dest = spans[i..]
                        .iter()
                        .find(|s| s.role == SpanRole::LinkDest)
                        .map(|s| source[s.range.clone()].to_string())
                        .unwrap_or_default();
                    let _ = write!(out, "#link(\"{dest}\")[");
                } else if ch == Some(b')') {
                    out.push(']');
                }
                i += 1;
            }
            (SpanKind::Widget, SpanRole::ImageMarker) => {
                // 图片：#image("src")[alt]
                if let Some((alt, dest)) = parse_image_typst(&source[sp.range.clone()]) {
                    let _ = write!(out, "#image(\"{dest}\")[{alt}]");
                }
                i += 1;
            }
            (SpanKind::Marker, SpanRole::SoftBreak) => {
                out.push('\n');
                i += 1;
            }
            (SpanKind::Marker, SpanRole::HardBreak) => {
                out.push_str("\\\n");
                i += 1;
            }
            (SpanKind::Marker, SpanRole::EscapeMarker) => {
                // 转义反斜杠后的标点：Typst 里直接输出标点
                if let Some(next) = spans.get(i + 1) {
                    out.push_str(&source[next.range.clone()]);
                    i += 2;
                } else {
                    i += 1;
                }
            }
            // 其余标记（标题/引用/列表/围栏/链接目标等）导出时隐藏
            _ => i += 1,
        }
    }
}

/// 从 `![alt](dest)` 源文本解析 alt 与 dest。
fn parse_image_typst(raw: &str) -> Option<(String, String)> {
    let inner = raw.strip_prefix("![")?.strip_suffix(')')?;
    let (alt, dest) = inner.split_once("](")?;
    Some((alt.to_string(), dest.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typst_heading() {
        let t = to_typst("# 标题\n");
        assert_eq!(t.trim(), "= 标题");
    }

    #[test]
    fn typst_heading_levels() {
        assert!(to_typst("## 二\n").trim().starts_with("== 二"));
        assert!(to_typst("### 三\n").trim().starts_with("=== 三"));
    }

    #[test]
    fn typst_paragraph_bold_italic() {
        let t = to_typst("**粗** *斜*\n");
        assert!(t.contains("*粗*"));
        assert!(t.contains("_斜_"));
    }

    #[test]
    fn typst_inline_code() {
        let t = to_typst("`code`\n");
        assert!(t.contains("`code`"));
    }

    #[test]
    fn typst_link() {
        let t = to_typst("[文本](https://x.com)\n");
        assert!(t.contains("#link(\"https://x.com\")["));
    }

    #[test]
    fn typst_list_unordered() {
        let t = to_typst("- 甲\n- 乙\n");
        assert!(t.contains("- 甲"));
        assert!(t.contains("- 乙"));
    }

    #[test]
    fn typst_list_ordered() {
        let t = to_typst("1. 甲\n2. 乙\n");
        assert!(t.contains("+ 甲"));
    }

    #[test]
    fn typst_code_block() {
        let t = to_typst("```rust\nfn main() {}\n```\n");
        assert!(t.contains("```rust"));
        assert!(t.contains("fn main() {}"));
        assert!(t.contains("```"));
    }

    #[test]
    fn typst_blockquote() {
        let t = to_typst("> 引用\n");
        assert!(t.contains("#quote["));
    }

    #[test]
    fn typst_table() {
        let t = to_typst("| a | b |\n| --- | --- |\n| 1 | 2 |\n");
        assert!(t.contains("#table("));
        assert!(t.contains("columns: 2,"));
        assert!(t.contains("[a]"));
        assert!(t.contains("[b]"));
        assert!(t.contains("[1]"));
        assert!(t.contains("[2]"));
    }

    #[test]
    fn typst_thematic_break() {
        let t = to_typst("---\n");
        assert!(t.contains("---"));
    }

    #[test]
    fn typst_callout() {
        let t = to_typst("> [!NOTE]\n> 内容\n");
        assert!(t.contains("#block("));
        assert!(t.contains("*NOTE*"));
    }

    #[test]
    fn typst_wikilink_visible() {
        let t = to_typst("[[Page]]\n");
        assert!(t.contains("Page"));
    }

    #[test]
    fn typst_tag_styled() {
        let t = to_typst("#标签\n");
        assert!(t.contains("#text(fill: blue)"));
        assert!(t.contains("#标签"));
    }

    #[test]
    fn typst_image() {
        let t = to_typst("![图](img.png)\n");
        assert!(t.contains("#image(\"img.png\")"));
    }

    #[test]
    fn typst_strikethrough() {
        let t = to_typst("~~删~~\n");
        assert!(t.contains("#strike["));
    }
}
