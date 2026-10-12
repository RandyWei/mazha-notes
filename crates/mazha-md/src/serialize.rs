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
    for (i, sp) in spans.iter().enumerate() {
        match (sp.kind, sp.role) {
            (SpanKind::Text, _) => out.push_str(&slice(source, &sp.range)),
            (SpanKind::Marker, SpanRole::SoftBreak) => out.push('\n'),
            (SpanKind::Marker, SpanRole::HardBreak) => out.push_str("  \n"),
            (SpanKind::Marker, SpanRole::EscapeMarker) => out.push('\\'),
            (SpanKind::Marker, SpanRole::InlineCode) => {
                // 同一角色的开/闭：此处只输出 ``
                out.push('`');
            }
            (SpanKind::Marker, SpanRole::Strong) => out.push_str("**"),
            (SpanKind::Marker, SpanRole::Emphasis) => out.push('*'),
            (SpanKind::Marker, SpanRole::Strikethrough) => out.push_str("~~"),
            (SpanKind::Marker, SpanRole::LinkMarker) => {
                let b = source.as_bytes()[sp.range.start];
                match b {
                    b'[' => out.push('['),
                    b']' => out.push(']'),
                    b'(' => out.push('('),
                    b')' => out.push(')'),
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
        // 跳过 LinkDest 的单独输出（放在 LinkMarker 流程）
        let _ = i;
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
