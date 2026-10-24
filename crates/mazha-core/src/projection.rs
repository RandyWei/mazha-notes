//! 渲染投影（`Projection`，SPEC §6.1）。
//!
//! 把"源文本 + `mazha-md` 解析出的块森林"编译成 UI 直接消费的**按行投影**：
//! 每一行给出块样式、带样式的可见文本片段（`TextRun`）、内联占位（`Widget`，如图片），
//! 以及把显示位置映射回源字节的 [`OffsetMap`]。语法标记（`#`、`**`、`` ` `` 等）在此被
//! 隐藏，UI 不再自己解析 Markdown。
//!
//! ## 为何按"逻辑行"投影
//! 文本布局、光标、命中测试都以行为单位，故每行一个 [`ProjectedLine`]。一行内的可见区间
//! 直接喂给 [`OffsetMap`]（隐藏区间=补集），即得该行的显示↔源双向映射。

use crate::offset_map::{DisplayPos, OffsetMap};
use crate::selection::Selection;
use mazha_md::{Align, Block, BlockKind, SpanKind, SpanRole, SyntaxSpan};
use std::ops::Range;

/// 整篇文档的渲染投影。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Projection {
    /// 按源文本逻辑行排列（含末尾空行，便于光标落点）。
    pub lines: Vec<ProjectedLine>,
    /// 块级 widget（代码块/表格/分割线…），按源序排列（SPEC §11.5、D30）。
    pub widgets: Vec<BlockWidget>,
}

impl Projection {
    /// 行数。
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// 是否为空投影。
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// 增量刷新投影（SPEC §6.1 "增量重建" 的投影侧）。
    ///
    /// `old_range` / `new_range` 是同一次编辑在**旧/新文本**中的字节范围（插入时
    /// `old_range` 为空区间）；`blocks` 是调用方已增量维护好的新块森林。
    ///
    /// 策略（正确性优先）：
    /// 1. 定位编辑在旧投影中的起止行 `[first, last]`；
    /// 2. 按"保留同样多的尾部行"反推新文本中的窗口末行 `new_last`；
    /// 3. 样式对新文本**全量**重算（O(lines)，只读块森林，代价低），
    ///    只对窗口 `[first, new_last]` 的行重建 runs/`OffsetMap`；
    /// 4. 窗口前的旧行原样保留，窗口后的旧行只做源坐标平移 ([`ProjectedLine::shifted`])。
    ///
    /// 任何异常情况（空投影、窗口为空）都退化为**全量重建**，保证不错。
    pub fn refresh(
        &mut self,
        source: &str,
        old_range: Range<usize>,
        new_range: Range<usize>,
        blocks: &[Block],
    ) {
        // delta = 新文本相对旧文本的长度变化
        let delta = len_delta(&old_range, &new_range);
        if self.lines.is_empty() {
            *self = build_from(source, blocks);
            return;
        }

        // 1. 旧行定位
        let last_line = self.lines.len() - 1;
        let first = line_index_of(&self.lines, old_range.start);
        let last = line_index_of(&self.lines, old_range.end.max(old_range.start)).min(last_line);
        let tail_keep = last_line - last; // 编辑点之后需要保留的旧行数

        // 2. 新文本行切分 + 窗口末行
        let all = split_lines(source);
        if all.is_empty() {
            self.lines.clear();
            self.widgets.clear();
            return;
        }
        let keep = tail_keep.min(all.len() - 1);
        let new_last = all.len() - 1 - keep;
        if new_last < first {
            // 删除导致窗口消失：兜底全量
            *self = build_from(source, blocks);
            return;
        }

        // 3. 样式全量重算 + 片段收集；只重建窗口行
        let mut styles: Vec<BlockStyle> = vec![BlockStyle::default(); all.len()];
        let ctx = StyleCtx::default();
        for b in blocks {
            assign_style(b, &ctx, &all, &mut styles);
        }
        let mut spans: Vec<SyntaxSpan> = Vec::new();
        for b in blocks {
            collect_spans(b, &mut spans);
        }
        let mut window: Vec<ProjectedLine> = (first..=new_last)
            .map(|i| build_line(source, &all[i], styles[i].clone(), &spans))
            .collect();

        // 4. 拼接
        let mut out = Vec::with_capacity(self.lines.len());
        out.extend(self.lines[..first].iter().cloned());
        out.append(&mut window);
        out.extend(self.lines[last + 1..].iter().map(|l| l.shifted(delta)));
        self.lines = out;
        // 块级 widget 分组依赖块森林，直接由新块森林全量重算（O(块数)，代价低）。
        self.widgets = collect_block_widgets(blocks, &all);
    }

    /// 全部块级 widget（按源序）。
    pub fn block_widgets(&self) -> &[BlockWidget] {
        &self.widgets
    }

    /// 源字节偏移所在的块级 widget（无则 `None`）。
    ///
    /// 用于渲染态/编辑态判定：光标落在 widget 的源范围内时 UI 应切到编辑态。
    /// 边界为左闭右开（`start <= src < end`）。
    pub fn block_widget_at(&self, src: usize) -> Option<&BlockWidget> {
        self.widgets
            .iter()
            .find(|w| w.source_range.start <= src && src < w.source_range.end)
    }

    /// 某一逻辑行所属的块级 widget（无则 `None`）。
    ///
    /// 比 [`block_widget_at`](Self::block_widget_at) 更稳的判定：即便光标刚好落在
    /// widget 源范围的边界（如围栏行的行尾），只要在该范围内任一逻辑行上就命中。
    pub fn block_widget_on_line(&self, line: usize) -> Option<&BlockWidget> {
        self.widgets
            .iter()
            .find(|w| w.line_range.start <= line && line < w.line_range.end)
    }
}

/// 文档坐标：逻辑行号 + 行内**显示列**（可见字符索引，非源字节）。
///
/// 与 [`crate::DisplayPos`] 的区别：`DisplayPos` 是**行内**显示位置，`DocPos`
/// 额外带**行号**，用于跨行场景（选区绘制、点击命中、IME 候选定位）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DocPos {
    /// 逻辑行号（0 起）。
    pub line: usize,
    /// 行内显示列（可见字符索引，0 起）。
    pub col: usize,
}

impl DocPos {
    /// 文档首（第 0 行第 0 列）。
    pub const ZERO: DocPos = DocPos { line: 0, col: 0 };
}

/// 跨行选区落在某一逻辑行上的显示列区间（绘制选区高亮用）。
///
/// `cols` 是**显示列**区间（`Range<usize>`，左闭右开）；同一行的多行选区只产出
/// 一条记录，UI 可直接按 `line` 定位、按 `cols` 画高亮。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineDisplayRange {
    /// 逻辑行号。
    pub line: usize,
    /// 该行被选中的显示列区间。
    pub cols: Range<usize>,
}

/// 文档级（跨行）坐标映射：所有命中测试、选区绘制、IME 定位的统一入口
/// （SPEC §6.1、§11.3）。由每行的 [`OffsetMap`] + 行区间组合而成。
impl Projection {
    /// 源字节偏移所在逻辑行号。
    ///
    /// 行尾边界（`pos == 某行 source_range.end`）归**该行**，与增量刷新时
    /// "删行尾换行"类编辑的行归属一致；超出末尾钳到末行。
    pub fn line_of_source(&self, src: usize) -> usize {
        if self.lines.is_empty() {
            return 0;
        }
        self.lines
            .partition_point(|l| l.source_range.end < src)
            .min(self.lines.len() - 1)
    }

    /// 源字节偏移 → 文档坐标（行号 + 显示列）。
    ///
    /// 落进隐藏语法标记内部时吸附到**标记起点**（见 [`OffsetMap::source_to_display`]）。
    /// 空投影返回 [`DocPos::ZERO`]。
    pub fn source_to_doc_pos(&self, src: usize) -> DocPos {
        if self.lines.is_empty() {
            return DocPos::ZERO;
        }
        let line = self.line_of_source(src);
        let col = self.lines[line].offset_map.source_to_display(src).get();
        DocPos { line, col }
    }

    /// 文档坐标 → 源字节偏移（点击命中的核心换算）。
    ///
    /// 列越界钳到该行末（返回行尾源偏移），行号越界钳到末行；交叉点仍取隐藏
    /// 标记起点（稳定落点）。空投影返回 `0`。
    pub fn doc_pos_to_source(&self, pos: DocPos) -> usize {
        if self.lines.is_empty() {
            return 0;
        }
        let line = pos.line.min(self.lines.len() - 1);
        self.lines[line]
            .offset_map
            .display_to_source(DisplayPos(pos.col))
    }

    /// 某一逻辑行的显示宽度（可见字符数）。行号越界钳到末行；空投影返回 `0`。
    pub fn display_len_at(&self, line: usize) -> usize {
        if self.lines.is_empty() {
            return 0;
        }
        self.lines[line.min(self.lines.len() - 1)]
            .offset_map
            .display_len()
    }

    /// 选区覆盖到的每行显示列区间（跨行选区绘制用）。
    ///
    /// 空选区（光标）或空投影返回空表。单行选区产出 1 条；跨行时首行
    /// `起点列..行宽`、中间行 `0..行宽`、末行 `0..终点列`。区间允许为空
    /// （如整行隐藏或选区恰好落在行尾），交由 UI 决定是否绘制。
    pub fn selection_display_ranges(&self, sel: Selection) -> Vec<LineDisplayRange> {
        if sel.is_collapsed() || self.lines.is_empty() {
            return Vec::new();
        }
        let start = self.source_to_doc_pos(sel.start());
        let end = self.source_to_doc_pos(sel.end());

        // 单行选区
        if start.line == end.line {
            return vec![LineDisplayRange {
                line: start.line,
                cols: start.col..end.col,
            }];
        }

        let mut out = Vec::with_capacity(end.line - start.line + 1);
        // 首行：起点列 → 行尾
        out.push(LineDisplayRange {
            line: start.line,
            cols: start.col..self.display_len_at(start.line),
        });
        // 中间整行
        for line in (start.line + 1)..end.line {
            out.push(LineDisplayRange {
                line,
                cols: 0..self.display_len_at(line),
            });
        }
        // 末行：行首 → 终点列
        out.push(LineDisplayRange {
            line: end.line,
            cols: 0..end.col,
        });
        out
    }
}

/// 编辑带来的文本长度变化：`new_range.len() - old_range.len()`。
fn len_delta(old_range: &Range<usize>, new_range: &Range<usize>) -> isize {
    let old_len = old_range.end.saturating_sub(old_range.start);
    let new_len = new_range.end.saturating_sub(new_range.start);
    isize::try_from(new_len).unwrap_or(isize::MAX) - isize::try_from(old_len).unwrap_or(isize::MAX)
}

/// 源偏移 `pos` 落在哪一行（超出末尾则返回最后一行）。
///
/// 行区间不含行尾换行；`pos` 恰好等于某行终点时归该行（使"删行尾换行"这类编辑
/// 仍能把两行都纳入重算窗口）。
fn line_index_of(lines: &[ProjectedLine], pos: usize) -> usize {
    lines
        .partition_point(|l| l.source_range.end < pos)
        .min(lines.len().saturating_sub(1))
}

/// 一行的投影。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedLine {
    /// 本行在源文本中的字节范围（不含行尾换行）。
    pub source_range: Range<usize>,
    /// 本行的块样式（标题级别/引用深度/列表/任务/代码块/分割线）。
    pub block: BlockStyle,
    /// 带样式的可见文本片段（已隐藏语法标记）。
    pub runs: Vec<TextRun>,
    /// 表格行的单元格显示文本范围（不含竖线，已裁掉两端空白）；非表格行为 `None`。
    ///
    /// 范围是**显示文本内的字节区间**，供 UI 按内容重排列宽、画竖向网格线，
    /// 以及在单元格之间跳转；竖线本身仍保留在显示文本里（编辑时可被光标命中）。
    pub table_cells: Option<Vec<Range<usize>>>,
    /// 内联占位（图片等）。
    pub widgets: Vec<Widget>,
    /// 显示位置 ↔ 源偏移 双向映射。
    pub offset_map: OffsetMap,
}

impl ProjectedLine {
    /// 本行显示文本（可见片段拼接，不含 Widget）。
    pub fn display_text(&self) -> String {
        let mut s = String::new();
        for r in &self.runs {
            s.push_str(&r.text);
        }
        s
    }

    /// 显示文本的字节长度。
    pub fn display_byte_len(&self) -> usize {
        self.runs.iter().map(|r| r.text.len()).sum()
    }

    /// 显示列（可见字符索引）→ 显示文本内的**字节偏移**（越界钳到文本末）。
    ///
    /// 供 UI 把 [`DocPos::col`]（字符列）换算成 `shape_text`/字形索引所需的字节偏移：
    /// 形状化与换行边界都以显示文本的 UTF-8 字节为单位，而光标/选区用的是字符列。
    pub fn display_byte_of_col(&self, col: usize) -> usize {
        let mut chars = 0usize;
        let mut bytes = 0usize;
        for r in &self.runs {
            let n = r.text.chars().count();
            if col <= chars + n {
                let want = col - chars;
                return bytes
                    + r.text
                        .char_indices()
                        .nth(want)
                        .map_or(r.text.len(), |(b, _)| b);
            }
            chars += n;
            bytes += r.text.len();
        }
        bytes
    }

    /// 显示文本内的**字节偏移** → 显示列（可见字符索引，越界钳到文本末）。
    ///
    /// [`display_byte_of_col`](Self::display_byte_of_col) 的逆；字节偏移落在多字节
    /// 字符内部时，向下吸附到该字符起点（即只统计"完全位于该字节之前"的字符）。
    pub fn display_col_of_byte(&self, byte: usize) -> usize {
        let mut chars = 0usize;
        let mut bytes = 0usize;
        for r in &self.runs {
            let rlen = r.text.len();
            if byte <= bytes + rlen {
                let want = byte - bytes;
                let n = r
                    .text
                    .char_indices()
                    .take_while(|&(b, c)| b + c.len_utf8() <= want)
                    .count();
                return chars + n;
            }
            chars += r.text.chars().count();
            bytes += rlen;
        }
        chars
    }

    /// 行内容未变、仅源坐标整体平移 `delta` 时，产出一行平移后的副本。
    ///
    /// 可见文本、显示位置、样式均不变；只平移行区间、片段区间与
    /// [`OffsetMap`] 内的源偏移。
    pub(crate) fn shifted(&self, delta: isize) -> Self {
        let mut line = self.clone();
        line.source_range = shift_range(line.source_range, delta);
        for run in &mut line.runs {
            run.source_range = shift_range(run.source_range.clone(), delta);
        }
        for widget in &mut line.widgets {
            widget.source_range = shift_range(widget.source_range.clone(), delta);
        }
        line.offset_map.shift(delta);
        line
    }
}

/// 平移区间两端（正值右移、负值左移，饱和运算）。
fn shift_range(r: Range<usize>, delta: isize) -> Range<usize> {
    let step = delta.unsigned_abs();
    let (s, e) = if delta >= 0 {
        (r.start.saturating_add(step), r.end.saturating_add(step))
    } else {
        (r.start.saturating_sub(step), r.end.saturating_sub(step))
    };
    s..e
}

/// 块样式：一行所属的块级语义（容器样式可叠加，如"引用中的列表项"）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BlockStyle {
    /// 引用嵌套深度（0 = 不在引用内）。
    pub quote_depth: u8,
    /// 标题级别（1..=6）；`None` 非标题。
    pub heading: Option<u8>,
    /// 列表项符号（有序/无序 + 序号）；`None` 非列表项。
    pub bullet: Option<Bullet>,
    /// 任务状态：`Some(true)` 已完成、`Some(false)` 未完成；`None` 非任务项。
    pub task: Option<bool>,
    /// 是否代码块内容行。
    pub code_block: bool,
    /// 代码块语言标识（若声明）。
    pub code_lang: Option<String>,
    /// 是否分割线。
    pub thematic_break: bool,
    /// 表格行的角色（表头/分隔行/表体）；非表格行为 `None`。
    pub table_line: Option<TableLine>,
}

/// GFM 表格中需要 UI 特殊处理的行（SPEC §11.5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableLine {
    /// 表头行：可加粗、铺底色。
    Header,
    /// 分隔行（`| --- |`）：投影显示为空，UI 可将其高度并入表头内边距。
    Delimiter,
    /// 表体数据行：UI 可据此把整张表按列网格渲染。
    Body,
}

/// 列表项符号。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bullet {
    /// 是否有序列表。
    pub ordered: bool,
    /// 有序列表序号（无序列表固定 1）。
    pub number: u64,
}

/// 一个带样式的可见文本片段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextRun {
    /// 源字节范围（连续可见文本）。
    pub source_range: Range<usize>,
    /// 本片段首字符在显示文本中的字符列。
    pub display_start: usize,
    /// 文本样式。
    pub style: RunStyle,
    /// 可见文本内容。
    pub text: String,
}

/// 文本样式（由 [`SpanRole`] 映射）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStyle {
    /// 普通文本。
    Normal,
    /// 斜体。
    Emphasis,
    /// 粗体。
    Strong,
    /// 删除线。
    Strikethrough,
    /// 行内代码。
    InlineCode,
    /// 链接文字。
    LinkText,
    /// 行内公式体 `$…$`（D34）：UI 用斜体 + 主题色区分。
    Math,
    /// 脚注标签（引用的 `[^label]`、定义的 `[^label]:`；D35）：UI 用主题色区分。
    Footnote,
}

impl From<SpanRole> for RunStyle {
    fn from(role: SpanRole) -> Self {
        match role {
            SpanRole::Emphasis => RunStyle::Emphasis,
            SpanRole::Strong => RunStyle::Strong,
            SpanRole::Strikethrough => RunStyle::Strikethrough,
            SpanRole::InlineCode => RunStyle::InlineCode,
            SpanRole::LinkText => RunStyle::LinkText,
            SpanRole::Math => RunStyle::Math,
            SpanRole::FootnoteRef => RunStyle::Footnote,
            _ => RunStyle::Normal,
        }
    }
}

/// 内联占位对象（源文本隐藏，由 UI 绘制）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Widget {
    /// 被隐藏的源字节范围（如整段 `![alt](dest)`）。
    pub source_range: Range<usize>,
    /// 位于显示文本中的字符列。
    pub display_col: usize,
    /// 占位类型。
    pub kind: WidgetKind,
}

/// 占位类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WidgetKind {
    /// 图片。
    Image {
        /// 替代文本。
        alt: String,
    },
}

impl WidgetKind {
    /// 占位的最小显示文本（用于纯文本场景占宽）。
    pub fn placeholder(&self) -> String {
        match self {
            WidgetKind::Image { alt } => format!("[图片: {alt}]"),
        }
    }
}

/// 块级 widget：整块由 UI 按"渲染态/编辑态"整体绘制（SPEC §11.5、D30）。
///
/// 光标不在其任一逻辑行时呈**渲染态**（UI 隐藏围栏/分隔等标记、按块绘制）；
/// 光标进入其任一行则切回**编辑态**（显示原文）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockWidget {
    /// 覆盖整块的源字节范围（含围栏/分隔行）。
    pub source_range: Range<usize>,
    /// 覆盖的逻辑行范围（0 起，半开）。
    pub line_range: Range<usize>,
    /// widget 类型。
    pub kind: BlockWidgetKind,
}

/// 块级 widget 类型（SPEC §6.1）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockWidgetKind {
    /// 代码块（围栏/缩进）：`lang` 为围栏信息串中的语言标识。
    Code {
        /// 语言标识（如 `rust`）。
        lang: Option<String>,
        /// 是否为围栏代码块（``` 包裹）。
        fenced: bool,
    },
    /// GFM 表格：`align` 为各列对齐方式。
    Table {
        /// 每列对齐方式（列数 = 数组长度）。
        align: Vec<Align>,
    },
    /// 分割线（单行）。
    Rule,
    /// 显示公式块 `$$…$$`（D34）。
    Math,
    /// 图表块：围栏语言为 `lang` 的代码块（如 `mermaid`）归类为此（D34 ③）。
    Chart {
        /// 图表语言标识（如 `mermaid`）。
        lang: String,
    },
}

/// 由源文本构造整篇投影。
pub fn build(source: &str) -> Projection {
    let blocks = mazha_md::parse(source);
    build_from(source, &blocks)
}

/// 用**已有块森林**构建投影（供 `Document` 复用缓存，避免重复全量解析）。
///
/// `blocks` 必须是 `source` 的解析结果（[`mazha_md::parse`] 或
/// [`mazha_md::reparse`] 产出），否则投影与文本会不一致。
pub fn build_from(source: &str, blocks: &[Block]) -> Projection {
    let lines = split_lines(source);

    // 每行的块样式：默认段落，再由块森林按继承上下文覆盖（最深叶子优先）。
    let mut styles: Vec<BlockStyle> = vec![BlockStyle::default(); lines.len()];
    let ctx = StyleCtx::default();
    for b in blocks {
        assign_style(b, &ctx, &lines, &mut styles);
    }

    // 收集整棵树的片段（容器标记 + 叶子内容），按行归并。
    let mut spans: Vec<SyntaxSpan> = Vec::new();
    for b in blocks {
        collect_spans(b, &mut spans);
    }

    let projected = lines
        .iter()
        .enumerate()
        .map(|(i, line)| build_line(source, line, styles[i].clone(), &spans))
        .collect();
    let widgets = collect_block_widgets(blocks, &lines);

    Projection {
        lines: projected,
        widgets,
    }
}

/// 行扫描用的行范围（不含行尾换行）。
#[derive(Debug, Clone, Copy)]
struct Line {
    start: usize,
    end: usize,
}

/// 按 `\n` 切分源文本为逻辑行（兼容 `\r\n`；保留末尾空行）。
fn split_lines(source: &str) -> Vec<Line> {
    let bytes = source.as_bytes();
    let mut lines = Vec::new();
    let mut start = 0usize;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' {
            let end = if i > start && bytes[i - 1] == b'\r' {
                i - 1
            } else {
                i
            };
            lines.push(Line { start, end });
            start = i + 1;
        }
    }
    // 末尾（含完全空文档）
    lines.push(Line {
        start,
        end: bytes.len(),
    });
    lines
}

/// 与给定源范围重叠的行下标（升序）。
fn lines_overlapping(lines: &[Line], range: &Range<usize>) -> Vec<usize> {
    if range.start >= range.end {
        return Vec::new();
    }
    let mut i = lines.partition_point(|l| l.end <= range.start);
    let mut out = Vec::new();
    while i < lines.len() && lines[i].start < range.end {
        out.push(i);
        i += 1;
    }
    out
}

/// 样式继承上下文（容器层累积）。
#[derive(Debug, Clone, Default)]
struct StyleCtx {
    quote_depth: u8,
    bullet: Option<Bullet>,
    task: Option<bool>,
}

/// 递归地把块样式写入其覆盖的行。
fn assign_style(block: &Block, ctx: &StyleCtx, lines: &[Line], styles: &mut [BlockStyle]) {
    if block.is_container() {
        match &block.kind {
            BlockKind::BlockQuote | BlockKind::Callout { .. } => {
                let mut inner = ctx.clone();
                inner.quote_depth = inner.quote_depth.saturating_add(1);
                for c in &block.children {
                    assign_style(c, &inner, lines, styles);
                }
            }
            BlockKind::List { ordered, start } => {
                for (offset, item) in block.children.iter().enumerate() {
                    let mut inner = ctx.clone();
                    inner.bullet = Some(Bullet {
                        ordered: *ordered,
                        number: start.saturating_add(offset as u64),
                    });
                    assign_style(item, &inner, lines, styles);
                }
            }
            BlockKind::ListItem { task } => {
                let mut inner = ctx.clone();
                inner.task = *task;
                // 列表项的**标记行**始终由容器写样式。仅凭 `children.is_empty()`
                // 判断不够：新空项后面若跟着缩进内容（嵌套列表/续行），它会有子块，
                // 但子块不覆盖标记行本身，标记行就退回默认段落样式——项目符号/
                // 复选框/序号因此要等输入文字后才出现。
                let style = BlockStyle {
                    quote_depth: inner.quote_depth,
                    bullet: inner.bullet.clone(),
                    task: inner.task,
                    ..BlockStyle::default()
                };
                if let Some(&idx) = lines_overlapping(lines, &block.range).first() {
                    styles[idx] = style;
                }
                for c in &block.children {
                    assign_style(c, &inner, lines, styles);
                }
            }
            // 其它容器（暂未出现）透传上下文
            _ => {
                for c in &block.children {
                    assign_style(c, ctx, lines, styles);
                }
            }
        }
        return;
    }

    // 叶子块：构造本行样式并覆盖
    let style = BlockStyle {
        quote_depth: ctx.quote_depth,
        heading: match block.kind {
            BlockKind::Heading { level } => Some(level),
            _ => None,
        },
        bullet: ctx.bullet.clone(),
        task: ctx.task,
        code_block: matches!(block.kind, BlockKind::CodeBlock { .. }),
        code_lang: match &block.kind {
            BlockKind::CodeBlock { lang, .. } => lang.clone(),
            _ => None,
        },
        thematic_break: matches!(block.kind, BlockKind::ThematicBreak),
        ..BlockStyle::default()
    };
    // 表格：首行是表头，次行是分隔行（解析器保证紧随表头）。分隔行在投影里显示为空，
    // 由 UI 当作表头与表体之间的内边距处理。
    if matches!(block.kind, BlockKind::Table { .. }) {
        let covered = lines_overlapping(lines, &block.range);
        for (offset, &idx) in covered.iter().enumerate() {
            let mut row = style.clone();
            row.table_line = match offset {
                0 => Some(TableLine::Header),
                1 => Some(TableLine::Delimiter),
                _ => Some(TableLine::Body),
            };
            styles[idx] = row;
        }
    } else {
        for idx in lines_overlapping(lines, &block.range) {
            styles[idx] = style.clone();
        }
    }
}

/// 递归收集块森林的全部片段（容器标记 + 叶子内容）。
fn collect_spans(block: &Block, out: &mut Vec<SyntaxSpan>) {
    out.extend(block.spans.iter().cloned());
    for c in &block.children {
        collect_spans(c, out);
    }
}

/// 收集块森林里的块级 widget（代码块/表格/分割线/公式/图表），按源序排列（SPEC §11.5、D30）。
///
/// 只处理叶子块；容器块（引用/列表/提示块）透传，故嵌套在列表项/引用内的代码块/表格
/// 也会各自产出一个 widget（其 `line_range` 覆盖各自的逻辑行）。块的 `line_range`
/// 由覆盖的逻辑行推导（首行..末行+1）。
fn collect_block_widgets(blocks: &[Block], lines: &[Line]) -> Vec<BlockWidget> {
    let mut out = Vec::new();
    for b in blocks {
        collect_block_widgets_into(b, lines, &mut out);
    }
    out.sort_by_key(|w| w.source_range.start);
    out
}

fn collect_block_widgets_into(block: &Block, lines: &[Line], out: &mut Vec<BlockWidget>) {
    let kind = match &block.kind {
        // mermaid 围栏块归类为图表 widget（D34 ③：解析器不特判，归类放在投影层）
        BlockKind::CodeBlock {
            lang: Some(lang), ..
        } if lang == "mermaid" => Some(BlockWidgetKind::Chart { lang: lang.clone() }),
        BlockKind::CodeBlock { lang, fenced } => Some(BlockWidgetKind::Code {
            lang: lang.clone(),
            fenced: *fenced,
        }),
        BlockKind::MathBlock => Some(BlockWidgetKind::Math),
        BlockKind::Table { align } => Some(BlockWidgetKind::Table {
            align: align.clone(),
        }),
        BlockKind::ThematicBreak => Some(BlockWidgetKind::Rule),
        _ => None,
    };
    if let Some(kind) = kind {
        let covered = lines_overlapping(lines, &block.range);
        if let (Some(&first), Some(&last)) = (covered.first(), covered.last()) {
            out.push(BlockWidget {
                source_range: block.range.clone(),
                line_range: first..last + 1,
                kind,
            });
        }
    }
    for c in &block.children {
        collect_block_widgets_into(c, lines, out);
    }
}

/// 按竖线位置切分显示文本，返回各单元格的显示字节范围（已裁掉两端空白）。
///
/// `pipes` 是竖线在显示文本中的字节偏移。行首/行尾竖线带来的空段被丢弃；
/// 行内连续竖线之间的空单元格保留（与 GFM 语义一致）。
fn table_cell_ranges(display: &str, pipes: &[usize]) -> Vec<Range<usize>> {
    let bytes = display.as_bytes();
    // 单元格候选：相邻两条竖线之间的内容；首/尾竖线外侧的段单独收集。
    let mut segs: Vec<Range<usize>> = Vec::new();
    if let Some(&first) = pipes.first() {
        segs.push(0..first);
    }
    for pair in pipes.windows(2) {
        segs.push(pair[0] + 1..pair[1]);
    }
    if let Some(&last) = pipes.last()
        && last < bytes.len()
    {
        segs.push(last + 1..bytes.len());
    }
    if pipes.is_empty() {
        return Vec::new();
    }
    // 首段只有空白（有行首竖线）则丢弃；末段同理。
    if let Some(first) = segs.first()
        && first.start == 0
        && display[first.clone()].trim().is_empty()
    {
        segs.remove(0);
    }
    if let Some(last) = segs.last()
        && last.end == bytes.len()
        && display[last.clone()].trim().is_empty()
    {
        segs.pop();
    }
    segs.into_iter()
        .map(|r| {
            let s = bytes[r.clone()]
                .iter()
                .position(|b| !b.is_ascii_whitespace())
                .map_or(r.end, |i| r.start + i);
            let e = bytes[r.clone()]
                .iter()
                .rposition(|b| !b.is_ascii_whitespace())
                .map_or(r.start, |i| r.start + i + 1);
            // 全空白单元格与 `split_cells` 一致，折成紧贴右竖线的空范围
            let e = e.max(s);
            s..e
        })
        .collect()
}

/// 构造单行投影。
fn build_line(source: &str, line: &Line, block: BlockStyle, all: &[SyntaxSpan]) -> ProjectedLine {
    let line_range = line.start..line.end;
    // 本行相关片段（含只覆盖本行一部分的容器标记）
    let mut spans: Vec<&SyntaxSpan> = all
        .iter()
        .filter(|s| s.range.start < line.end && s.range.end > line.start && !s.is_empty())
        .collect();
    spans.sort_by_key(|s| s.range.start);

    let mut runs: Vec<TextRun> = Vec::new();
    let mut widgets: Vec<Widget> = Vec::new();
    let mut visible: Vec<Range<usize>> = Vec::new();
    // 表格竖线在显示文本中的字节位置（仅 TablePipe 片段；转义竖线是普通文本）
    let mut pipes: Vec<usize> = Vec::new();
    let mut disp_bytes = 0usize;
    let mut disp_col = 0usize;

    for span in spans {
        if span.is_text() {
            // 可见文本：记录可见区间，累积显示列，合并相邻同样式片段
            let style = RunStyle::from(span.role);
            let text = source
                .get(span.range.clone())
                .unwrap_or_default()
                .to_string();
            visible.push(span.range.clone());
            if span.role == SpanRole::TablePipe {
                pipes.push(disp_bytes);
            }
            let chars = text.chars().count();
            disp_bytes += text.len();
            match runs.last_mut() {
                Some(last) if last.style == style && last.source_range.end == span.range.start => {
                    last.text.push_str(&text);
                    last.source_range.end = span.range.end;
                }
                _ => runs.push(TextRun {
                    source_range: span.range.clone(),
                    display_start: disp_col,
                    style,
                    text,
                }),
            }
            disp_col += chars;
        } else if span.kind == SpanKind::Widget {
            widgets.push(Widget {
                source_range: span.range.clone(),
                display_col: disp_col,
                kind: extract_widget(source, &span.range),
            });
        }
    }

    // 可见区间需有序不重叠；解析器保证文本片段互不重叠，排序后即可。
    visible.sort_by_key(|r| r.start);
    let offset_map =
        OffsetMap::from_visible_runs(source, &visible).unwrap_or_else(|_| OffsetMap::empty());

    // 表格单元格：按 TablePipe 片段切分显示文本，得到各单元格的显示字节范围。
    // 用片段角色而不是字符 `|` 判断，避免把转义竖线 `\|` 误当分隔符。
    let table_cells = block.table_line.map(|_| {
        let display = runs.iter().map(|r| r.text.as_str()).collect::<String>();
        table_cell_ranges(&display, &pipes)
    });
    ProjectedLine {
        source_range: line_range,
        block,
        runs,
        table_cells,
        widgets,
        offset_map,
    }
}

/// 从被隐藏的 Widget 源范围里提取占位信息。
fn extract_widget(source: &str, range: &Range<usize>) -> WidgetKind {
    let s = source.get(range.clone()).unwrap_or_default();
    if let Some(rest) = s.strip_prefix("![") {
        let alt = rest.find(']').map_or("", |i| &rest[..i]);
        WidgetKind::Image {
            alt: alt.to_string(),
        }
    } else {
        WidgetKind::Image { alt: String::new() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(source: &str) -> ProjectedLine {
        let p = build(source);
        assert_eq!(p.lines.len(), 1, "期望单行：{source:?}");
        p.lines.into_iter().next().unwrap()
    }

    /// 新建空任务项（回车续行产出 `- [ ] `）必须立刻带复选框/项目符号，
    /// 否则光标落在空行上时看不到"下一项"的前缀图标。
    #[test]
    fn empty_task_item_keeps_marker() {
        let l = one("- [ ] ");
        assert_eq!(l.block.task, Some(false));
        assert!(l.block.bullet.is_some());
        assert_eq!(l.display_text(), "");
    }

    /// 新空项后面紧跟缩进内容时，子块会让该项不再是"空容器"；标记行仍须有
    /// 列表样式，否则序号/项目符号要等输入后才显示。
    #[test]
    fn empty_ordered_item_before_nested_content_keeps_marker() {
        let p = build("1. a\n2. \n   - x\n");
        let l = &p.lines[1];
        assert_eq!(l.display_text(), "");
        assert_eq!(
            l.block.bullet,
            Some(Bullet {
                ordered: true,
                number: 2
            })
        );
    }

    #[test]
    fn plain_paragraph() {
        let l = one("hello world");
        assert_eq!(l.display_text(), "hello world");
        assert_eq!(l.block, BlockStyle::default());
        assert_eq!(l.runs.len(), 1);
        assert_eq!(l.runs[0].display_start, 0);
        assert_eq!(l.offset_map.display_len(), 11);
        assert_eq!(l.offset_map.display_to_source(crate::DisplayPos(0)), 0);
        assert_eq!(l.offset_map.display_to_source(crate::DisplayPos(11)), 11);
    }

    #[test]
    fn heading_hides_marker() {
        let l = one("# 标题");
        assert_eq!(l.block.heading, Some(1));
        assert_eq!(l.display_text(), "标题");
        assert_eq!(l.offset_map.display_len(), 2);
        // 源 2..8 是"标题"；交叉点稳定落在标记起点
        assert_eq!(l.offset_map.source_to_display(2).get(), 0);
        assert_eq!(l.offset_map.source_to_display(8).get(), 2);
    }

    #[test]
    fn strong_text_run_style() {
        let l = one("**粗**体");
        assert_eq!(l.display_text(), "粗体");
        assert_eq!(l.runs.len(), 2);
        assert_eq!(l.runs[0].style, RunStyle::Strong);
        assert_eq!(l.runs[0].text, "粗");
        assert_eq!(l.runs[1].style, RunStyle::Normal);
        assert_eq!(l.runs[1].display_start, 1);
    }

    #[test]
    fn list_item_has_bullet() {
        let l = one("- item");
        assert_eq!(l.display_text(), "item");
        let bullet = l.block.bullet.expect("列表项应有 bullet");
        assert!(!bullet.ordered);
        assert_eq!(bullet.number, 1);
    }

    #[test]
    fn task_item_flag() {
        let l = one("- [x] done");
        assert_eq!(l.display_text(), "done");
        assert_eq!(l.block.task, Some(true));
    }

    #[test]
    fn ordered_bullet_number_advances() {
        let p = build("1. a\n2. b");
        assert_eq!(p.lines.len(), 2);
        assert_eq!(p.lines[0].block.bullet.as_ref().unwrap().number, 1);
        assert_eq!(p.lines[1].block.bullet.as_ref().unwrap().number, 2);
        assert!(p.lines[1].block.bullet.as_ref().unwrap().ordered);
    }

    #[test]
    fn block_quote_depth() {
        let l = one("> quoted");
        assert_eq!(l.block.quote_depth, 1);
        assert_eq!(l.display_text(), "quoted");
    }

    #[test]
    fn fenced_code_lines() {
        let p = build("```rust\nlet x = 1;\n```\n");
        let code = &p.lines[1];
        assert!(code.block.code_block);
        assert_eq!(code.block.code_lang.as_deref(), Some("rust"));
        assert_eq!(code.display_text(), "let x = 1;");
        // 围栏行本身没有可见文本
        assert_eq!(p.lines[0].display_text(), "");
        assert_eq!(p.lines[2].display_text(), "");
    }

    #[test]
    fn image_becomes_widget() {
        let l = one("![alt](x.png)");
        assert_eq!(l.display_text(), "");
        assert_eq!(l.widgets.len(), 1);
        assert_eq!(l.widgets[0].display_col, 0);
        assert_eq!(
            l.widgets[0].kind,
            WidgetKind::Image {
                alt: "alt".to_string()
            }
        );
        assert_eq!(l.offset_map.display_len(), 0);
    }

    #[test]
    fn multiline_paragraph_splits_lines() {
        let p = build("line1\nline2");
        assert_eq!(p.lines.len(), 2);
        assert_eq!(p.lines[0].display_text(), "line1");
        assert_eq!(p.lines[1].display_text(), "line2");
        // 软换行标记隐藏，两行各自映射
        assert_eq!(p.lines[0].offset_map.display_len(), 5);
        assert_eq!(
            p.lines[1]
                .offset_map
                .display_to_source(crate::DisplayPos(0)),
            6
        );
    }

    #[test]
    fn trailing_newline_yields_empty_last_line() {
        let p = build("a\n");
        assert_eq!(p.lines.len(), 2);
        assert_eq!(p.lines[1].source_range, 2..2);
        assert_eq!(p.lines[1].display_text(), "");
    }

    #[test]
    fn link_hides_url_keeps_text() {
        let l = one("[文字](https://x.com)");
        assert_eq!(l.display_text(), "文字");
        assert_eq!(l.offset_map.display_len(), 2);
        assert_eq!(l.runs[0].style, RunStyle::LinkText);
    }

    #[test]
    fn thematic_break_flag() {
        let l = one("---");
        assert!(l.block.thematic_break);
        assert_eq!(l.display_text(), "");
    }

    // ---------- 文档级（跨行）坐标映射：SPEC §6.1 / §11.3 ----------

    #[test]
    fn line_of_source_boundaries() {
        // "line1\nline2"：line0 0..5，'\n' 在 5，line1 6..11
        let p = build("line1\nline2");
        assert_eq!(p.line_of_source(0), 0);
        assert_eq!(p.line_of_source(5), 0, "行尾边界归前一行");
        assert_eq!(p.line_of_source(6), 1, "次行起点归本行");
        assert_eq!(p.line_of_source(11), 1);
        assert_eq!(p.line_of_source(99), 1, "越界钳到末行");
    }

    #[test]
    fn line_of_source_blank_and_trailing_lines() {
        // "a\n\nb"：line0 0..1、line1 2..2（空行）、line2 3..4
        let p = build("a\n\nb");
        assert_eq!(p.lines.len(), 3);
        assert_eq!(p.line_of_source(1), 0);
        assert_eq!(p.line_of_source(2), 1);
        assert_eq!(p.line_of_source(3), 2);
        // 末尾换行产生空末行
        let q = build("a\n");
        assert_eq!(q.lines.len(), 2);
        assert_eq!(q.line_of_source(2), 1, "EOF 归末尾空行");
    }

    #[test]
    fn source_to_doc_pos_hides_markers() {
        // "# 标题" 单行：隐藏 "# "（0..2），可见 "标题"（2..8）
        let p = build("# 标题");
        assert_eq!(p.source_to_doc_pos(2), DocPos { line: 0, col: 0 });
        assert_eq!(p.source_to_doc_pos(5), DocPos { line: 0, col: 1 });
        assert_eq!(p.source_to_doc_pos(8), DocPos { line: 0, col: 2 });
        // 落进隐藏标记内部吸附到标记起点
        assert_eq!(p.source_to_doc_pos(1), DocPos { line: 0, col: 0 });
    }

    #[test]
    fn doc_pos_to_source_is_inverse_on_visible_text() {
        let p = build("# 标题\n正文**粗**。");
        // 首行可见列 0..2
        assert_eq!(p.doc_pos_to_source(DocPos { line: 0, col: 0 }), 2);
        assert_eq!(p.doc_pos_to_source(DocPos { line: 0, col: 2 }), 8);
        // 第二行起点（"正文" 的 "正"，行首）
        let s = p.doc_pos_to_source(DocPos { line: 1, col: 0 });
        assert_eq!(p.source_to_doc_pos(s), DocPos { line: 1, col: 0 });
    }

    #[test]
    fn doc_pos_clamps_column_and_line() {
        let p = build("ab\ncd");
        // 列越界钳到行尾（返回该行末的源偏移）
        assert_eq!(p.doc_pos_to_source(DocPos { line: 0, col: 99 }), 2);
        // 行越界钳到末行
        assert_eq!(p.doc_pos_to_source(DocPos { line: 99, col: 0 }), 3);
        assert_eq!(p.display_len_at(99), 2, "行越界取末行宽度");
    }

    #[test]
    fn selection_within_one_line() {
        let p = build("**粗体**文本");
        // 可见 "粗体文本"，源 2..8 是 "粗体"
        let ranges = p.selection_display_ranges(Selection::new(2, 8));
        assert_eq!(
            ranges,
            vec![LineDisplayRange {
                line: 0,
                cols: 0..2
            }]
        );
    }

    #[test]
    fn selection_across_two_lines() {
        // "ab\ncd"：line0 0..2，line1 3..5
        let p = build("ab\ncd");
        let ranges = p.selection_display_ranges(Selection::new(1, 4));
        assert_eq!(
            ranges,
            vec![
                LineDisplayRange {
                    line: 0,
                    cols: 1..2
                },
                LineDisplayRange {
                    line: 1,
                    cols: 0..1
                },
            ]
        );
    }

    #[test]
    fn selection_across_three_lines_includes_whole_middle() {
        // "中\n文\n字"：每个 CJK 3 字节，行区间 0..3 / 4..7 / 8..11
        let p = build("中\n文\n字");
        assert_eq!(p.lines.len(), 3);
        let ranges = p.selection_display_ranges(Selection::new(0, 11));
        assert_eq!(
            ranges,
            vec![
                LineDisplayRange {
                    line: 0,
                    cols: 0..1
                },
                LineDisplayRange {
                    line: 1,
                    cols: 0..1
                },
                LineDisplayRange {
                    line: 2,
                    cols: 0..1
                },
            ]
        );
    }

    #[test]
    fn selection_reversed_and_collapsed() {
        let p = build("ab\ncd");
        // 反向选区与正向等价
        assert_eq!(
            p.selection_display_ranges(Selection::new(4, 1)),
            p.selection_display_ranges(Selection::new(1, 4))
        );
        // 空选区（光标）不产出高亮
        assert!(p.selection_display_ranges(Selection::caret(2)).is_empty());
    }

    #[test]
    fn empty_projection_maps_to_zero() {
        let p = build("");
        assert_eq!(p.lines.len(), 1, "空文档仍有一行（便于光标落点）");
        assert_eq!(p.line_of_source(0), 0);
        assert_eq!(p.source_to_doc_pos(0), DocPos::ZERO);
        assert_eq!(p.doc_pos_to_source(DocPos::ZERO), 0);
        assert_eq!(p.display_len_at(0), 0);
    }

    // ---------- 显示列 ↔ 显示文本字节：SPEC §6.1（软换行布局用） ----------

    #[test]
    fn display_byte_of_col_mixed_widths() {
        // "a中b"：字节 0 / 1..4 / 4..5；字符列 0 / 1 / 2 / 3
        let l = one("a中b");
        assert_eq!(l.display_text(), "a中b");
        assert_eq!(l.display_byte_len(), 5);
        assert_eq!(l.display_byte_of_col(0), 0);
        assert_eq!(l.display_byte_of_col(1), 1, "中 起于字节 1");
        assert_eq!(l.display_byte_of_col(2), 4, "b 起于字节 4");
        assert_eq!(l.display_byte_of_col(3), 5, "列=字符数 → 文本末");
        assert_eq!(l.display_byte_of_col(99), 5, "越界钳到文本末");
    }

    #[test]
    fn display_col_of_byte_inverts_and_snaps() {
        let l = one("a中b");
        assert_eq!(l.display_col_of_byte(0), 0);
        assert_eq!(l.display_col_of_byte(1), 1);
        assert_eq!(l.display_col_of_byte(4), 2);
        assert_eq!(l.display_col_of_byte(5), 3);
        assert_eq!(l.display_col_of_byte(99), 3, "越界钳到文本末");
        // 落在多字节字符内部：向上取整到该字符之后
        assert_eq!(l.display_col_of_byte(2), 1, "中 内部 → 吸附到其起点");
        // 往返一致（列边界）
        for col in 0..=3 {
            assert_eq!(l.display_col_of_byte(l.display_byte_of_col(col)), col);
        }
    }

    #[test]
    fn display_byte_of_col_spans_runs() {
        // "**粗**体"：可见 "粗体"（两段：粗 Strong、体 Normal）
        let l = one("**粗**体");
        assert_eq!(l.display_text(), "粗体");
        assert_eq!(l.runs.len(), 2);
        assert_eq!(l.display_byte_of_col(0), 0);
        assert_eq!(l.display_byte_of_col(1), 3, "第二段起点（粗 为 3 字节）");
        assert_eq!(l.display_byte_of_col(2), 6);
        assert_eq!(l.display_col_of_byte(3), 1);
        assert_eq!(l.display_col_of_byte(6), 2);
    }

    #[test]
    fn display_byte_of_col_empty_line() {
        let l = one("");
        assert_eq!(l.display_byte_len(), 0);
        assert_eq!(l.display_byte_of_col(0), 0);
        assert_eq!(l.display_col_of_byte(0), 0);
        assert_eq!(l.display_byte_of_col(9), 0);
        assert_eq!(l.display_col_of_byte(9), 0);
    }

    // ---------- 块级 widget：SPEC §11.5 / D30 ----------

    #[test]
    fn fenced_code_block_is_one_block_widget() {
        // 开围栏行 + 内容行 + 闭围栏行 = 3 逻辑行，聚成 1 个 Code widget
        let p = build("```rust\nlet x = 1;\n```\n");
        assert_eq!(p.widgets.len(), 1);
        let w = &p.widgets[0];
        assert_eq!(w.line_range, 0..3, "覆盖含围栏的全部逻辑行");
        assert_eq!(
            w.kind,
            BlockWidgetKind::Code {
                lang: Some("rust".to_string()),
                fenced: true,
            }
        );
    }

    #[test]
    fn fenced_code_block_without_lang() {
        let p = build("```\nplain\n```\n");
        assert_eq!(p.widgets.len(), 1);
        assert_eq!(
            p.widgets[0].kind,
            BlockWidgetKind::Code {
                lang: None,
                fenced: true,
            }
        );
    }

    #[test]
    fn table_lines_mark_header_and_delimiter() {
        let p = build("| a | b |\n| --- | --- |\n| 1 | 2 |\n");
        assert_eq!(p.lines[0].block.table_line, Some(TableLine::Header));
        assert_eq!(p.lines[1].block.table_line, Some(TableLine::Delimiter));
        assert_eq!(p.lines[2].block.table_line, Some(TableLine::Body));
    }

    /// 表格行给出单元格的显示字节范围（不含竖线与两端空格），供 UI 按列排版。
    #[test]
    fn table_cells_exclude_pipes_and_padding() {
        let p = build("| a | 中 b |\n| --- | --- |\n| 1 | 2 |\n");
        let l = &p.lines[0];
        assert_eq!(l.display_text(), "| a | 中 b |");
        let cells = l.table_cells.as_ref().expect("表格行应有单元格");
        assert_eq!(cells.len(), 2);
        let text = l.display_text();
        assert_eq!(&text[cells[0].clone()], "a");
        assert_eq!(&text[cells[1].clone()], "中 b");
        // 分隔行显示为空，没有可见单元格
        assert_eq!(p.lines[1].table_cells.as_deref(), Some(&[][..]));
        // 表体行也带单元格
        assert_eq!(p.lines[2].table_cells.as_ref().map(Vec::len), Some(2));
    }

    /// 单元格内的转义竖线不应被当作分隔符。
    #[test]
    fn table_cells_keep_escaped_pipe() {
        let p = build("| a \\| b | c |\n| --- | --- |\n| 1 | 2 |");
        let l = &p.lines[0];
        let text = l.display_text();
        let cells = l.table_cells.clone().expect("表格行应有单元格");
        assert_eq!(cells.len(), 2, "显示文本: {text:?}");
        assert_eq!(&text[cells[0].clone()], "a | b");
        assert_eq!(&text[cells[1].clone()], "c");
    }

    #[test]
    fn table_is_block_widget_with_align() {
        let p = build("| a | b |\n| :- | -: |\n| 1 | 2 |\n");
        assert_eq!(p.widgets.len(), 1);
        let w = &p.widgets[0];
        assert_eq!(w.line_range, 0..3);
        assert_eq!(
            w.kind,
            BlockWidgetKind::Table {
                align: vec![Align::Left, Align::Right],
            }
        );
    }

    #[test]
    fn thematic_break_is_rule_widget() {
        let p = build("---");
        assert_eq!(p.widgets.len(), 1);
        assert_eq!(p.widgets[0].kind, BlockWidgetKind::Rule);
        assert_eq!(p.widgets[0].line_range, 0..1);
    }

    #[test]
    fn widgets_are_source_ordered() {
        let p = build("```\na\n```\n\n---\n\n```\nb\n```\n");
        assert_eq!(p.widgets.len(), 3);
        assert_eq!(
            p.widgets[0].kind,
            BlockWidgetKind::Code {
                lang: None,
                fenced: true,
            }
        );
        assert_eq!(p.widgets[1].kind, BlockWidgetKind::Rule);
        assert_eq!(
            p.widgets[2].kind,
            BlockWidgetKind::Code {
                lang: None,
                fenced: true,
            }
        );
        assert!(p.widgets[0].source_range.start < p.widgets[1].source_range.start);
        assert!(p.widgets[1].source_range.start < p.widgets[2].source_range.start);
    }

    #[test]
    fn plain_paragraph_has_no_block_widget() {
        let p = build("hello\n\nworld");
        assert!(p.block_widgets().is_empty());
        assert!(p.block_widget_at(0).is_none());
        assert!(p.block_widget_on_line(0).is_none());
    }

    #[test]
    fn inline_image_does_not_make_block_widget() {
        // 图片是**内联**占位（ProjectedLine.widgets），不产生块级 widget
        let p = build("![alt](x.png)");
        assert!(p.block_widgets().is_empty());
        assert_eq!(p.lines[0].widgets.len(), 1);
    }

    #[test]
    fn block_widget_lookup_by_source_and_line() {
        let p = build("```rust\nlet x = 1;\n```\n");
        // 源偏移 0（开围栏起点）落在 widget 内；末偏移等于 range.end 视为外部
        assert!(p.block_widget_at(0).is_some());
        assert!(p.block_widget_at(p.widgets[0].source_range.end).is_none());
        // 逻辑行 1（内容行）与 0（开围栏行）都属于 widget
        assert!(p.block_widget_on_line(0).is_some());
        assert!(p.block_widget_on_line(1).is_some());
    }

    #[test]
    fn code_block_inside_quote_is_block_widget() {
        // 容器（引用）透传，内部的围栏代码块仍产出 1 个 widget
        let p = build("> ```rust\n> let x = 1;\n> ```\n");
        assert!(
            p.widgets
                .iter()
                .any(|w| matches!(&w.kind, BlockWidgetKind::Code { .. })),
            "引用内的代码块应产出 Code widget"
        );
    }

    // ---------- 公式/图表 widget（D34） ----------

    #[test]
    fn math_blocks_are_block_widgets() {
        // 单行与多行 $$ 公式各产出 1 个 Math widget，覆盖含定界行的全部逻辑行
        let p = build("$$E=mc^2$$\n\n$$\na\n$$\n");
        assert_eq!(p.widgets.len(), 2, "{:#?}", p.widgets);
        assert_eq!(p.widgets[0].kind, BlockWidgetKind::Math);
        assert_eq!(p.widgets[0].line_range, 0..1);
        assert_eq!(p.widgets[1].kind, BlockWidgetKind::Math);
        assert_eq!(p.widgets[1].line_range, 2..5);
    }

    #[test]
    fn math_block_hides_delimiters_in_display_text() {
        // 显示文本只剩公式体：`$$` 是隐藏定界符
        let p = build("$$E=mc^2$$");
        assert_eq!(p.lines[0].display_text(), "E=mc^2");
        let p2 = build("> $$\n> x\n> $$\n");
        assert_eq!(
            p2.lines[1].display_text(),
            "x",
            "引用内的公式同样只显示公式体"
        );
    }

    #[test]
    fn mermaid_fence_is_chart_widget() {
        // mermaid 归类为 Chart（D34 ③），其余语言仍是 Code
        let p = build("```mermaid\ngraph TD;\n```\n");
        assert_eq!(p.widgets.len(), 1);
        assert_eq!(
            p.widgets[0].kind,
            BlockWidgetKind::Chart {
                lang: "mermaid".to_string()
            }
        );
        let p2 = build("```dot\ngraph{}\n```\n");
        assert_eq!(
            p2.widgets[0].kind,
            BlockWidgetKind::Code {
                lang: Some("dot".to_string()),
                fenced: true,
            },
            "非 mermaid 语言仍按代码块处理"
        );
    }

    #[test]
    fn inline_math_runs_use_math_style() {
        // 行内公式体是可见文本并带 RunStyle::Math（D34 ④）
        let p = build("当 $x^2$ 时");
        let run = p.lines[0]
            .runs
            .iter()
            .find(|r| r.text.contains('x'))
            .expect("应有含 x 的文本段");
        assert_eq!(run.style, RunStyle::Math);
        assert_eq!(run.text, "x^2");
        assert_eq!(
            p.lines[0].display_text(),
            "当 x^2 时",
            "`$` 定界符隐藏，公式体可见"
        );
    }

    #[test]
    fn footnote_ref_run_uses_footnote_style() {
        // 行内引用 `[^1]`：`[^`/`]` 隐藏、标签可见并带 RunStyle::Footnote（D35 ③）
        let p = build("见 [^1] 处");
        assert_eq!(p.lines[0].display_text(), "见 1 处");
        let run = p.lines[0]
            .runs
            .iter()
            .find(|r| r.text == "1")
            .expect("应有标签文本段");
        assert_eq!(run.style, RunStyle::Footnote);
    }

    #[test]
    fn footnote_def_is_plain_block_with_visible_label() {
        // 定义块：标签 + 正文都可见、样式为 Footnote/Normal，且**不产生块级 widget**（D35 ③）
        let p = build("[^1]: note\n");
        assert!(p.widgets.is_empty(), "定义块不应产生块级 widget");
        assert_eq!(p.lines[0].display_text(), "1 note");
        assert!(
            p.lines[0]
                .runs
                .iter()
                .any(|r| r.style == RunStyle::Footnote && r.text == "1")
        );
    }

    #[test]
    fn footnote_def_continuation_projects_two_lines() {
        // 投影 = 源文本逻辑行 + 末尾空行（SPEC §6.1）
        let p = build("[^a]: first\n    续行\n");
        assert_eq!(p.lines.len(), 3);
        assert_eq!(p.lines[0].display_text(), "a first");
        assert_eq!(p.lines[1].display_text(), "续行");
    }
}
