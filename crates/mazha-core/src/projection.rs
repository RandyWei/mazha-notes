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

use crate::offset_map::OffsetMap;
use mazha_md::{Block, BlockKind, SpanKind, SpanRole, SyntaxSpan};
use std::ops::Range;

/// 整篇文档的渲染投影。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Projection {
    /// 按源文本逻辑行排列（含末尾空行，便于光标落点）。
    pub lines: Vec<ProjectedLine>,
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
}

impl From<SpanRole> for RunStyle {
    fn from(role: SpanRole) -> Self {
        match role {
            SpanRole::Emphasis => RunStyle::Emphasis,
            SpanRole::Strong => RunStyle::Strong,
            SpanRole::Strikethrough => RunStyle::Strikethrough,
            SpanRole::InlineCode => RunStyle::InlineCode,
            SpanRole::LinkText => RunStyle::LinkText,
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

    Projection { lines: projected }
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
    };
    for idx in lines_overlapping(lines, &block.range) {
        styles[idx] = style.clone();
    }
}

/// 递归收集块森林的全部片段（容器标记 + 叶子内容）。
fn collect_spans(block: &Block, out: &mut Vec<SyntaxSpan>) {
    out.extend(block.spans.iter().cloned());
    for c in &block.children {
        collect_spans(c, out);
    }
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
            let chars = text.chars().count();
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

    ProjectedLine {
        source_range: line_range,
        block,
        runs,
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
}
