//! 输入意图 → 编辑命令（`IntentMapper`，SPEC §6.1、§11.6）。
//!
//! 键盘/触摸产生的是**意图**（如"回车"），而非直接文本。`IntentMapper` 依据当前文档
//! 上下文把意图翻译成一组 [`Edit`]，例如：在列表项里回车自动续行并递增序号、在空列表
//! 项上回车退出列表。UI 只需把意图交给 mapper，再 `apply` 结果即可。

use crate::Document;
use crate::edit::{BlockTarget, Edit, InlineMark};
use std::ops::Range;

/// 用户输入意图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    /// 输入文本（可覆盖选区）。
    ///
    /// `range` 为当前选区：空区间表示纯插入（光标），非空则表示"用 `text`
    /// 替换选区内容"——这是"选中后打字"的标准语义。
    InsertText {
        /// 选区（源字节范围）；空区间 = 纯插入点。
        range: Range<usize>,
        /// 要输入的文本。
        text: String,
    },
    /// 回车换行（在列表项中续行、空项则退出列表）。
    Newline {
        /// 光标位置（源字节偏移）。
        at: usize,
    },
    /// 切换行内强调。
    ToggleInline {
        /// 选区。
        range: Range<usize>,
        /// 强调种类。
        mark: InlineMark,
    },
    /// 切换块样式。
    ToggleBlock {
        /// 目标块行范围。
        range: Range<usize>,
        /// 目标样式。
        target: BlockTarget,
    },
    /// 缩进 / 反缩进（Tab / Shift+Tab）：对选区覆盖的整块行统一加/删两空格。
    /// 产出**单个** `Replace`（原子操作，只产生一条 undo 记录）。
    Indent {
        /// 选区（起止任意，内部会扩展到整行边界）。
        range: Range<usize>,
        /// `true` 为反缩进（删除前导空格）。
        unindent: bool,
    },
    /// 智能退格（Backspace）：光标位于空的中文/英文配对符号之间时**成对删除**。
    ///
    /// 不再额外触发"在列表项开头退格先删缩进"等复合行为——那类行为交由
    /// UI 组合多个意图完成，保持 mapper 单一职责。
    DeletePair {
        /// 光标位置（成对符号之间的源字节偏移）。
        at: usize,
    },
    /// 删除选区内容（Delete / Backspace 且有选区时）。
    DeleteSelection {
        /// 选区（源字节范围）。
        range: Range<usize>,
    },
}

/// 把输入意图翻译为编辑命令。
pub trait IntentMapper {
    /// 返回实现该意图所需的编辑序列（可能为空）。
    fn map(&self, intent: &Intent, doc: &Document) -> Vec<Edit>;
}

/// 默认意图映射器。
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultIntentMapper;

impl IntentMapper for DefaultIntentMapper {
    fn map(&self, intent: &Intent, doc: &Document) -> Vec<Edit> {
        match intent {
            Intent::InsertText { range, text } => {
                if range.is_empty() {
                    // 空选区 = 纯插入（不在选区上做替换）
                    vec![Edit::Insert {
                        at: range.start,
                        text: text.clone(),
                    }]
                } else {
                    // 有选区 = 覆盖输入
                    vec![Edit::Replace {
                        range: range.clone(),
                        text: text.clone(),
                    }]
                }
            }
            Intent::DeleteSelection { range } => vec![Edit::Delete {
                range: range.clone(),
            }],
            Intent::Newline { at } => vec![newline_edit(&doc.text(), *at)],
            Intent::ToggleInline { range, mark } => vec![Edit::ToggleInline {
                range: range.clone(),
                mark: *mark,
            }],
            Intent::ToggleBlock { range, target } => vec![Edit::ToggleBlock {
                range: range.clone(),
                target: target.clone(),
            }],
            Intent::Indent { range, unindent } => {
                indent_edit(&doc.text(), range.clone(), *unindent)
            }
            Intent::DeletePair { at } => delete_pair_edit(&doc.text(), *at),
        }
    }
}

/// 缩进/反缩进：把选区扩展到整行后，对每行加/删前导两空格（或一个制表符）。
///
/// 返回空 `Vec` 表示无需改动（例如反缩进时没有任何行有前导空格）。
fn indent_edit(source: &str, range: Range<usize>, unindent: bool) -> Vec<Edit> {
    let start = line_start_of(source, range.start);
    let end = line_end_of(source, range.end);
    if start >= end {
        return Vec::new();
    }
    let region = source.get(start..end).unwrap_or_default();

    let mut out = String::with_capacity(region.len() + 8);
    let mut changed = false;
    for (i, line) in region.split('\n').enumerate() {
        // 兼容 Windows 换行：剥离行尾 \r 后再处理，结束时补回
        let (body, eol) = match line.strip_suffix('\r') {
            Some(b) => (b, "\r"),
            None => (line, ""),
        };
        let rewritten = if unindent {
            // 优先删两个空格；单个制表符也视为一级缩进
            body.strip_prefix("  ")
                .or_else(|| body.strip_prefix('\t'))
                .map_or_else(|| format!("{body}{eol}"), |rest| format!("{rest}{eol}"))
        } else {
            format!("  {body}{eol}")
        };
        if rewritten != line {
            changed = true;
        }
        out.push_str(&rewritten);
        if i + 1 < region.split('\n').count() {
            out.push('\n');
        }
    }
    if !changed {
        return Vec::new();
    }
    vec![Edit::Replace {
        range: start..end,
        text: out,
    }]
}

/// 光标所在行的行首偏移。
fn line_start_of(source: &str, at: usize) -> usize {
    source
        .get(..at)
        .and_then(|s| s.rfind('\n'))
        .map_or(0, |i| i + 1)
}

/// 光标所在行（或选区末行）的行尾偏移（不含换行符）。
fn line_end_of(source: &str, at: usize) -> usize {
    source
        .get(at..)
        .and_then(|s| s.find('\n'))
        .map_or(source.len(), |i| at + i)
}

/// 智能退格：`at` 前后恰好是一对**成对符号**（中间无内容）时，成对删除。
fn delete_pair_edit(source: &str, at: usize) -> Vec<Edit> {
    let (Some(before), Some(rest)) = (source.get(..at), source.get(at..)) else {
        return Vec::new();
    };
    let (Some(open), Some(close)) = (before.chars().next_back(), rest.chars().next()) else {
        return Vec::new();
    };
    if !is_pair(open, close) {
        return Vec::new();
    }
    let start = at - open.len_utf8();
    let end = at + close.len_utf8();
    vec![Edit::Replace {
        range: start..end,
        text: String::new(),
    }]
}

/// 是否为一对可以直接互相闭合的符号（退格时成对删除）。
fn is_pair(open: char, close: char) -> bool {
    matches!(
        (open, close),
        ('(', ')')
            | ('[', ']')
            | ('{', '}')
            | ('"', '"')
            | ('\'', '\'')
            | ('`', '`')
            | ('*', '*')
            | ('_', '_')
            | ('~', '~')
    )
}

/// 行首列表标记信息。
struct ListPrefix {
    /// 标记前的缩进字节数（嵌套列表）。
    indent: usize,
    /// 标记的字节长度（含其后的空格；不含缩进）。
    len: usize,
    /// 续行标记（含缩进；有序列表为递增后的序号）。
    next: String,
}

/// 计算回车编辑。
///
/// - 行首是列表标记且有内容 → 换行并续接标记（有序列表序号 +1）；
/// - 行首是列表标记但内容为空 → 删除标记（退出列表）；
/// - 其它 → 普通换行。
pub fn newline_edit(source: &str, at: usize) -> Edit {
    let line_start = source
        .get(..at)
        .and_then(|s| s.rfind('\n'))
        .map_or(0, |i| i + 1);
    let line = source.get(line_start..at).unwrap_or_default();
    // 先剥掉引用前缀（`> ` / `> > `），列表可能嵌套在引用里
    let (quote_len, body) = split_quote_prefix(line);
    match list_prefix(body) {
        Some(prefix)
            if body
                .get(prefix.indent + prefix.len..)
                .is_none_or(|rest| rest.trim().is_empty()) =>
        {
            Edit::Replace {
                // 空列表项：只去掉标记（保留引用前缀），退回收引用里的空行
                range: line_start + quote_len..at,
                text: String::new(),
            }
        }
        Some(prefix) => Edit::Insert {
            at,
            text: format!("\n{}{}", &line[..quote_len], prefix.next),
        },
        None => Edit::Insert {
            at,
            text: "\n".to_string(),
        },
    }
}

/// 剥离行首的引用前缀（支持嵌套 `> > `，最多 3 个前导空格）。
///
/// 返回 `(前缀字节数, 剩余内容)`；前缀原样保留，便于续行时补回相同的引用层级。
fn split_quote_prefix(line: &str) -> (usize, &str) {
    let bytes = line.as_bytes();
    let mut start = 0usize;
    loop {
        let mut j = start;
        let mut spaces = 0;
        while spaces < 3 && bytes.get(j) == Some(&b' ') {
            j += 1;
            spaces += 1;
        }
        if bytes.get(j) != Some(&b'>') {
            break;
        }
        j += 1;
        if matches!(bytes.get(j), Some(b' ' | b'\t')) {
            j += 1;
        }
        start = j;
    }
    (start, &line[start..])
}

/// 识别行首列表标记（`- `/`* `/`+ `/`1. `，可选任务框 `[ ] `）。
///
/// 允许标记前有空格/制表符（嵌套列表）：续行时**保留相同缩进**，否则在嵌套项上
/// 回车会突然跳到顶层。`len` 仍只计到标记+任务框（不含缩进），供调用方判断
/// "标记之后是否还有内容"。
fn list_prefix(line: &str) -> Option<ListPrefix> {
    let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
    let rest = &line[indent..];
    let bytes = rest.as_bytes();
    let (mut base, next): (usize, String) = if matches!(bytes.first(), Some(b'-' | b'*' | b'+')) {
        (1, "- ".to_string())
    } else {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        // 与扫描器保持一致：有序标记接受 `.` / `)`，且其后必须是空白或行尾
        let delim = match bytes.get(digits) {
            Some(b'.') => '.',
            Some(b')') => ')',
            _ => return None,
        };
        if !matches!(bytes.get(digits + 1), None | Some(b' ' | b'\t')) {
            return None;
        }
        let n = rest[..digits].parse::<u64>().unwrap_or(1);
        (digits + 1, format!("{}{delim} ", n + 1))
    };
    // 跳过标记后的空白（与扫描器一致；行尾 `-`/`1.` 也算列表标记）
    while matches!(bytes.get(base), Some(b' ' | b'\t')) {
        base += 1;
    }
    // 任务框（`[ ] ` / `[x] `）作为标记一部分，续行时补回未勾选框
    let task = ["[ ] ", "[x] ", "[X] "]
        .iter()
        .find(|t| rest[base..].starts_with(**t))
        .map_or(0, |t| t.len());
    let next = if task > 0 {
        format!("{next}[ ] ")
    } else {
        next
    };
    Some(ListPrefix {
        indent,
        len: base + task,
        next: format!("{}{next}", &line[..indent]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Document;

    fn mapper() -> DefaultIntentMapper {
        DefaultIntentMapper
    }

    fn apply(doc: &mut Document, edits: Vec<Edit>) {
        for e in edits {
            doc.apply(e).unwrap();
        }
    }

    #[test]
    fn insert_text_passthrough() {
        let doc = Document::new("ab");
        let edits = mapper().map(
            &Intent::InsertText {
                range: 1..1,
                text: "X".into(),
            },
            &doc,
        );
        assert_eq!(
            edits,
            vec![Edit::Insert {
                at: 1,
                text: "X".into()
            }]
        );
    }

    #[test]
    fn insert_text_over_selection_replaces() {
        let mut doc = Document::new("hello world");
        let edits = mapper().map(
            &Intent::InsertText {
                range: 6..11, // 选中 "world"
                text: "世界".into(),
            },
            &doc,
        );
        apply(&mut doc, edits);
        assert_eq!(doc.text(), "hello 世界");
    }

    #[test]
    fn insert_text_over_selection_is_atomic() {
        let mut doc = Document::new("abcdef");
        let edits = mapper().map(
            &Intent::InsertText {
                range: 1..4, // 选中 "bcd"
                text: "Z".into(),
            },
            &doc,
        );
        // 单条 Replace → 一次 undo 即可还原
        assert_eq!(edits.len(), 1);
        apply(&mut doc, edits);
        assert_eq!(doc.text(), "aZef");
        doc.undo().unwrap();
        assert_eq!(doc.text(), "abcdef");
    }

    #[test]
    fn delete_selection_removes_range() {
        let mut doc = Document::new("hello world");
        let edits = mapper().map(
            &Intent::DeleteSelection {
                range: 5..11, // " world"
            },
            &doc,
        );
        assert_eq!(edits, vec![Edit::Delete { range: 5..11 }]);
        apply(&mut doc, edits);
        assert_eq!(doc.text(), "hello");
    }

    #[test]
    fn newline_continues_unordered_list() {
        let mut doc = Document::new("- a");
        let edits = mapper().map(&Intent::Newline { at: 3 }, &doc);
        apply(&mut doc, edits);
        assert_eq!(doc.text(), "- a\n- ");
    }

    #[test]
    fn newline_increments_ordered_list() {
        let mut doc = Document::new("3. a");
        let edits = mapper().map(&Intent::Newline { at: 4 }, &doc);
        apply(&mut doc, edits);
        assert_eq!(doc.text(), "3. a\n4. ");
    }

    #[test]
    fn newline_on_empty_item_exits_list() {
        let mut doc = Document::new("- a\n- ");
        let edits = mapper().map(&Intent::Newline { at: 6 }, &doc);
        apply(&mut doc, edits);
        assert_eq!(doc.text(), "- a\n");
    }

    #[test]
    fn newline_plain_paragraph() {
        let mut doc = Document::new("hello");
        let edits = mapper().map(&Intent::Newline { at: 5 }, &doc);
        apply(&mut doc, edits);
        assert_eq!(doc.text(), "hello\n");
    }

    #[test]
    fn newline_continues_ordered_paren() {
        let mut doc = Document::new("1) a");
        let edits = mapper().map(&Intent::Newline { at: 4 }, &doc);
        apply(&mut doc, edits);
        assert_eq!(doc.text(), "1) a\n2) ");
    }

    #[test]
    fn newline_continues_ordered_dot() {
        let mut doc = Document::new("9. a");
        let edits = mapper().map(&Intent::Newline { at: 4 }, &doc);
        apply(&mut doc, edits);
        assert_eq!(doc.text(), "9. a\n10. ");
    }

    #[test]
    fn newline_continues_quoted_list() {
        let mut doc = Document::new("> - a");
        let edits = mapper().map(&Intent::Newline { at: 5 }, &doc);
        apply(&mut doc, edits);
        assert_eq!(doc.text(), "> - a\n> - ");
    }

    #[test]
    fn newline_continues_nested_quoted_task() {
        let mut doc = Document::new("> > 1. a");
        let edits = mapper().map(&Intent::Newline { at: 8 }, &doc);
        apply(&mut doc, edits);
        assert_eq!(doc.text(), "> > 1. a\n> > 2. ");
    }

    #[test]
    fn newline_on_empty_quoted_item_keeps_quote_prefix() {
        let mut doc = Document::new("> - a\n> - ");
        let edits = mapper().map(&Intent::Newline { at: 10 }, &doc);
        apply(&mut doc, edits);
        assert_eq!(doc.text(), "> - a\n> ");
    }

    #[test]
    fn newline_continues_nested_list_with_indent() {
        let mut doc = Document::new("- a\n  - b");
        let edits = mapper().map(&Intent::Newline { at: 9 }, &doc);
        apply(&mut doc, edits);
        assert_eq!(doc.text(), "- a\n  - b\n  - ");
    }

    #[test]
    fn newline_continues_nested_task_with_indent() {
        let mut doc = Document::new("1. a\n   - [x] b");
        let edits = mapper().map(&Intent::Newline { at: 15 }, &doc);
        apply(&mut doc, edits);
        assert_eq!(doc.text(), "1. a\n   - [x] b\n   - [ ] ");
    }

    #[test]
    fn newline_on_empty_nested_item_exits_list() {
        let mut doc = Document::new("- a\n  - ");
        let edits = mapper().map(&Intent::Newline { at: 8 }, &doc);
        apply(&mut doc, edits);
        assert_eq!(doc.text(), "- a\n");
    }

    #[test]
    fn newline_continues_task_item() {
        let mut doc = Document::new("- [x] done");
        let edits = mapper().map(&Intent::Newline { at: 10 }, &doc);
        apply(&mut doc, edits);
        assert_eq!(doc.text(), "- [x] done\n- [ ] ");
    }

    #[test]
    fn toggle_intents_delegate() {
        let doc = Document::new("x");
        let edits = mapper().map(
            &Intent::ToggleInline {
                range: 0..1,
                mark: InlineMark::Strong,
            },
            &doc,
        );
        assert_eq!(
            edits,
            vec![Edit::ToggleInline {
                range: 0..1,
                mark: InlineMark::Strong
            }]
        );
    }

    #[test]
    fn indent_single_line_is_atomic_replace() {
        let mut doc = Document::new("hello");
        let edits = mapper().map(
            &Intent::Indent {
                range: 2..3,
                unindent: false,
            },
            &doc,
        );
        // 单个 Replace 覆盖整行（原子操作 → 一条 undo）
        assert_eq!(edits.len(), 1);
        assert!(matches!(&edits[0], Edit::Replace { text, .. } if text == "  hello"));
        apply(&mut doc, edits);
        assert_eq!(doc.text(), "  hello");
    }

    #[test]
    fn indent_multiple_lines_at_once() {
        let mut doc = Document::new("a\nb\nc\n");
        // 选区从第一行中间到第三行中间 → 三行都缩进
        let edits = mapper().map(
            &Intent::Indent {
                range: 1..5,
                unindent: false,
            },
            &doc,
        );
        apply(&mut doc, edits);
        assert_eq!(doc.text(), "  a\n  b\n  c\n");
        // 原子性：只产生一条编辑记录
        assert!(doc.can_undo());
    }

    #[test]
    fn unindent_removes_up_to_two_spaces() {
        let mut doc = Document::new("    code\n");
        let edits = mapper().map(
            &Intent::Indent {
                range: 4..6,
                unindent: true,
            },
            &doc,
        );
        apply(&mut doc, edits);
        // 每次只删一级（两个空格）
        assert_eq!(doc.text(), "  code\n");
    }

    #[test]
    fn unindent_tab_is_one_level() {
        let mut doc = Document::new("\tcode\n");
        let edits = mapper().map(
            &Intent::Indent {
                range: 1..3,
                unindent: true,
            },
            &doc,
        );
        apply(&mut doc, edits);
        assert_eq!(doc.text(), "code\n");
    }

    #[test]
    fn unindent_noop_when_no_leading_space() {
        let doc = Document::new("plain\n- item\n");
        let edits = mapper().map(
            &Intent::Indent {
                range: 0..6,
                unindent: true,
            },
            &doc,
        );
        assert!(edits.is_empty(), "没有前导空格时不应产生编辑");
    }

    #[test]
    fn unindent_only_touches_selected_lines() {
        let mut doc = Document::new("  a\n  b\n");
        // 选区只覆盖第二行：第一行保持不动
        let edits = mapper().map(
            &Intent::Indent {
                range: 5..5,
                unindent: true,
            },
            &doc,
        );
        apply(&mut doc, edits);
        assert_eq!(doc.text(), "  a\nb\n");
    }

    #[test]
    fn delete_pair_removes_both_chars() {
        let doc = Document::new("(x)");
        // 光标在 "(" 与 "x" 之间 → 不是空对，不删
        let edits = mapper().map(&Intent::DeletePair { at: 1 }, &doc);
        assert!(edits.is_empty());

        let mut doc = Document::new("()");
        let edits = mapper().map(&Intent::DeletePair { at: 1 }, &doc);
        apply(&mut doc, edits);
        assert_eq!(doc.text(), "");
    }

    #[test]
    fn delete_pair_marks_and_quotes() {
        for (open, close) in [
            ('[', ']'),
            ('{', '}'),
            ('"', '"'),
            ('\'', '\''),
            ('`', '`'),
            ('*', '*'),
            ('~', '~'),
        ] {
            let src = format!("{open}{close}");
            let mut doc = Document::new(&src);
            let edits = mapper().map(&Intent::DeletePair { at: 1 }, &doc);
            apply(&mut doc, edits);
            assert_eq!(doc.text(), "", "{open}{close} 应被成对删除");
        }
    }

    #[test]
    fn delete_pair_unmatched_is_noop() {
        let doc = Document::new("(]");
        let edits = mapper().map(&Intent::DeletePair { at: 1 }, &doc);
        assert!(edits.is_empty(), "不匹配的符号不应成对删除");
    }

    #[test]
    fn delete_pair_at_start_is_noop() {
        let doc = Document::new(")");
        let edits = mapper().map(&Intent::DeletePair { at: 0 }, &doc);
        assert!(edits.is_empty());
    }
}
