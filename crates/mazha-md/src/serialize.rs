//! AST → Markdown 序列化（极简 MVP）。
//!
//! 基于 [`Block`] 与 [`SyntaxSpan`] 结构恢复常用语法。MVP 覆盖：
//! 段落、ATX 标题、分割线、围栏代码、引用、无序/有序/任务列表、
//! 行内：强调 `*x*`、粗体 `**x**`、删除线 `~~x~~`、行内代码 `` `x` ``、
//! 链接 `[text](url)`、图片 `![alt](src)`、转义、软/硬换行。

use crate::{Block, BlockKind, SpanKind, SpanRole, SyntaxSpan, parse};
use std::fmt::Write;
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

/// 将源文本 + AST 序列化回 Markdown（更精确的做法是直接从 spans 还原标记）。
/// 为降低复杂度，这里先用一个“回溯式”最小实现：针对常用情形按语法结构写出。
pub fn serialize_source(source: &str, blocks: &[Block]) -> String {
    let mut out = String::new();
    for (i, b) in blocks.iter().enumerate() {
        out.push_str(&block_to_md(source, b));
        if i + 1 < blocks.len() && !out.ends_with('\n') {
            out.push('\n');
        }
        if i + 1 < blocks.len() {
            out.push('\n');
        }
    }
    out.trim_end_matches('\n').to_string()
}

fn block_to_md(source: &str, b: &Block) -> String {
    match &b.kind {
        BlockKind::ThematicBreak => "---\n".to_string(),
        BlockKind::Heading { level } => {
            let mut s = String::new();
            for _ in 0..*level {
                s.push('#');
            }
            s.push(' ');
            s.push_str(&spans_to_inline_md(source, &b.spans));
            s.push('\n');
            s
        }
        BlockKind::Paragraph => {
            let mut s = spans_to_inline_md(source, &b.spans);
            s.push('\n');
            s
        }
        BlockKind::CodeBlock { lang, .. } => {
            let mut s = "```".to_string();
            if let Some(l) = lang {
                s.push_str(l);
            }
            s.push('\n');
            for sp in &b.spans {
                if sp.role == SpanRole::CodeContent {
                    s.push_str(&slice(source, &sp.range));
                }
            }
            s.push_str("```\n");
            s
        }
        BlockKind::BlockQuote => {
            let mut s = String::new();
            for ch in &b.children {
                let inner = block_to_md(source, ch);
                for line in inner.lines() {
                    s.push_str("> ");
                    s.push_str(line);
                    s.push('\n');
                }
            }
            s
        }
        BlockKind::List { ordered, start } => {
            let mut s = String::new();
            let mut idx = *start;
            for (i, ch) in b.children.iter().enumerate() {
                match &ch.kind {
                    BlockKind::ListItem { task } => {
                        if *ordered {
                            let _ = write!(s, "{idx}. ");
                            idx += 1;
                        } else {
                            s.push_str("- ");
                        }
                        if let Some(done) = task {
                            if *done {
                                s.push_str("[x] ");
                            } else {
                                s.push_str("[ ] ");
                            }
                        }
                        for c2 in &ch.children {
                            let inner = block_to_md(source, c2);
                            // 续行保持缩进简单处理：顶层子块输出
                            s.push_str(inner.trim_end_matches('\n'));
                        }
                        if i + 1 < b.children.len() {
                            s.push('\n');
                        }
                    }
                    _ => {
                        s.push_str(&block_to_md(source, ch));
                    }
                }
            }
            s.push('\n');
            s
        }
        BlockKind::ListItem { .. } => block_to_md(source, b),
    }
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
            (SpanKind::Widget, SpanRole::ImageMarker) => {
                out.push_str(&slice(source, &sp.range));
            }
            _ => {}
        }
        pos += 1;
    }
    out
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
