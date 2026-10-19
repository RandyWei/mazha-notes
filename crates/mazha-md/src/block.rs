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
    /// 提示块（容器，`> [!NOTE]` 语法）。
    Callout {
        /// 提示块类型。
        kind: CalloutKind,
        /// 自定义标题（`> [!NOTE] 自定义标题` 时非空，否则用默认标题）。
        title: Option<String>,
    },
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
    /// Front-matter（文档开头 `---` YAML `---`；`content` 为原始 YAML 文本）。
    FrontMatter {
        /// 原始 YAML 文本（不含分隔线）。
        content: String,
    },
    /// GFM 表格（表头行 + 分隔行 + 若干数据行）。
    Table {
        /// 每列的对齐方式（列数 = 该数组长度）。
        align: Vec<Align>,
    },
    /// 显示公式块（`$$…$$`，可跨行；D34）。
    ///
    /// 公式体记 `SpanRole::Math`（可见），两端 `$$` 记 `SpanRole::MathMarker`（隐藏）。
    /// 行内公式 `$…$` 不产生块，只是段落里的片段。
    MathBlock,
    /// 脚注定义块（行首 `[^label]: body`，缩进 ≥4 的续行归入本块；D35）。
    ///
    /// `label` 是脚注标签（引用 `[^label]` 与之对应）。片段：`[^` 与 `]:` 记
    /// `SpanRole::FootnoteMarker`（隐藏）、label 记 `SpanRole::FootnoteRef`（可见）、
    /// 正文按行内语法解析。行内引用 `[^label]` 不产生块，只是段落里的片段。
    FootnoteDef {
        /// 脚注标签（不含 `[^` 与 `]`）。
        label: String,
    },
}

/// 表格列对齐方式（GFM 分隔行 `:--` / `:-:` / `--:` 决定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    /// 左对齐（默认）。
    Left,
    /// 居中。
    Center,
    /// 右对齐。
    Right,
}

/// 提示块类型（GitHub 五种 + 自定义）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CalloutKind {
    /// 说明（默认）。
    Note,
    /// 技巧。
    Tip,
    /// 重要。
    Important,
    /// 警告。
    Warning,
    /// 注意。
    Caution,
    /// 自定义类型名（如 `> [!BUG]`）。
    Custom(String),
}

impl CalloutKind {
    /// 从标记名解析（大小写不敏感）；未知名归入 [`Custom`](Self::Custom)。
    pub fn parse(name: &str) -> Self {
        match name.to_ascii_uppercase().as_str() {
            "NOTE" => Self::Note,
            "TIP" => Self::Tip,
            "IMPORTANT" => Self::Important,
            "WARNING" => Self::Warning,
            "CAUTION" => Self::Caution,
            _ => Self::Custom(name.to_string()),
        }
    }

    /// 标记名（用于序列化回 `[!NAME]`）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Note => "NOTE",
            Self::Tip => "TIP",
            Self::Important => "IMPORTANT",
            Self::Warning => "WARNING",
            Self::Caution => "CAUTION",
            Self::Custom(s) => s,
        }
    }
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
            BlockKind::BlockQuote
                | BlockKind::Callout { .. }
                | BlockKind::List { .. }
                | BlockKind::ListItem { .. }
        )
    }
}
