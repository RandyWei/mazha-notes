//! 编辑命令的实现（SPEC §6.1、§11.6）。
//!
//! 把"用户意图"翻译成对**源文本**的具体 [`Edit`]（Insert/Delete/Replace）。命令本身
//! 是**纯函数**（只读源文本，返回要应用的编辑），由 `Document::apply` 解析 `ToggleInline`
//! / `ToggleBlock` 时调用。这样编辑逻辑可脱离 rope 单测，undo/redo 只需记录源级副作用。

use crate::edit::{BlockTarget, Edit, InlineMark};
use std::ops::Range;

/// 行内强调的包裹/解包裹。
///
/// - 选区两端若已被同种定界串包裹 → **解包裹**（删除两侧标记）；
/// - 空选区 → 插入一对标记（`****`），供 UI 将光标置于中间；
/// - 否则 → 用定界串包裹选区文本。
pub fn toggle_inline(source: &str, range: Range<usize>, mark: InlineMark) -> Edit {
    let marker = mark.marker();
    let a = range.start;
    let b = range.end;
    let before = source.get(..a).unwrap_or_default();
    let after = source.get(b..).unwrap_or_default();
    let selection = source.get(range).unwrap_or_default();

    let wrapped = a >= marker.len() && before.ends_with(marker) && after.starts_with(marker);
    if wrapped {
        // 解包裹：把两侧标记一并替换掉
        Edit::Replace {
            range: (a - marker.len())..(b + marker.len()),
            text: selection.to_string(),
        }
    } else if a == b {
        // 空选区：插入成对标记
        Edit::Insert {
            at: a,
            text: format!("{marker}{marker}"),
        }
    } else {
        Edit::Replace {
            range: a..b,
            text: format!("{marker}{selection}{marker}"),
        }
    }
}

/// 设置块级样式：逐行剥离旧块前缀，再套用目标前缀。
///
/// `range` 应是目标块的行范围（取自投影的 `ProjectedLine::source_range`，可跨多行）。
/// 空行保持为空；有序列表按出现顺序编号。
pub fn set_block(source: &str, range: Range<usize>, target: &BlockTarget) -> Edit {
    let body = source.get(range.clone()).unwrap_or_default();
    let lines: Vec<&str> = body.split('\n').collect();

    let mut out = String::with_capacity(body.len() + 8);
    let mut order = 0u64;
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if line.trim().is_empty() {
            out.push_str(line); // 空行原样保留（不套前缀）
            continue;
        }
        let stripped = strip_block_prefix(line);
        out.push_str(&block_prefix(target, order));
        out.push_str(stripped);
        order += 1;
    }

    Edit::Replace { range, text: out }
}

/// 目标块样式对应的行前缀（`order` 为当前有序项序号，从 0 起）。
fn block_prefix(target: &BlockTarget, order: u64) -> String {
    match target {
        BlockTarget::Paragraph => String::new(),
        BlockTarget::Heading(level) => {
            let n = (*level).clamp(1, 6);
            let mut s = "#".repeat(usize::from(n));
            s.push(' ');
            s
        }
        BlockTarget::BlockQuote => "> ".to_string(),
        BlockTarget::List { ordered: false } => "- ".to_string(),
        BlockTarget::List { ordered: true } => format!("{}. ", order + 1),
    }
}

/// 逐层剥离行首的引用 / 列表 / 标题前缀，返回纯内容。
fn strip_block_prefix(mut line: &str) -> &str {
    loop {
        if let Some(rest) = line.strip_prefix("> ") {
            line = rest;
            continue;
        }
        if let Some(rest) = line.strip_prefix("- ") {
            line = rest;
            continue;
        }
        if let Some(rest) = strip_ordered_marker(line) {
            line = rest;
            continue;
        }
        if let Some(rest) = strip_heading_marker(line) {
            line = rest;
            continue;
        }
        break;
    }
    line
}

/// 剥离有序列表标记（`1. `），返回剩余内容。
fn strip_ordered_marker(line: &str) -> Option<&str> {
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let rest = &line[digits..];
    rest.strip_prefix(". ")
}

/// 剥离标题标记（`#`×1..=6 后跟空格），返回剩余内容。
fn strip_heading_marker(line: &str) -> Option<&str> {
    let hashes = line.bytes().take_while(|&b| b == b'#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    line[hashes..].strip_prefix(' ')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(source: &str, edit: Edit) -> String {
        let r = edit.source_range();
        match edit {
            Edit::Insert { at, text } => {
                format!("{}{}{}", &source[..at], text, &source[at..])
            }
            Edit::Delete { .. } => format!("{}{}", &source[..r.start], &source[r.end..]),
            Edit::Replace { text, .. } => {
                format!("{}{}{}", &source[..r.start], text, &source[r.end..])
            }
            _ => unreachable!("命令只产出基础编辑"),
        }
    }

    #[test]
    fn inline_wrap_selection() {
        let src = "hello world";
        let e = toggle_inline(src, 6..11, InlineMark::Strong);
        assert_eq!(apply(src, e), "hello **world**");
    }

    #[test]
    fn inline_unwrap_when_already_wrapped() {
        let src = "hello **world**";
        // 选区指向 world（8..13），两侧已有 **
        let e = toggle_inline(src, 8..13, InlineMark::Strong);
        assert_eq!(apply(src, e), "hello world");
    }

    #[test]
    fn inline_empty_inserts_pair() {
        let src = "ab";
        let e = toggle_inline(src, 1..1, InlineMark::Emphasis);
        assert_eq!(apply(src, e), "a**b");
    }

    #[test]
    fn inline_code_marker() {
        let src = "x";
        let e = toggle_inline(src, 0..1, InlineMark::Code);
        assert_eq!(apply(src, e), "`x`");
    }

    #[test]
    fn block_set_heading() {
        let src = "title\nnext";
        let e = set_block(src, 0..5, &BlockTarget::Heading(2));
        assert_eq!(apply(src, e), "## title\nnext");
    }

    #[test]
    fn block_heading_strips_existing() {
        let src = "### old";
        let e = set_block(src, 0..7, &BlockTarget::Heading(1));
        assert_eq!(apply(src, e), "# old");
    }

    #[test]
    fn block_paragraph_removes_prefix() {
        let src = "## title";
        let e = set_block(src, 0..8, &BlockTarget::Paragraph);
        assert_eq!(apply(src, e), "title");
    }

    #[test]
    fn block_quote_multi_line() {
        let src = "a\nb";
        let e = set_block(src, 0..3, &BlockTarget::BlockQuote);
        assert_eq!(apply(src, e), "> a\n> b");
    }

    #[test]
    fn block_unordered_list_numbers_off_and_keeps_blank() {
        let src = "a\n\nb";
        let e = set_block(src, 0..4, &BlockTarget::List { ordered: false });
        assert_eq!(apply(src, e), "- a\n\n- b");
    }

    #[test]
    fn block_ordered_list_numbers_sequentially() {
        let src = "a\nb";
        let e = set_block(src, 0..3, &BlockTarget::List { ordered: true });
        assert_eq!(apply(src, e), "1. a\n2. b");
    }

    #[test]
    fn block_switch_list_to_quote() {
        let src = "1. a\n2. b";
        let e = set_block(src, 0..9, &BlockTarget::BlockQuote);
        assert_eq!(apply(src, e), "> a\n> b");
    }

    #[test]
    fn block_heading_level_clamped() {
        let src = "x";
        let e = set_block(src, 0..1, &BlockTarget::Heading(9));
        assert_eq!(apply(src, e), "###### x");
    }
}
