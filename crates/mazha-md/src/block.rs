//! 块级结构（SPEC §5）。
//!
//! 解析结果是一棵 [`Block`] 森林：叶子块（段落/标题/代码块/分割线）的内容以
//! [`SyntaxSpan`] 表示；容器块（引用/列表/列表项）通过 `children` 承载嵌套子块。
//! 所有范围都是**源文本字节范围**，是脏块增量重解析与投影的最小单位。

use crate::span::SyntaxSpan;
use std::ops::Range;

/// 块类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockKind {
    /// 段落。
    Paragraph,
    /// ATX/Setext 标题，`level` ∈ 1..=6。
    Heading {
        /// 标题级别。
        level: u8,
    },
    /// 引用块（容器）。
    BlockQuote,
    /// 列表容器。
    List {
        /// 是否有序列表。
        ordered: bool,
        /// 有序列表起始序号（无序列表固定为 1）。
        start: u64,
    },
    /// 列表项（容器）。
    ListItem {
        /// 任务列表状态：`None` 非任务；`Some(true)` 已完成；`Some(false)` 未完成。
        task: Option<bool>,
    },
    /// 代码块（围栏或缩进）。
    CodeBlock {
        /// 围栏信息串中的语言标识（如 `rust`）。
        lang: Option<String>,
        /// 是否为围栏代码块（``` 包裹）而非缩进代码块。
        fenced: bool,
    },
    /// 分割线（`---` / `***` / `___`）。
    ThematicBreak,
}

/// 一个块。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// 块类型。
    pub kind: BlockKind,
    /// 覆盖整块（含标记与换行）的源字节范围。
    pub range: Range<usize>,
    /// 片段：块级标记 + 叶子内容（行内解析前，内容暂以单个 Text 片段占位）。
    pub spans: Vec<SyntaxSpan>,
    /// 容器子块（引用/列表/列表项）；叶子块为空。
    pub children: Vec<Block>,
}

impl Block {
    /// 新建叶子块。
    pub fn leaf(kind: BlockKind, range: Range<usize>, spans: Vec<SyntaxSpan>) -> Self {
        Self {
            kind,
            range,
            spans,
            children: Vec::new(),
        }
    }

    /// 新建容器块。
    pub fn container(
        kind: BlockKind,
        range: Range<usize>,
        spans: Vec<SyntaxSpan>,
        children: Vec<Block>,
    ) -> Self {
        Self {
            kind,
            range,
            spans,
            children,
        }
    }

    /// 是否为容器块（引用/列表/列表项）。
    pub fn is_container(&self) -> bool {
        matches!(
            self.kind,
            BlockKind::BlockQuote | BlockKind::List { .. } | BlockKind::ListItem { .. }
        )
    }
}
