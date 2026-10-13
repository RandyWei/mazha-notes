//! AST → Markdown 序列化（极简 MVP）。
//!
//! 基于 [`Block`] 与 [`SyntaxSpan`] 结构恢复常用语法。MVP 覆盖：
//! 段落、ATX 标题、分割线、围栏代码、引用、无序/有序/任务列表、
//! 行内：强调 `*x*`、粗体 `**x**`、删除线 `~~x~~`、行内代码 `` `x` ``、
//! 链接 `[text](url)`、图片 `![alt](src)`、转义、软/硬换行。

use crate::{Align, Block, BlockKind, SpanKind, SpanRole, SyntaxSpan, parse};
use std::ops::Range;

/// 将 AST 序列化为 Markdown。
pub fn serialize(blocks: &[Block]) -> String {
    let mut out = String::new();
    for (i, b) in blocks.iter().enumerate() {
        serialize_block(b, &mut out);
        if i + 1 < blocks.len() {
            out.push('\n');
        }
    }
    out
}

/// 把块森林序列化回 Markdown 文本。
///
/// 顶层块之间以**空行**分隔（保证段落不会被合并成一个），结果以单个换行结尾。
pub fn serialize_source(source: &str, blocks: &[Block]) -> String {
    let mut out = String::new();
    for b in blocks {
        out.push_str(&block_to_md(source, b, ""));
        // 块之间补一个空行（块自身已带一个行尾换行）
        out.push('\n');
    }
    // 收尾：压缩尾部换行，只保留一个
    while out.ends_with("\n\n") {
        out.pop();
    }
    if out.is_empty() {
        return String::new();
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// 把一个块渲染为 Markdown；`prefix` 会补到**每一行**的行首。
///
/// 之所以逐行补（而不是只补首行）：段落内的软换行（`SoftBreak`）渲染成 `\n` 后，
/// 续行同样需要前缀，否则"引用中的多行段落""列表项中的多行段落"会漏前缀。
fn block_to_md(source: &str, b: &Block, prefix: &str) -> String {
    match &b.kind {
        BlockKind::ThematicBreak => prefix_lines("---\n", prefix),
        BlockKind::Heading { level } => {
            let hashes = "#".repeat(*level as usize);
            prefix_lines(
                &format!("{hashes} {}\n", spans_to_inline_md(source, &b.spans)),
                prefix,
            )
        }
        BlockKind::Paragraph => prefix_lines(
            &format!("{}\n", spans_to_inline_md(source, &b.spans)),
            prefix,
        ),
        BlockKind::CodeBlock { lang, .. } => {
            let mut body = match lang {
                Some(l) => format!("```{l}\n"),
                None => "```\n".to_string(),
            };
            // 代码内容：围栏标记不输出，内容原样（续行由 prefix_lines 补前缀）
            for sp in &b.spans {
                if sp.role == SpanRole::CodeContent {
                    let text = slice(source, &sp.range);
                    body.push_str(&text);
                    if !text.ends_with('\n') {
                        body.push('\n');
                    }
                }
            }
            body.push_str("```\n");
            prefix_lines(&body, prefix)
        }
        BlockKind::BlockQuote => {
            // 引用：子块整体加 "> " 前缀；空引用也要留下 ">"
            if b.children.is_empty() {
                return prefix_lines(">\n", prefix);
            }
            let mut inner = String::new();
            for ch in &b.children {
                inner.push_str(&block_to_md(source, ch, "> "));
            }
            prefix_lines(&inner, prefix)
        }
        BlockKind::Callout { kind, title } => {
            // 提示块：首行 "> [!TYPE]"（+ 可选标题），后续子块同引用加 "> " 前缀
            let mut inner = String::new();
            inner.push_str("> [!");
            inner.push_str(kind.as_str());
            inner.push(']');
            if let Some(t) = title {
                inner.push(' ');
                inner.push_str(t);
            }
            inner.push('\n');
            for ch in &b.children {
                inner.push_str(&block_to_md(source, ch, "> "));
            }
            prefix_lines(&inner, prefix)
        }
        BlockKind::List { ordered, start } => {
            let mut s = String::new();
            for (i, item) in b.children.iter().enumerate() {
                let n = start + i as u64;
                s.push_str(&list_item_to_md(source, item, prefix, *ordered, n));
            }
            s
        }
        BlockKind::Table { align } => {
            // 表格：从源文本按行重建（竖线两端补空格）；分隔行紧跟表头重写，
            // 否则序列化结果不再构成表格。
            let rows = table_rows(source, b);
            let delim = delimiter_row(align);
            let mut s = String::new();
            for (i, row) in rows.iter().enumerate() {
                s.push_str(row);
                s.push('\n');
                if i == 0 {
                    s.push_str(&delim);
                    s.push('\n');
                }
            }
            s
        }
        // 列表项由 List 分支统一渲染，这里只在异常输入下兜底
        BlockKind::ListItem { .. } => list_item_to_md(source, b, prefix, false, 1),
    }
}

/// 渲染列表项：标记 + 首个段落同行，其余子块落在续行（缩进对齐内容起点）。
fn list_item_to_md(source: &str, item: &Block, prefix: &str, ordered: bool, number: u64) -> String {
    let marker = item_marker(item, ordered, number);
    let cont = format!("{prefix}{}", " ".repeat(marker.len()));

    let mut s = String::new();
    let mut wrote_head = false;
    for (i, ch) in item.children.iter().enumerate() {
        if i == 0 && matches!(ch.kind, BlockKind::Paragraph) {
            // 首段与标记同行：首行加 prefix，段内软换行续行加 cont
            let head = format!("{marker}{}\n", spans_to_inline_md(source, &ch.spans));
            for (k, line) in head.split_inclusive('\n').enumerate() {
                if k == 0 {
                    s.push_str(prefix);
                } else {
                    s.push_str(&cont);
                }
                s.push_str(line);
            }
            wrote_head = true;
        } else {
            // 其余子块（嵌套列表/引用/续段）走续行缩进
            s.push_str(&block_to_md(source, ch, &cont));
        }
    }
    if !wrote_head {
        // 空列表项（或内容不是段落）：标记单独成行
        s.push_str(prefix);
        s.push_str(&marker);
        s.push('\n');
    }
    s
}

/// 列表项标记串：`- ` / `- [ ] ` / `- [x] ` / `1. `。
fn item_marker(item: &Block, ordered: bool, number: u64) -> String {
    let base = if ordered {
        format!("{number}. ")
    } else {
        "- ".to_string()
    };
    match item.kind {
        BlockKind::ListItem { task: Some(done) } => {
            if done {
                format!("{base}[x] ")
            } else {
                format!("{base}[ ] ")
            }
        }
        _ => base,
    }
}

/// 给一段文本的**每一行**都补上前缀（末行无换行时也处理）。
fn prefix_lines(body: &str, prefix: &str) -> String {
    if prefix.is_empty() {
        return body.to_string();
    }
    let mut out = String::with_capacity(body.len() + prefix.len() * 4);
    for (k, line) in body.split_inclusive('\n').enumerate() {
        // 只跳过完全空的行（纯换行），否则 "> \n> " 之间会多出无意义空格
        if line == "\n" && k > 0 {
            out.push_str(prefix.trim_end_matches(' '));
            out.push('\n');
        } else {
            out.push_str(prefix);
            out.push_str(line);
        }
    }
    out
}

/// 取出表格的每个数据行（跳过源文本里的分隔行，由调用方按对齐重写）。
///
/// 行从源文本按 `|` 切分并 trim 后重建，故单元格内的行内标记（`**粗**` 等）
/// 会随源文本原样保留。
fn table_rows(source: &str, b: &Block) -> Vec<String> {
    let text = &source[b.range.clone()];
    text.lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.trim().is_empty())
        .filter(|l| !is_delim_like(l))
        .map(normalize_row)
        .collect()
}

/// 粗略判断是否是分隔行（避免把 `| --- |` 当数据行）。
fn is_delim_like(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty() && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ' | '\t')) && t.contains('-')
}

/// 规范化一行表格：切掉首尾竖线后按未转义 `|` 切分，逐单元格 trim。
fn normalize_row(line: &str) -> String {
    let t = line.trim();
    let inner = t.strip_prefix('|').unwrap_or(t);
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    let mut out = String::from("|");
    for cell in split_cells(inner) {
        out.push(' ');
        out.push_str(cell.trim());
        out.push_str(" |");
    }
    out
}

/// 按**未转义**的 `|` 切分单元格。
fn split_cells(inner: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let bytes = inner.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'|' && (i == 0 || bytes[i - 1] != b'\\') {
            out.push(&inner[start..i]);
            i += 1;
            start = i;
        } else {
            i += 1;
        }
    }
    out.push(&inner[start..]);
    out
}

/// 按对齐方式生成分隔行（`| --- | :-: | --: |`）。
fn delimiter_row(align: &[Align]) -> String {
    let mut out = String::from("|");
    for a in align {
        out.push_str(match a {
            Align::Left => " --- ",
            Align::Center => " :-: ",
            Align::Right => " --: ",
        });
        out.push('|');
    }
    out
}

fn spans_to_inline_md(source: &str, spans: &[SyntaxSpan]) -> String {
    let mut out = String::new();
    let mut pos = 0usize;
    while pos < spans.len() {
        let sp = &spans[pos];
        if sp.kind == SpanKind::Text {
            out.push_str(&slice(source, &sp.range));
            pos += 1;
            continue;
        }
        match (sp.kind, sp.role) {
            (SpanKind::Marker, SpanRole::SoftBreak) => out.push('\n'),
            (SpanKind::Marker, SpanRole::HardBreak) => out.push_str("  \n"),
            (SpanKind::Marker, SpanRole::EscapeMarker) => out.push('\\'),
            (SpanKind::Marker, SpanRole::InlineCode) => out.push('`'),
            (SpanKind::Marker, SpanRole::Strong) => out.push_str("**"),
            (SpanKind::Marker, SpanRole::Emphasis) => out.push('*'),
            (SpanKind::Marker, SpanRole::Strikethrough) => out.push_str("~~"),
            (SpanKind::Marker, SpanRole::LinkMarker) => {
                let ch = source.as_bytes()[sp.range.start];
                if ch == b'[' {
                    // 普通链接：`[text](url)` 整体重建（含嵌套行内样式）
                    let mut dest: Option<String> = None;
                    let mut close: Option<usize> = None;
                    let mut bracket: Option<usize> = None;
                    let mut k = pos + 1;
                    while k < spans.len() {
                        let item = &spans[k];
                        if item.kind == SpanKind::Marker && item.role == SpanRole::LinkDest {
                            dest = Some(slice(source, &item.range));
                        }
                        if item.kind == SpanKind::Marker && item.role == SpanRole::LinkMarker {
                            let ch2 = source.as_bytes()[item.range.start];
                            if ch2 == b']' && bracket.is_none() {
                                bracket = Some(k);
                            }
                            if ch2 == b')' {
                                close = Some(k);
                                break;
                            }
                        }
                        k += 1;
                    }
                    let bracket = bracket.unwrap_or(close.unwrap_or(spans.len()));
                    out.push('[');
                    for item2 in &spans[pos + 1..bracket] {
                        if item2.kind == SpanKind::Text {
                            out.push_str(&slice(source, &item2.range));
                        } else if item2.kind == SpanKind::Marker {
                            match item2.role {
                                SpanRole::Strong => out.push_str("**"),
                                SpanRole::Emphasis => out.push('*'),
                                SpanRole::Strikethrough => out.push_str("~~"),
                                SpanRole::InlineCode => out.push('`'),
                                _ => {}
                            }
                        }
                    }
                    out.push_str("](");
                    out.push_str(&dest.unwrap_or_default());
                    out.push(')');
                    pos = (close.unwrap_or(spans.len())) + 1;
                    continue;
                }
                match ch {
                    b'<' => out.push('<'),
                    b'>' => out.push('>'),
                    _ => {}
                }
            }
            (SpanKind::Marker, SpanRole::WikiLinkMarker) => {
                // 仅在 `[[`（起始标记）处重建；`|`/`]]` 由重建过程整体消费
                if source.as_bytes().get(sp.range.start) == Some(&b'[') {
                    pos = reconstruct_wiki_link(source, spans, pos, &mut out);
                    continue;
                }
            }
            (SpanKind::Widget, SpanRole::ImageMarker) => {
                out.push_str(&slice(source, &sp.range));
            }
            _ => {}
        }
        pos += 1;
    }
    out
}

/// 从 `pos`（`[[` 标记处）重建 wiki-link 并写入 `out`；返回 `]]` 之后的 `pos`。
fn reconstruct_wiki_link(
    source: &str,
    spans: &[SyntaxSpan],
    pos: usize,
    out: &mut String,
) -> usize {
    let mut target: Option<String> = None;
    let mut alias: Option<String> = None;
    let mut k = pos + 1;
    while k < spans.len() {
        let item = &spans[k];
        if item.kind == SpanKind::Marker
            && item.role == SpanRole::WikiLinkMarker
            && source.as_bytes().get(item.range.start) == Some(&b']')
        {
            break; // `]]`
        }
        if item.role == SpanRole::WikiLinkTarget {
            target = Some(slice(source, &item.range));
        }
        if item.role == SpanRole::WikiLinkText {
            alias = Some(slice(source, &item.range));
        }
        k += 1;
    }
    out.push_str("[[");
    out.push_str(&target.unwrap_or_default());
    if let Some(a) = alias {
        out.push('|');
        out.push_str(&a);
    }
    out.push_str("]]");
    // 跳到 `]]` 之后
    while k < spans.len() {
        let item = &spans[k];
        if item.kind == SpanKind::Marker
            && item.role == SpanRole::WikiLinkMarker
            && source.as_bytes().get(item.range.start) == Some(&b']')
        {
            return k + 1;
        }
        k += 1;
    }
    k
}

fn serialize_block(_block: &Block, _out: &mut String) {}

fn slice(source: &str, r: &Range<usize>) -> String {
    source[r.start..r.end].to_string()
}

/// 将解析结果重新序列化（parse→serialize，MVP）。
pub fn to_md(source: &str) -> String {
    let blocks = parse(source);
    serialize_source(source, &blocks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_md_paragraph() {
        let s = "hello world\n";
        let out = to_md(s);
        assert!(out.contains("hello world"));
    }

    #[test]
    fn to_md_heading() {
        let out = to_md("# 标题\n");
        assert!(out.contains("标题"));
        assert!(out.starts_with('#'));
    }

    #[test]
    fn to_md_list() {
        let out = to_md("- a\n- b\n");
        assert!(out.contains("- a"));
        assert!(out.contains("- b"));
    }
    #[test]
    fn to_md_code_block() {
        let out = to_md("```rust\nfn main(){}\n```\n");
        assert!(out.contains("```"));
        assert!(out.contains("fn main(){}"));
    }

    #[test]
    fn to_md_blockquote() {
        let out = to_md("> 引用\n");
        assert!(out.contains('>'));
    }

    #[test]
    fn to_md_task_list() {
        let out = to_md("- [x] done\n- [ ] todo\n");
        assert!(out.contains("[x]"));
        assert!(out.contains("[ ]"));
    }
}

#[test]
fn to_md_inline_marks() {
    let out = to_md("**粗** *斜* ~~删~~ `码`\n");
    assert!(out.contains("粗"));
    assert!(out.contains("斜"));
    assert!(out.contains("删"));
}

#[test]
fn to_md_link() {
    let out = to_md("[文字](https://x.com)\n");
    assert!(out.contains("文字"));
    assert!(out.contains("https://x.com"));
}

#[cfg(test)]
mod roundtrip {
    use super::to_md;
    use crate::{Block, BlockKind, parse};

    /// 结构签名：顶层块类型 + 嵌套一层（用于判定"结构是否保持"）。
    fn shape(blocks: &[Block]) -> Vec<String> {
        let mut out = Vec::new();
        for b in blocks {
            out.push(format!("{:?}", b.kind));
            for c in &b.children {
                out.push(format!("  {:?}", c.kind));
            }
        }
        out
    }

    /// 断言序列化后再解析，结构保持一致（这是序列化器的核心契约）。
    fn assert_structure_preserved(src: &str) {
        let out = to_md(src);
        let before = shape(&parse(src));
        let after = shape(&parse(&out));
        assert_eq!(
            before, after,
            "序列化后结构应保持一致\nsrc={src:?}\nout={out:?}\nbefore={before:?}\nafter={after:?}"
        );
    }

    /// 断言规范化输入可**逐字节**往返（序列化是幂等的）。
    fn assert_roundtrip_exact(src: &str) {
        let out = to_md(src);
        assert_eq!(out, src, "规范化输入应逐字节往返");
        // 幂等：再次序列化结果不变
        assert_eq!(to_md(&out), out, "序列化应幂等");
    }

    #[test]
    fn nested_unordered_list_roundtrip() {
        assert_roundtrip_exact("- a\n  - b\n");
        assert_roundtrip_exact("- a\n  - b\n  - c\n");
        assert_structure_preserved("- a\n- b\n");
    }

    #[test]
    fn nested_ordered_list_roundtrip() {
        assert_roundtrip_exact("1. a\n   1. b\n");
        assert_roundtrip_exact("1. a\n2. b\n");
    }

    #[test]
    fn quote_with_nested_list_roundtrip() {
        assert_roundtrip_exact("> - a\n>   - b\n");
        assert_structure_preserved("> 引用\n");
    }

    #[test]
    fn task_list_roundtrip() {
        assert_roundtrip_exact("- [x] done\n- [ ] todo\n");
        assert_structure_preserved("- [x] done\n- [ ] todo\n");
    }

    #[test]
    fn mixed_document_structure_preserved() {
        let src = "# 标题\n\n正文第一段，含**粗体**与`代码`。\n\n```rust\nfn main() {}\n```\n\n> 引用一\n> 引用二\n\n- 项一\n  - 子项\n\n1. 第一\n2. 第二\n\n---\n\n结尾。\n";
        assert_structure_preserved(src);
    }

    #[test]
    fn empty_and_blank_inputs() {
        assert_eq!(to_md(""), "");
        // 纯空行：没有任何块
        assert_eq!(to_md("\n\n\n"), "");
    }

    #[test]
    fn inline_marks_roundtrip_structure() {
        for src in [
            "**粗** 与 *斜* 与 ~~删~~ 与 `码`\n",
            "[链接](https://example.com/path)\n",
            "![图片](img.png)\n",
            "转义 \\* 字面量\n",
        ] {
            assert_structure_preserved(src);
        }
    }

    #[test]
    fn image_widget_survives_roundtrip() {
        let out = to_md("![图](a.png)\n");
        assert_eq!(out, "![图](a.png)\n");
    }

    #[test]
    fn task_kind_is_reachable() {
        // 保证 BlockKind 在测试中被引用（无死代码告警）
        let b = parse("- [ ] x\n").pop().unwrap();
        assert!(matches!(b.kind, BlockKind::List { .. }));
    }

    #[test]
    fn wikilink_roundtrip_no_alias() {
        let src = "[[Page]]\n";
        let out = to_md(src);
        assert_eq!(out, src);
    }

    #[test]
    fn wikilink_roundtrip_with_alias() {
        let src = "[[Page|别名]]\n";
        let out = to_md(src);
        assert_eq!(out, src);
    }

    #[test]
    fn wikilink_roundtrip_mixed() {
        let src = "前缀 [[Page]] 后缀\n";
        let out = to_md(src);
        assert_eq!(out, src);
    }

    #[test]
    fn wikilink_idempotent() {
        let src = "[[Page|别名]]\n";
        let once = to_md(src);
        let twice = to_md(&once);
        assert_eq!(once, twice);
    }
}
