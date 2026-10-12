//! 编辑命令与变更摘要。
//!
//! `Edit` 是对源文本（Markdown 原文字符串）的一次原子修改，是 **UI 与核心之间
//! 唯一的修改通道**：任何键盘/触摸/命令面板操作，最终都要翻译成 `Edit` 再交给
//! `Document::apply`（SPEC §11.6）。这保证了渲染投影始终可重建、undo/redo 始终正确。

use std::ops::Range;

/// 行内强调种类（`ToggleInline` 的目标，SPEC §6.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineMark {
    /// 斜体（`*x*`）。
    Emphasis,
    /// 粗体（`**x**`）。
    Strong,
    /// 删除线（`~~x~~`）。
    Strikethrough,
    /// 行内代码（`` `x` ``）。
    Code,
}

impl InlineMark {
    /// 对应的 Markdown 定界串。
    pub fn marker(self) -> &'static str {
        match self {
            InlineMark::Emphasis => "*",
            InlineMark::Strong => "**",
            InlineMark::Strikethrough => "~~",
            InlineMark::Code => "`",
        }
    }
}

/// 块级样式目标（`ToggleBlock` 的目标，SPEC §6.1）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockTarget {
    /// 普通段落（去除块前缀）。
    Paragraph,
    /// 标题，级别 1..=6。
    Heading(u8),
    /// 引用。
    BlockQuote,
    /// 列表：`ordered` 决定有序/无序。
    List {
        /// 是否有序列表。
        ordered: bool,
    },
}

/// 对源文本的一次原子编辑。
///
/// 所有偏移量都是 **UTF-8 字节偏移**，且必须是字符边界，否则 `Document::apply`
/// 返回 [`crate::error::EditError::InvalidBoundary`]。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    /// 在字节偏移 `at` 处插入 `text`。
    Insert {
        /// 插入位置（字符边界）。
        at: usize,
        /// 要插入的文本。
        text: String,
    },

    /// 删除字节范围 `range` 内的文本。
    Delete {
        /// 待删除范围（两端都必须落在字符边界）。
        range: Range<usize>,
    },

    /// 用 `text` 替换字节范围 `range` 内的文本。
    Replace {
        /// 被替换的范围。
        range: Range<usize>,
        /// 新文本。
        text: String,
    },

    /// 包裹/取消选区内的行内强调。会在 `apply` 时解析成具体的 Insert/Replace。
    ToggleInline {
        /// 选区（源字节范围）。
        range: Range<usize>,
        /// 强调种类。
        mark: InlineMark,
    },

    /// 设置块级样式。会在 `apply` 时解析成具体的 Replace。
    ToggleBlock {
        /// 目标块的行范围（源字节范围）。
        range: Range<usize>,
        /// 目标样式。
        target: BlockTarget,
    },
}

impl Edit {
    /// 本次编辑在**修改前**文档中影响的源文本字节范围。
    ///
    /// 插入：`at..at+len`；删除/替换/切换：对应 `range`。供 undo/redo 与投影失效计算使用。
    pub fn source_range(&self) -> Range<usize> {
        match self {
            Edit::Insert { at, text } => *at..at.saturating_add(text.len()),
            Edit::Delete { range }
            | Edit::Replace { range, .. }
            | Edit::ToggleInline { range, .. }
            | Edit::ToggleBlock { range, .. } => range.clone(),
        }
    }
}

/// 一次编辑应用后的摘要，供 UI 决定哪些区域需要重解析/重绘。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeSummary {
    /// 应用后，**新文档**中内容发生变化（需要重解析/重绘）的字节范围。
    ///
    /// - 纯插入：`at..at+len`；
    /// - 纯删除：`start..start`（空区间，表示删除点）；
    /// - 替换：`start..start+new_len`。
    pub affected: Range<usize>,
    /// 应用后文档的字节长度。
    pub doc_len: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_range_all_variants() {
        assert_eq!(
            Edit::Insert {
                at: 0,
                text: "ab".into()
            }
            .source_range(),
            0..2
        );
        assert_eq!(Edit::Delete { range: 1..5 }.source_range(), 1..5);
        assert_eq!(
            Edit::Replace {
                range: 2..6,
                text: "x".into()
            }
            .source_range(),
            2..6
        );
    }
}
