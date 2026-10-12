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
    /// 插入文本。
    InsertText {
        /// 插入位置（源字节偏移）。
        at: usize,
        /// 文本内容。
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
            Intent::InsertText { at, text } => vec![Edit::Insert {
                at: *at,
                text: text.clone(),
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
        }
    }
}

/// 行首列表标记信息。
struct ListPrefix {
    /// 标记的字节长度（含其后的空格）。
    len: usize,
    /// 续行标记（有序列表为递增后的序号）。
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
    match list_prefix(line) {
        Some(prefix) if line[prefix.len..].trim().is_empty() => Edit::Replace {
            // 空列表项：去掉标记，退回普通段落行
            range: line_start..at,
            text: String::new(),
        },
        Some(prefix) => Edit::Insert {
            at,
            text: format!("\n{}", prefix.next),
        },
        None => Edit::Insert {
            at,
            text: "\n".to_string(),
        },
    }
}

/// 识别行首列表标记（`- `/`* `/`+ `/`1. `，可选任务框 `[ ] `）。
fn list_prefix(line: &str) -> Option<ListPrefix> {
    let (base, next): (usize, String) =
        if line.starts_with("- ") || line.starts_with("* ") || line.starts_with("+ ") {
            (2, "- ".to_string())
        } else {
            let digits = line.bytes().take_while(u8::is_ascii_digit).count();
            if digits == 0 || !line[digits..].starts_with(". ") {
                return None;
            }
            let n = line[..digits].parse::<u64>().unwrap_or(1);
            (digits + 2, format!("{}. ", n + 1))
        };
    // 任务框（`[ ] ` / `[x] `）作为标记一部分，续行时补回未勾选框
    let task = ["[ ] ", "[x] ", "[X] "]
        .iter()
        .find(|t| line[base..].starts_with(**t))
        .map_or(0, |t| t.len());
    let next = if task > 0 {
        format!("{next}[ ] ")
    } else {
        next
    };
    Some(ListPrefix {
        len: base + task,
        next,
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
                at: 1,
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
}
