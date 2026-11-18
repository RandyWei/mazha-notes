//! 导出前的脚注规整（决策 D69）。
//!
//! WYSIWYG 编辑器保留脚注标签原文（`[^label]`），以便直接编辑；导出时则按
//! 「引用首次出现顺序」编号，并把定义块**归集到文末**。两种视图的差异集中在这里
//! 通过**一次源文本重写**完成，各格式的渲染器（HTML/Typst/DOCX/EPUB）因此不必
//! 各自理解脚注编号：
//!
//! 1. `renumber`：把引用与定义头部的标签文本替换成序号（如 `[^note]` → `[^1]`）；
//!    只改写 `SpanRole::FootnoteRef` 片段，代码块/公式等字面量不受影响。
//! 2. `move_definitions`：把**所有**脚注定义块（含引用/列表内的嵌套定义）整块移到
//!    文末并按序号排序；嵌套定义先剥离容器前缀（`> `/列表标记/缩进），保证归集后
//!    仍是合法的顶层定义块（D81）。
//!
//! 保留的限制（写入 SPEC D69/D81）：
//! - 重复定义的标签都改成同一序号，位置按序号稳定排序；
//! - 嵌套定义的容器前缀只识别空格与 `>` / 列表标记，tab 或混合前缀保留原样；
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

/// 把所有脚注定义块（含嵌套在引用/列表内的）移到文末，并按序号升序排列。
///
/// D81 起不再只处理顶层块：嵌套定义原先虽被 `renumber` 编号但留在容器内，无法在
/// 统一文末脚注区阅读。这里递归收集全部定义块，剥离容器前缀后归集到文末。
fn move_definitions(source: &str) -> String {
    let blocks = parse(source);
    let mut defs: Vec<(usize, &Block)> = Vec::new();
    collect_defs(&blocks, &mut defs);
    if defs.is_empty() {
        return source.to_string();
    }

    // 先按序号排序取出（已剥离前缀的）文本；此时源文本尚未删除，区间仍然有效。
    defs.sort_by_key(|(number, _)| *number);
    let texts: Vec<String> = defs
        .iter()
        .filter_map(|(_, block)| cleaned_def_text(source, block))
        .collect();

    // 删除必须按**源偏移倒序**（与序号顺序无关），否则先删前面的定义会让后面区间失效。
    let mut cuts: Vec<Range<usize>> = defs.iter().map(|(_, b)| b.range.clone()).collect();
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

/// 深度优先收集所有脚注定义块（顶层 + 嵌套在引用/列表内的），附重编号后的序号。
fn collect_defs<'a>(blocks: &'a [Block], out: &mut Vec<(usize, &'a Block)>) {
    for block in blocks {
        if let BlockKind::FootnoteDef { label } = &block.kind {
            out.push((label.parse::<usize>().unwrap_or(usize::MAX), block));
        }
        collect_defs(&block.children, out);
    }
}

/// 取定义块源文本并剥离嵌套容器前缀，使其归集后仍能被解析为顶层定义块。
///
/// 首行 `[^` 之前的部分即容器前缀（如 `> `、`- `、`  - ` 或纯缩进）。续行若以
/// 同一前缀开头就整段剥掉；列表项续行只有缩进、没有列表标记，此时剥掉与前缀
/// 等宽的前导空格。**边界**：只按字节剥空格/`>`/列表标记，tab 或混合前缀不识别，
/// 保留原样（退化为就地渲染，不破坏结构）。
fn cleaned_def_text(source: &str, block: &Block) -> Option<String> {
    let text = source.get(block.range.clone())?;
    let first_line = text.split('\n').next().unwrap_or("");
    let prefix = first_line.get(..first_line.find("[^")?)?;
    if prefix.is_empty() {
        return Some(text.to_string());
    }
    let prefix_len = prefix.len();
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if i == 0 || line.starts_with(prefix) {
            // 首行必带完整前缀；引用内的续行也带同样的 `> ` 前缀。
            out.push_str(&line[prefix_len..]);
        } else {
            // 列表项续行只有缩进（宽度 == 列表标记宽度），剥掉同宽空格后
            // 恰好留下脚注定义续行需要的 4 空格缩进。
            let spaces = line
                .bytes()
                .take_while(|b| *b == b' ')
                .count()
                .min(prefix_len);
            out.push_str(&line[spaces..]);
        }
    }
    Some(out)
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
    fn nested_quote_definition_is_collected_with_prefix_stripped() {
        // D81：引用内的定义块剥掉 `> ` 前缀后归集文末（D69 仅重编号、留在原地）
        let src = "> [^a]: A\n\n见[^a]\n";
        let out = prepare(src);
        assert!(out.trim_start().starts_with("见[^1]"), "{out}");
        assert!(out.trim_end().ends_with("[^1]: A"), "{out}");
        assert!(!out.contains("> [^1]"), "{out}");
    }

    #[test]
    fn nested_list_definition_keeps_four_space_continuation_indent() {
        // 列表项续行只有缩进（列表标记宽度 + 4），剥掉标记宽度后须留下 4 空格定义缩进
        let src = "- [^a]: first\n      续行\n\n见[^a]\n";
        let out = prepare(src);
        assert!(out.trim_end().ends_with("[^1]: first\n    续行"), "{out}");
        assert!(!out.contains("- [^1]"), "{out}");
    }

    #[test]
    fn nested_and_top_level_definitions_are_sorted_by_number() {
        // 引用顺序 b → a；一个嵌套、一个顶层，归集后按序号统一排序
        let src = "> [^b]: B\n\n见[^b]与[^a]。\n\n[^a]: A\n";
        let out = prepare(src);
        assert!(out.trim_end().ends_with("[^1]: B\n\n[^2]: A"), "{out}");
        assert!(!out.contains("> [^1]"), "{out}");
    }

    #[test]
    fn cleaned_def_text_handles_quote_and_plain_prefixes() {
        // 纯函数：引用前缀与续行剥离、顶层缩进剥离
        let mk = |range: Range<usize>| {
            Block::leaf(BlockKind::FootnoteDef { label: "1".into() }, range, vec![])
        };
        let src = "> [^1]: A\n>     cont\n";
        let block = mk(0..src.len());
        assert_eq!(
            cleaned_def_text(src, &block).as_deref(),
            Some("[^1]: A\n    cont\n")
        );

        let src = "  [^1]: B\n";
        let block = mk(0..src.len());
        assert_eq!(cleaned_def_text(src, &block).as_deref(), Some("[^1]: B\n"));
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
