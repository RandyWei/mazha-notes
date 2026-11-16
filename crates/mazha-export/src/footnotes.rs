//! 导出前的脚注规整（决策 D69）。
//!
//! WYSIWYG 编辑器保留脚注标签原文（`[^label]`），以便直接编辑；导出时则按
//! 「引用首次出现顺序」编号，并把定义块**归集到文末**。两种视图的差异集中在这里
//! 通过**一次源文本重写**完成，各格式的渲染器（HTML/Typst/DOCX/EPUB）因此不必
//! 各自理解脚注编号：
//!
//! 1. `renumber`：把引用与定义头部的标签文本替换成序号（如 `[^note]` → `[^1]`）；
//!    只改写 `SpanRole::FootnoteRef` 片段，代码块/公式等字面量不受影响。
//! 2. `move_definitions`：把**顶层**脚注定义块整块移到文末，并按序号排序。
//!
//! 保留的限制（写入 SPEC D69）：
//! - 嵌套在引用/列表里的定义块不移动（移动会破坏容器结构），但仍会重新编号；
//! - 重复定义的标签都改成同一序号，位置按序号稳定排序；
//! - 没有脚注的文档走快速路径，返回的字符串与输入逐字节相同。

use mazha_md::{Block, BlockKind, FootnoteIndex, SpanKind, SpanRole, parse};
use std::ops::Range;

/// 把源文本规整为「导出用」脚注形态：顺序编号 + 定义归集文末。
///
/// 文档没有任何脚注时原样返回（零拷贝之外的唯一代价是一次解析）。
pub(crate) fn prepare(source: &str) -> String {
    let blocks = parse(source);
    let index = FootnoteIndex::new(source, &blocks);
    if index.is_empty() {
        return source.to_string();
    }
    let renumbered = renumber(source, &blocks, &index);
    // 重编号改变了字节长度，必须重新解析后再定位定义块。
    move_definitions(&renumbered)
}

/// 把所有脚注标签（引用 + 定义头部）替换为序号。
///
/// 片段按起点**倒序**替换，保证尚未处理的片段偏移仍然有效。
fn renumber(source: &str, blocks: &[Block], index: &FootnoteIndex) -> String {
    let mut spans: Vec<Range<usize>> = Vec::new();
    collect_label_spans(blocks, &mut spans);
    spans.sort_by_key(|r| std::cmp::Reverse(r.start));

    let mut out = source.to_string();
    for range in spans {
        let Some(label) = source.get(range.clone()) else {
            continue;
        };
        let Some(number) = index.number_of(label) else {
            // 索引里没有的标签（理论上不会出现）保持原文，避免误删内容。
            continue;
        };
        out.replace_range(range, &number.to_string());
    }
    out
}

/// 深度优先收集所有 `FootnoteRef` 可见片段的区间（引用与定义头部都会命中）。
fn collect_label_spans(blocks: &[Block], out: &mut Vec<Range<usize>>) {
    for block in blocks {
        for span in &block.spans {
            if span.kind == SpanKind::Text && span.role == SpanRole::FootnoteRef {
                out.push(span.range.clone());
            }
        }
        collect_label_spans(&block.children, out);
    }
}

/// 把顶层脚注定义块移到文末，并按序号升序排列。
///
/// 只处理顶层块：引用/列表内部的定义保持原位（移动会破坏容器前缀），
/// 它们已经被 `renumber` 编号，仍按原位渲染。
fn move_definitions(source: &str) -> String {
    let blocks = parse(source);
    let mut defs: Vec<(usize, Range<usize>)> = blocks
        .iter()
        .filter_map(|block| match &block.kind {
            BlockKind::FootnoteDef { label } => Some((
                label.parse::<usize>().unwrap_or(usize::MAX),
                block.range.clone(),
            )),
            _ => None,
        })
        .collect();
    if defs.is_empty() {
        return source.to_string();
    }

    // 先按序号排序取出文本（此时源文本尚未删除，区间有效）。
    defs.sort_by_key(|(number, _)| *number);
    let texts: Vec<&str> = defs
        .iter()
        .filter_map(|(_, range)| source.get(range.clone()))
        .collect();

    // 删除必须按**源偏移倒序**（与序号顺序无关），否则先删前面的定义会让后面区间失效。
    let mut cuts: Vec<&Range<usize>> = defs.iter().map(|(_, range)| range).collect();
    cuts.sort_by_key(|range| std::cmp::Reverse(range.start));
    let mut body = source.to_string();
    for range in cuts {
        let start = line_start(&body, range.start);
        let end = line_end_inclusive(&body, range.end);
        if start <= end {
            body.replace_range(start..end, "");
        }
    }

    let mut out = body.trim_end().to_string();
    for text in texts {
        out.push_str("\n\n");
        out.push_str(text.trim_end());
    }
    out.push('\n');
    out
}

/// 行首字节偏移（`pos` 所在行）。
fn line_start(source: &str, pos: usize) -> usize {
    source
        .get(..pos)
        .and_then(|head| head.rfind('\n'))
        .map_or(0, |i| i + 1)
}

/// 行尾**含换行**的字节偏移（`pos` 所在行）。
fn line_end_inclusive(source: &str, pos: usize) -> usize {
    source
        .get(pos..)
        .and_then(|tail| tail.find('\n'))
        .map_or(source.len(), |i| pos + i + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_document_is_unchanged() {
        // 无脚注必须逐字节不变（各格式的既有回归测试依赖这一点）
        let src = "# 标题\n\n普通段落，含 $x^2$。\n";
        assert_eq!(prepare(src), src);
    }

    #[test]
    fn references_are_numbered_by_first_appearance_and_defs_move_to_end() {
        let src = "见[^b]与[^a]。\n\n[^a]: A\n[^b]: B\n";
        assert_eq!(prepare(src), "见[^1]与[^2]。\n\n[^1]: B\n\n[^2]: A\n");
    }

    #[test]
    fn repeated_reference_uses_same_number() {
        let src = "甲[^note] 乙[^note]\n\n[^note]: N\n";
        assert_eq!(prepare(src), "甲[^1] 乙[^1]\n\n[^1]: N\n");
    }

    #[test]
    fn unreferenced_definition_is_collected_after_referenced() {
        let src = "见[^a]\n\n[^b]: B\n[^a]: A\n";
        assert_eq!(prepare(src), "见[^1]\n\n[^1]: A\n\n[^2]: B\n");
    }

    #[test]
    fn continuation_line_moves_with_definition() {
        let src = "见[^a]\n\n[^a]: first\n    续行\n";
        assert_eq!(prepare(src), "见[^1]\n\n[^1]: first\n    续行\n");
    }

    #[test]
    fn nested_definition_stays_in_place_but_is_renumbered() {
        // 引用内的定义块不移动（保结构），只改标签
        let src = "> [^a]: A\n\n见[^a]\n";
        let out = prepare(src);
        assert!(out.starts_with("> [^1]: A"), "{out}");
        assert!(out.contains("见[^1]"), "{out}");
    }

    #[test]
    fn code_block_literal_is_not_renumbered() {
        // 代码块里的 `[^a]` 是字面量，不能当成脚注片段改写
        let src = "```\n[^a]\n```\n\n见[^a]\n\n[^a]: A\n";
        let out = prepare(src);
        assert!(out.contains("```\n[^a]\n```"), "{out}");
        assert!(out.contains("见[^1]"), "{out}");
    }
}
