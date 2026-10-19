//! 语法片段：把源文本切成带角色的字节区间（SPEC §5）。
//!
//! WYSIWYG 的核心动作是"隐藏语法标记、只渲染文本"。因此解析器不只给出结构，
//! 还把每一块源文本切成若干 [`SyntaxSpan`]：**可见文本** 与 **语法标记**，
//! 二者的分界即 [`crate::Block`] 投影成显示文本时的裁剪依据（配合 `mazha-core`
//! 的 `OffsetMap`：把 Marker 区间当作隐藏 gap）。

use std::ops::Range;

/// 片段种类：区分"渲染可见的文本"与"仅用于语法、渲染时隐藏的标记"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanKind {
    /// 可见文本（渲染、可编辑）。
    Text,
    /// 语法标记（渲染时隐藏，如 `**`、`#`、`-`）。
    Marker,
    /// 内联对象占位（图片/公式/图表；源文本隐藏，由 UI 渲染）。Phase 3。
    Widget,
}

/// 片段角色：决定渲染样式与标记用途。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanRole {
    /// 普通文本。
    Normal,
    /// 斜体文本（`*x*`）。
    Emphasis,
    /// 粗体文本（`**x**`）。
    Strong,
    /// 删除线文本（`~~x~~`）。
    Strikethrough,
    /// 行内代码文本。
    InlineCode,
    /// 链接显示文本。
    LinkText,
    /// 链接的 `[` `](` `)` 标记。
    LinkMarker,
    /// 链接目标 URL。
    LinkDest,
    /// 图片 `![` `](` `)` 标记。
    ImageMarker,
    /// 标题 `#` 前缀。
    HeadingMarker,
    /// 引用 `>` 前缀。
    QuoteMarker,
    /// 提示块标记 `[!NOTE]`（渲染时隐藏）。
    CalloutMarker,
    /// Wiki-link 标记 `[[` / `]]` / `|`（渲染时隐藏）。
    WikiLinkMarker,
    /// Wiki-link 目标页名（无别名时可见，有别名时隐藏）。
    WikiLinkTarget,
    /// Wiki-link 显示别名（`[[Page|Alias]]` 的 Alias，可见）。
    WikiLinkText,
    /// 标签 `#tag`（可见）。
    Tag,
    /// 列表项 `-`/`1.` 前缀。
    ListMarker,
    /// 任务列表复选框 `[ ]`/`[x]`。
    TaskMarker,
    /// 代码围栏行（``` 或 ~~~）。
    FenceMarker,
    /// 代码块内容。
    CodeContent,
    /// 分割线整行（`---`）。
    ThematicBreak,
    /// 表格单元格之间的竖线（渲染时**保留**：投影没有分栏布局，竖线有助于可读）。
    TablePipe,
    /// 表格分隔行（`| --- | :-: |`，渲染时隐藏）。
    TableDelim,
    /// 软换行（同一段落内的换行，渲染时视为空格/隐藏）。
    SoftBreak,
    /// 硬换行（行尾两空格或反斜杠）。
    HardBreak,
    /// 转义反斜杠。
    EscapeMarker,
    /// 行内公式 `$` 定界符（渲染时隐藏，D34）。
    MathMarker,
    /// 行内/块级公式体（TeX 源文本，**可见**，D34）。
    ///
    /// 可见的理由：公式尚未接入真排版（`ratex-*`），若整段当 Widget 隐藏，
    /// 编辑区会凭空"丢字"；保留可见文本由 UI 用 `RunStyle::Math` 标注。
    Math,
}

/// 覆盖源文本一段字节区间的语法片段；区间端点必为字符边界。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxSpan {
    /// 片段在源文本中的字节范围。
    pub range: Range<usize>,
    /// 片段种类（可见/标记/占位）。
    pub kind: SpanKind,
    /// 片段角色。
    pub role: SpanRole,
}

impl SyntaxSpan {
    /// 构造一个"可见文本"片段。
    pub fn text(range: Range<usize>, role: SpanRole) -> Self {
        Self {
            range,
            kind: SpanKind::Text,
            role,
        }
    }

    /// 构造一个"语法标记"片段。
    pub fn marker(range: Range<usize>, role: SpanRole) -> Self {
        Self {
            range,
            kind: SpanKind::Marker,
            role,
        }
    }

    /// 是否为可见文本（会被渲染）。
    pub fn is_text(&self) -> bool {
        matches!(self.kind, SpanKind::Text)
    }

    /// 字节范围长度。
    pub fn len(&self) -> usize {
        self.range.end.saturating_sub(self.range.start)
    }

    /// 是否为空片段。
    pub fn is_empty(&self) -> bool {
        self.range.start >= self.range.end
    }
}

/// 从片段集合中取出"可见文本"区间（升序），供 `OffsetMap` 计算隐藏 gap。
///
/// 空片段与非 Text 片段会被过滤；结果按起点升序。
pub fn visible_runs(spans: &[SyntaxSpan]) -> Vec<Range<usize>> {
    let mut runs: Vec<Range<usize>> = spans
        .iter()
        .filter(|s| s.is_text() && !s.is_empty())
        .map(|s| s.range.clone())
        .collect();
    runs.sort_by_key(|r| r.start);
    runs
}
