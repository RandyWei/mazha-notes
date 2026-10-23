//! 码札桌面端（Phase 2：只读投影渲染 + 光标/标记显隐 + 跨行选区）。见 `SPEC.md` §7.1、§11。
//!
//! 管线：`.md` 源文本 → [`Document`]（源文本真相 + 投影缓存）→ `Projection`
//! （块样式 + 带样式可见文本片段 + 图片占位）→ 自写 [`DocElement`] 用 `shape_text`
//! 逐块软换行并连续绘制。
//!
//! 本里程碑已具备：
//! - **只读投影渲染**：语法标记默认隐藏（由 core 投影决定，UI 不解析 Markdown）；
//! - **光标**：点击文本放置光标（整篇命中 → [`OffsetMap`] → 源字节偏移），
//!   方向键/Home/End 移动；
//! - **WYSIWYG 标记显隐**（SPEC §11.2）：默认渲染投影（标记隐藏）；光标落在链接、
//!   粗体、斜体、删除线、行内代码内时，**该格式组**临时展开为原始 Markdown
//!   （`inline_reveals` / `reveal_inline`），离开后复原；整行原文显示（§11.2 全量版）仍留后续；
//! - **链接打开**：Cmd+点击链接调系统默认浏览器（`DocView::link_at`）；
//! - **跨行选区**（SPEC §11.2）：鼠标拖选 + Shift+方向键，选区为源字节 `Range<usize>`，
//!   经核心 `Projection::selection_display_ranges` 折算成每行显示列区间再画高亮。
//! - **多光标**：Alt+Click 增删光标，编辑命令对所有光标生效。
//! - **块级 widget**（SPEC §11.5、D30）：代码块/表格/分割线按核心
//!   `Projection::block_widgets` 聚成整块绘制——代码块面板描边 + 语言标签、表格外框 +
//!   行分隔线、分割线真实细线；光标进入块内则隐藏装饰（编辑态）。
//!
//! 行归属、上下移动（保留显示列）、选区折算、IME 候选定位统一走核心的文档级投影
//! （`Projection::{line_of_source, source_to_doc_pos, doc_pos_to_source,
//! selection_display_ranges}`），UI 侧不再自行维护偏移换算。
//!
//! **软换行**（SPEC §7.1、§11）：每条投影行用 `shape_text`（带 `wrap_width`）形状化为
//! 一个 [`WrappedLine`]——按内容宽度自动折成多个**视觉行**。布局几何按视觉行累计
//! （`DocLayout::row_starts`），命中测试、光标、选区、IME 候选框、块级 widget 装饰
//! 均以视觉行为单位；光标/选区仍以逻辑行 + 显示**字符列**表示，经核心
//! `ProjectedLine::display_byte_of_col`（↔ `display_col_of_byte`）与 `WrappedLine` 的
//! 字节索引互换（引用 "> " 前缀等额外文本以 `prefix_bytes` 计入）。

use rfd::AsyncFileDialog;
use std::cmp::Reverse;
use std::ops::Range;
use std::panic::Location;
use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::base::h_flex;
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::{ActiveTheme, Theme};
use gpui_kit::prelude::*;
use gpui_kit::*;
use mazha_core::{
    BlockStyle, BlockTarget, BlockWidgetKind, ColoredRun, DisplayPos, DocPos, Document, Edit,
    Highlighter, InlineMark, OffsetMap, ProjectedLine, Projection, RunStyle, Selection, TableOp,
    TextRun as DocTextRun, Workspace,
    commands::{set_block, toggle_inline},
    edit_table, find_all, replace_all_edit,
};
use mazha_export::to_pdf;
use mazha_md::{SpanKind, SpanRole, parse_inline};
#[allow(unused_imports, dead_code)]
use mazha_store::{DocPath, FileSystemProvider};
mod oblique;

/// 未指定文件时的内置示例文档（覆盖主要语法，便于肉眼验收投影渲染）。
const SAMPLE: &str = "\
# 码札 Mazha

一款 *Typora 式* 的 *WYSIWYG* Markdown 编辑器，纯 Rust 构建。

## 功能清单

- 基础语法：**粗体**、*斜体*、~~删除线~~、`行内代码`
- 链接：[码札仓库](https://github.com/RandyWei/mazha-notes) 与自动链接 <https://example.com>
- 知识库扩展：[[Page|别名]] 与 #标签 #嵌套/标签

### 任务与列表

1. 有序项一
2. 有序项二
   - 嵌套无序
   - 第二个嵌套

- [x] 已完成任务
- [ ] 待办任务

### 引用

> 普通引用
> > 深层引用仍正确缩进

### 表格

| 左对齐 | 居中 | 右对齐 |
| :-- | :-: | --: |
| 甲 | 乙 | 丙 |

### 代码块

```rust
fn main() {
    println!(\"码札 — 纯 Rust Markdown 编辑器\");
}
```

### 其他

![示例图片](assets/sample.png)

---
";

/// 一组渲染用主题色（render 时从主题取快照，供子元素闭包捕获）。
#[derive(Debug, Clone, Copy)]
struct Palette {
    /// 正文前景色。
    foreground: Hsla,
    /// 次要文字（列表符号、占位等）。
    muted: Hsla,
    /// 链接文字色。
    link: Hsla,
    /// 强调色（引用竖线、行内代码文字、光标）。
    accent: Hsla,
    /// 边框/分割线。
    border: Hsla,
    /// 背景。
    background: Hsla,
    /// 弱底色（图片占位、表格/图表块背景）。
    subtle: Hsla,
    /// 行内代码底色（经 `TextRun.background_color` 由 gpui 逐视觉行绘制）。
    code_bg: Hsla,
    /// 代码块面板浅底（比 subtle 更淡，编辑态也要铺）。
    code_block_bg: Hsla,
    /// 引用块整行浅底（比 `subtle` 更淡，避免压过正文）。
    quote_bg: Hsla,
    /// 选区高亮底色（accent 的低透明度）。
    selection: Hsla,
}

impl Palette {
    /// 从当前主题取色（`ActiveTheme::theme`，经 Deref 到 `ThemeColor`）。
    fn from_theme(theme: &Theme) -> Self {
        Self {
            foreground: theme.foreground,
            muted: theme.muted_foreground,
            link: theme.link,
            accent: theme.accent,
            border: theme.border,
            background: theme.background,
            // 弱底色：取 muted 的低透明度版本，深浅主题都可见
            subtle: Hsla {
                a: 0.25,
                ..theme.muted
            },
            // 行内代码底色：前景色低透明度，浅色主题也可见
            code_bg: Hsla {
                a: 0.08,
                ..theme.foreground
            },
            // 代码块浅底：前景色极低透明度
            code_block_bg: Hsla {
                a: 0.06,
                ..theme.foreground
            },
            // 引用块底色：由**前景色**极低透明度混合（只需刚好能和正文区分）
            quote_bg: Hsla {
                a: 0.04,
                ..theme.foreground
            },
            // 选区底色：accent 的低透明度，跟随强调色随主题变化
            selection: Hsla {
                a: 0.25,
                ..theme.accent
            },
        }
    }

    /// 单测用调色板：不依赖全局主题，各字段给中性值即可。
    #[cfg(test)]
    fn default_for_test() -> Self {
        let fg = Hsla {
            h: 0.,
            s: 0.,
            l: 0.,
            a: 1.,
        };
        Self {
            foreground: fg,
            muted: fg,
            link: fg,
            accent: fg,
            border: fg,
            background: fg,
            subtle: fg,
            code_bg: fg,
            code_block_bg: fg,
            quote_bg: fg,
            selection: fg,
        }
    }
}

/// 一行内可展开/可点击的链接组：`[文字](目标)`（偏移均**相对行首**）。
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct InlineLink {
    /// 整组源区间（`[` 起、`)` 止）。
    range: Range<usize>,
    /// 目标 URL 区间（已去除两侧空白；空目标不产生链接组）。
    dest: Range<usize>,
    /// 组内被投影隐藏、展开时需要还原为可见文本的片段（`[` / `](` / URL / `)`）。
    markers: Vec<Range<usize>>,
}

/// 行内可展开的格式组（粗体/斜体/删除线/行内代码），偏移均**相对行首**。
#[derive(Debug, Clone, PartialEq, Eq)]
struct InlineReveal {
    /// 整组源区间。
    range: Range<usize>,
    /// 需要展开的标记片段（如 `**`、`*`、`~~`、`` ` ``）。
    markers: Vec<Range<usize>>,
    /// 链接目标（仅链接组有值，用于 Cmd+点击）。
    link_dest: Option<Range<usize>>,
}

/// 解析一行源文本里的完整链接组（输出偏移相对行首）。
#[allow(dead_code)]
fn inline_links(line_src: &str) -> Vec<InlineLink> {
    let spans = parse_inline(line_src, 0..line_src.len());
    let markers: Vec<&mazha_md::SyntaxSpan> = spans
        .iter()
        .filter(|s| s.role == SpanRole::LinkMarker)
        .collect();
    let mut links = Vec::new();
    // 每个链接恰好 3 个 LinkMarker，按序成组
    for chunk in markers.as_chunks::<3>().0 {
        let (open, mid, close) = (chunk[0], chunk[1], chunk[2]);
        // 结构校验：防御解析器行为变化，避免误把其它标记当链接
        if line_src.get(open.range.clone()) != Some("[")
            || line_src.get(mid.range.clone()) != Some("](")
            || line_src.get(close.range.clone()) != Some(")")
        {
            continue;
        }
        let Some(dest_span) = spans.iter().find(|s| {
            s.role == SpanRole::LinkDest
                && s.range.start >= mid.range.end
                && s.range.end <= close.range.start
        }) else {
            continue;
        };
        let dest = trim_ascii_range(line_src, dest_span.range.clone());
        if dest.is_empty() {
            continue;
        }
        let mut marks = vec![
            open.range.clone(),
            mid.range.clone(),
            close.range.clone(),
            dest_span.range.clone(),
        ];
        marks.sort_by_key(|r| r.start);
        links.push(InlineLink {
            range: open.range.start..close.range.end,
            dest,
            markers: marks,
        });
    }
    links
}

/// 解析行内可展开的格式组（链接、粗体、斜体、删除线、行内代码）。
fn inline_reveals(line_src: &str) -> Vec<InlineReveal> {
    let spans = parse_inline(line_src, 0..line_src.len());
    let mut reveals = Vec::new();

    // 链接组
    let markers: Vec<&mazha_md::SyntaxSpan> = spans
        .iter()
        .filter(|s| s.role == SpanRole::LinkMarker)
        .collect();
    for chunk in markers.as_chunks::<3>().0 {
        let (open, mid, close) = (chunk[0], chunk[1], chunk[2]);
        if line_src.get(open.range.clone()) != Some("[")
            || line_src.get(mid.range.clone()) != Some("](")
            || line_src.get(close.range.clone()) != Some(")")
        {
            continue;
        }
        let Some(dest_span) = spans.iter().find(|s| {
            s.role == SpanRole::LinkDest
                && s.range.start >= mid.range.end
                && s.range.end <= close.range.start
        }) else {
            continue;
        };
        let dest = trim_ascii_range(line_src, dest_span.range.clone());
        if dest.is_empty() {
            continue;
        }
        let mut marks = vec![
            open.range.clone(),
            mid.range.clone(),
            close.range.clone(),
            dest_span.range.clone(),
        ];
        marks.sort_by_key(|r| r.start);
        reveals.push(InlineReveal {
            range: open.range.start..close.range.end,
            markers: marks,
            link_dest: Some(dest),
        });
    }

    // 粗体、斜体、删除线、行内代码：标记与内容同角色，标记为 Marker
    for span in &spans {
        if span.kind != SpanKind::Marker {
            continue;
        }
        let role = span.role;
        if !matches!(
            role,
            SpanRole::Strong | SpanRole::Emphasis | SpanRole::Strikethrough | SpanRole::InlineCode
        ) {
            continue;
        }
        // 找到同角色的一对标记，确定范围
        let _pair_start: Option<usize> = None;
        let _pair_end: Option<usize> = None;
        // 简单做法：收集所有同角色 Marker，成对处理
        // 但更准确的是找到与此标记配对的另一个
    }

    // 收集配对的标记
    let mut used = vec![false; spans.len()];
    for i in 0..spans.len() {
        if used[i] {
            continue;
        }
        let s = &spans[i];
        if s.kind != SpanKind::Marker {
            continue;
        }
        let role = s.role;
        if !matches!(
            role,
            SpanRole::Strong | SpanRole::Emphasis | SpanRole::Strikethrough | SpanRole::InlineCode
        ) {
            continue;
        }
        // 找配对的同角色 Marker
        if let Some(j) = (i + 1..spans.len())
            .find(|&k| !used[k] && spans[k].kind == SpanKind::Marker && spans[k].role == role)
        {
            used[i] = true;
            used[j] = true;
            let r1 = spans[i].range.clone();
            let r2 = spans[j].range.clone();
            let start = r1.start.min(r2.start);
            let end = r1.end.max(r2.end);
            let mut marks = vec![r1, r2];
            marks.sort_by_key(|r| r.start);
            reveals.push(InlineReveal {
                range: start..end,
                markers: marks,
                link_dest: None,
            });
        }
    }

    reveals.sort_by_key(|r| r.range.start);
    reveals
}

/// 把区间两端的 ASCII 空白裁掉（链接目标常写成 `( https://x )`）。
fn trim_ascii_range(s: &str, mut r: Range<usize>) -> Range<usize> {
    let bytes = s.as_bytes();
    while r.start < r.end && bytes[r.start].is_ascii_whitespace() {
        r.start += 1;
    }
    while r.end > r.start && bytes[r.end - 1].is_ascii_whitespace() {
        r.end -= 1;
    }
    r
}

/// 生成"原文展开"的行副本：把标记还原成可见文本（链接、粗体、斜体、删除线、行内代码）。
///
/// 只改 `runs`（追加标记片段）与 `offset_map`（按新可见区间重建），`source_range`
/// 与块样式不变。标记片段按源偏移与既有 run 归并，并重算 `display_start`。
/// 重建的 `offset_map` 使用**行内相对偏移**（`from_visible_runs` 只需要行内文本做
/// 边界校验），绝对映射由 [`BlockGeom::source_of_col`] 加回行起点。
///
/// 返回 `None` 表示该行不宜展开（见函数内两条守卫），调用方继续按投影渲染。
fn reveal_inline(
    line: &ProjectedLine,
    line_src: &str,
    reveal: &InlineReveal,
) -> Option<ProjectedLine> {
    let base = line.source_range.start;
    // 守卫 1：标记必须确实被投影隐藏。代码块整行都是可见文本（不解析行内语法），
    // 若照插会把标记重复显示一遍。
    let markers_hidden = reveal.markers.iter().all(|m| {
        let (a, b) = (base + m.start, base + m.end);
        !line
            .runs
            .iter()
            .any(|r| r.source_range.start < b && a < r.source_range.end)
    });
    // 守卫 2：没有可见片段（Front-matter 元数据行、公式块内容行）时，不展开。
    if !markers_hidden || line.runs.is_empty() {
        return None;
    }
    let mut inserts: Vec<DocTextRun> = reveal
        .markers
        .iter()
        .filter_map(|r| {
            let text = line_src.get(r.clone())?;
            (!text.is_empty()).then(|| DocTextRun {
                source_range: (base + r.start)..(base + r.end),
                display_start: 0,
                style: RunStyle::Normal,
                text: text.to_string(),
            })
        })
        .collect();
    inserts.sort_by_key(|r| r.source_range.start);

    // 按源顺序归并：插入的标记片段与投影可见片段不会重叠（标记本就被隐藏）
    let mut runs: Vec<DocTextRun> = Vec::with_capacity(line.runs.len() + inserts.len());
    let mut chars = 0usize;
    let mut ins = inserts.into_iter().peekable();
    let push = |runs: &mut Vec<DocTextRun>, mut r: DocTextRun, chars: &mut usize| {
        r.display_start = *chars;
        *chars += r.text.chars().count();
        runs.push(r);
    };
    for run in &line.runs {
        while ins
            .peek()
            .is_some_and(|next| next.source_range.start < run.source_range.start)
        {
            let Some(next) = ins.next() else { break };
            push(&mut runs, next, &mut chars);
        }
        push(&mut runs, run.clone(), &mut chars);
    }
    for next in ins {
        push(&mut runs, next, &mut chars);
    }

    let visible: Vec<Range<usize>> = runs
        .iter()
        .map(|r| (r.source_range.start - base)..(r.source_range.end - base))
        .collect();
    let offset_map =
        OffsetMap::from_visible_runs(line_src, &visible).unwrap_or_else(|_| OffsetMap::empty());
    let mut out = line.clone();
    out.runs = runs;
    out.offset_map = offset_map;
    Some(out)
}

/// 一个投影块的布局几何（供命中测试与选区绘制）。
#[derive(Clone)]
struct BlockGeom {
    /// 本块的投影行（`runs`/`offset_map`；供显示列↔字节、源偏移换算）。
    proj: ProjectedLine,
    /// 已形状化并**软换行**的行（行内相对坐标；`position_for_index` 按行高定位视觉行）。
    ///
    /// 用 `Arc` 包裹：`WrappedLine` 不实现 `Clone`，而 [`DocLayout`] 需可克隆回存视图。
    shaped: Arc<WrappedLine>,
    /// 本块的视觉行数（`>= 1`）。
    rows: usize,
    /// 前缀字节数（引用 `"> "` 等投影外文本；显示列→形状文本字节需加上它）。
    prefix_bytes: usize,
    /// 左缩进（引用深度、列表缩进），行内 x 坐标需加上它才是块内绝对 x。
    indent: Pixels,
    /// 本块是否处于"原文展开"态（光标在链接/粗体/斜体/删除线/行内代码内）。
    ///
    /// 展开态的 `proj` 是 [`reveal_inline`] 生成的副本：标记重新变为可见文本
    /// （同行其它标记仍隐藏）；`offset_map` 以**行首为基准**
    /// （见 [`BlockGeom::source_of_col`] 的加回逻辑）。
    revealed: bool,
    /// 光标在本块时的行内**显示列**（`None` = 光标不在本块；仅一块为 `Some`）。
    ///
    /// 由 `Projection::source_to_doc_pos` 于 prepaint 阶段算好，paint 直接据此画竖线。
    caret_col: Option<usize>,
    /// 本块在当前选区内的**显示列**区间（`None` 表示本块不在选区内）。
    ///
    /// 由 `Projection::selection_display_ranges` 于 prepaint 阶段算好，paint 直接按
    /// 视觉行换算屏幕 x（经 `display_byte_of_col`）。
    selection: Option<Range<usize>>,
    /// 本块**每个视觉行**的高度：正文 = [`LINE_HEIGHT`]，标题/代码按字号放大。
    ///
    /// 标题字号大于正文，若沿用统一行高会与相邻行重叠，故逐块记录；
    /// 命中测试、选区、光标、绘制都以它为准（块内各行同高）。
    line_height: Pixels,
    /// 列表符号（`•` / `N.`；已在 prepaint shape 好，任务项为 `None`——改画复选框）。
    ///
    /// 绘制在 [`BlockGeom::indent`] 左侧预留的标记区内（见 [`block_indent`]）。
    marker: Option<Box<ShapedLine>>,
    /// 任务项的勾（`✓`，已完成时画在复选框内；未完成为 `None`）。
    check: Option<Box<ShapedLine>>,
    /// 行内图片占位小标签：`(窗口坐标 (x, y), 已 shape 文本)`。
    ///
    /// `![alt](url)` 整段被投影隐藏，若不画占位屏幕上就是空白，故补一个可见标签。
    image_chips: Vec<(Point<Pixels>, Box<ShapedLine>)>,
    /// 需自绘合成倾斜的中文片段（相对行文本字节区间）。
    ///
    /// 这些片段在 gpui 侧是透明占位（保留排版/光标/选区），字形轮廓在 paint 阶段
    /// 按视觉行基线现取现画，避免在布局阶段重复计算几何。
    oblique_spans: Vec<oblique::ObliqueSpan>,
    /// 本块字号（自绘斜体取轮廓用）。
    font_size: Pixels,
    /// 围栏行在"编辑代码块"时的源码显示文本（含 ``` / ```lang）。
    ///
    /// 围栏行投影后显示文本为空（整行都是隐藏标记），光标落上去就"看不见"。
    /// 编辑该代码块时把源码直接画出来，并令显示列与源字节 1:1 映射。
    fence_src: Option<SharedString>,
}

/// 字符列 → UTF-8 字节偏移（越界钳到末尾）。
fn char_col_to_byte(s: &str, col: usize) -> usize {
    s.char_indices().nth(col).map_or(s.len(), |(i, _)| i)
}

impl BlockGeom {
    /// 显示列 → 形状化文本内的字节偏移（含前缀）。
    fn byte_of_col(&self, col: usize) -> usize {
        if let Some(src) = &self.fence_src {
            return char_col_to_byte(src, col);
        }
        self.prefix_bytes + self.proj.display_byte_of_col(col)
    }

    /// 形状化文本内的字节偏移 → 显示列（扣除前缀，越界钳到行末）。
    fn col_of_byte(&self, byte: usize) -> usize {
        if let Some(src) = &self.fence_src {
            let byte = byte.min(src.len());
            return src.get(..byte).map_or(0, |s| s.chars().count());
        }
        self.proj
            .display_col_of_byte(byte.saturating_sub(self.prefix_bytes))
    }

    /// 显示列 → 源字节偏移（钳在本块 `source_range` 内）。
    ///
    /// 空行、以及只含被隐藏语法的行（图片行、围栏行、分割线等）的 `OffsetMap` 没有任何
    /// 可落点，`display_to_source` 会退回 0（文档开头），导致点击或上下移动误跳文首；
    /// 这里显式落到本行起点。非空行也把结果钳进本行范围，避免跨行串位。
    fn source_of_col(&self, col: usize) -> usize {
        if self.fence_src.is_some() {
            return (self.proj.source_range.start + self.byte_of_col(col))
                .min(self.proj.source_range.end);
        }
        if self.proj.offset_map.display_len() == 0 {
            return self.proj.source_range.start;
        }
        let raw = self.proj.offset_map.display_to_source(DisplayPos(col));
        // 展开态的映射以行首为基准（行内文本做字符边界校验），补回行起点
        let src = if self.revealed {
            self.proj.source_range.start + raw
        } else {
            raw
        };
        src.clamp(self.proj.source_range.start, self.proj.source_range.end)
    }

    /// 源字节偏移 → 本块显示列（供光标定位、IME 候选框、上下移动取列）。
    ///
    /// 两种状态都经 `OffsetMap::source_to_display`：展开态以行首为基准，投影态用绝对
    /// 偏移；落在隐藏标记内时吸附到标记起点（与光标规则一致）。
    fn col_of_source(&self, src: usize) -> usize {
        if self.fence_src.is_some() {
            let rel = src.saturating_sub(self.proj.source_range.start);
            return self.col_of_byte(rel);
        }
        let src = src.clamp(self.proj.source_range.start, self.proj.source_range.end);
        // 展开态的 OffsetMap 以行首为基准；投影态是整篇绝对偏移
        let base = if self.revealed {
            self.proj.source_range.start
        } else {
            0
        };
        self.proj.offset_map.source_to_display(src - base).get()
    }
}

/// 整篇文档的布局几何：块序列 + 光标原点 + 视觉行顶边表 + 视觉行累积。
///
/// 在 [`DocElement::prepaint`] 中构建，经 `cx.defer` 回存到 [`DocView`]，
/// 供鼠标命中测试在事件回调里读取。
#[derive(Clone)]
struct DocLayout {
    /// 块序列（与投影行一一对应，已跳过分割线等无文本块）。
    blocks: Vec<BlockGeom>,
    /// 块级 widget 的装饰（代码块/表格/分割线；SPEC §11.5、D30）。
    widgets: Vec<WidgetDecor>,
    /// 内容左上角（窗口坐标，已含滚动偏移）。
    origin: Point<Pixels>,
    /// 基准行高（正文字号对应的行高；逐块实际行高见 [`BlockGeom::line_height`]）。
    line_height: Pixels,
    /// 每个块**首视觉行**的全局行号；末位为总视觉行数（长度 = `blocks.len() + 1`）。
    ///
    /// 软换行下逻辑行 ≠ 视觉行：命中测试/绘制都以它把"全局视觉行"映射回块。
    row_starts: Vec<usize>,
    /// 每个全局**视觉行**的顶边（内容坐标 y；长度 = 总视觉行数 + 1，末位 = 内容总高）。
    ///
    /// 标题按字号放大后行高不一，不能用"行号 × 统一行高"换算 y，故用累计顶边表
    /// 二分定位（见 [`DocLayout::row_at`]）。
    row_tops: Vec<Pixels>,
    /// 光标所在视觉行的 `(顶边, 行高)`（内容坐标）；`None` = 布局时尚无光标。
    ///
    /// 供编辑区滚动跟随（见 [`DocView::caret_scroll_key`]）。
    caret_row: Option<(Pixels, Pixels)>,
    /// 任务复选框：`(窗口坐标矩形, `[ ]`/`[x]` 的源字节区间)`；点击可切换完成状态。
    task_boxes: Vec<(Bounds<Pixels>, Range<usize>)>,
    /// 滚动视口（滚动容器的窗口坐标 bounds；prepaint 从 `ScrollHandle` 读取）。
    ///
    /// 内容随滚动上移后仅凭 `origin` 无法区分"标签栏/信息条"与"已滚出视口的内容"，
    /// 命中测试用它把点击先限定在编辑区视口内。
    viewport: Bounds<Pixels>,
}

impl DocLayout {
    /// 内容总高度（末行顶边 + 底部留白）；`DocElement` 据此上报可滚动高度。
    fn total_height(&self) -> Pixels {
        self.row_tops.last().copied().unwrap_or(Pixels::ZERO) + BASE
    }

    /// 把**内容坐标** y 映射到全局视觉行（越界钳到首/末行）。
    fn row_at(&self, y: Pixels) -> Option<usize> {
        let total = *self.row_starts.last()?;
        if total == 0 {
            return None;
        }
        // row_tops 升序且末位是内容高度（不含留白）：取最后一个 <= y 的下标再钳到 [0, total-1]
        let idx = self.row_tops.partition_point(|&t| t <= y);
        Some(idx.saturating_sub(1).min(total - 1))
    }

    /// 某全局视觉行的顶边（内容坐标）。
    fn row_top(&self, row: usize) -> Pixels {
        self.row_tops.get(row).copied().unwrap_or(Pixels::ZERO)
    }

    /// 某全局视觉行的行高（顶边表相邻差；越界退化为基准行高）。
    fn row_height(&self, row: usize) -> Pixels {
        match (self.row_tops.get(row), self.row_tops.get(row + 1)) {
            (Some(&a), Some(&b)) => b - a,
            _ => self.line_height,
        }
    }

    /// 块首视觉行的行高（块内各行同高；越界退化为基准行高）。
    fn block_line_height(&self, block_idx: usize) -> Pixels {
        self.row_starts
            .get(block_idx)
            .map_or(self.line_height, |&r| self.row_height(r))
    }

    /// 从窗口坐标命中一个**显示位置**（块序号 + 行内显示字符列）。
    ///
    /// 软换行下先由 `row_tops`/`row_starts` 把窗口 y 二分定位到块与块内视觉行，
    /// 再取块内相对坐标交给 [`WrappedLine::closest_index_for_position`] 求字节偏移，
    /// 最后换算成显示列。
    fn hit(&self, position: Point<Pixels>) -> Option<(usize, usize)> {
        if self.blocks.is_empty() {
            return None;
        }
        // 先限定在编辑区视口内：标签栏/信息条与已滚出视口的内容都不放光标
        if position.x < self.viewport.left()
            || position.x > self.viewport.right()
            || position.y < self.viewport.top()
            || position.y > self.viewport.bottom()
        {
            return None;
        }
        let rel_y = position.y - self.origin.y;
        // 落在内容末尾以下也不放光标（行映射会钳到末行，但那会误改光标位置）
        if rel_y < Pixels::ZERO || rel_y >= self.total_height() {
            return None;
        }
        let row = self.row_at(rel_y)?;
        // row_starts[i] <= row < row_starts[i+1] → 块 i（首元素为 0，故减一后非负）
        let block_idx = self.row_starts.partition_point(|&s| s <= row) - 1;
        let block = &self.blocks[block_idx];
        let line_px = self.block_line_height(block_idx);
        // 块内相对 y（含所在视觉行的偏移；行内 x 需再减块缩进）
        let block_top = self.row_top(self.row_starts[block_idx]);
        let rel_x = position.x - self.origin.x - block.indent;
        let local_y = rel_y - block_top;
        let byte = block
            .shaped
            .closest_index_for_position(point(rel_x, local_y), line_px)
            .unwrap_or_else(|ix| ix);
        Some((block_idx, block.col_of_byte(byte)))
    }
}

/// 块级 widget 的绘制装饰（由核心 [`mazha_core::Projection::block_widgets`] 投影而来）。
#[derive(Clone)]
struct WidgetDecor {
    /// 装饰类型（决定画什么）。
    kind: WidgetDecorKind,
    /// 覆盖的逻辑行范围（与 `blocks` 下标一致；半开）。
    line_range: Range<usize>,
    /// 左侧缩进（与首行块缩进一致）。
    indent: Pixels,
    /// 光标是否位于其内（编辑态：不画装饰，露出可编辑内容）。
    editing: bool,
}

/// 装饰类型。
#[derive(Clone)]
enum WidgetDecorKind {
    /// 代码块：面板描边 + 右上角语言标签（已 shape，直接 paint）。
    Code {
        /// 语言标签（围栏信息串；无则不画）。装箱以避免枚举变体过大。
        label: Option<Box<ShapedLine>>,
    },
    /// 表格：外框 + 行分隔线。
    Table,
    /// 分割线：一条水平细线。
    Rule,
    /// 显示公式块（D34）：外框（公式体文字自身为斜体主题色）。
    Math,
    /// 图表块（如 mermaid，D34）：弱底 + 外框 + 右上角语言标签。
    /// 真正的图表渲染（mermaid → SVG）属渲染层后续（风险 R4），此处只画占位面板。
    Chart {
        /// 语言标签（如 `mermaid`；已 shape，直接 paint）。装箱以避免枚举变体过大。
        label: Option<Box<ShapedLine>>,
    },
}

/// 在软换行布局下，把某处位置向上/向下移动一个**视觉行**，尽量保留水平像素位置。
///
/// 返回新的源字节偏移；已是全文首/末个视觉行时返回 `None`（调用方保持原位）。
///
/// 计算步骤（视觉行 ≠ 逻辑行，故不能只按逻辑行 +1/-1）：
/// 1. 用投影把 `caret` 换成 (逻辑行, 显示列)，经 `byte_of_col` + `position_for_index`
///    反查出该处所在**块内视觉行**与块内 x，加上块缩进得**绝对 x**；
/// 2. 绝对视觉行 = `row_starts[逻辑行] + 块内视觉行`，上/下一行即 ±1（越界返回 `None`）；
/// 3. 目标绝对视觉行经 `row_starts` 二分定位到块与块内行，绝对 x 扣掉目标块缩进得块内 x；
/// 4. 用 `closest_index_for_position` 在目标视觉行上取最近字节 → 显示列 → 源偏移。
fn visual_vertical_target(
    layout: &DocLayout,
    projection: &Projection,
    caret: usize,
    step: isize,
) -> Option<usize> {
    let pos = projection.source_to_doc_pos(caret);
    let block = layout.blocks.get(pos.line)?;
    let line_px = layout.block_line_height(pos.line);
    // 用块几何取列：展开态（光标在链接内）显示文本更长，源列不能直接用投影列
    let byte = block.byte_of_col(block.col_of_source(caret));
    let p = block.shaped.position_for_index(byte, line_px)?;
    // position_for_index 的 y = 视觉行号 × 行高（行顶），除以该块行高即得块内视觉行
    let cur_row = (p.y / line_px) as usize;
    let abs_x = block.indent + p.x;
    let global = *layout.row_starts.get(pos.line)? + cur_row;
    let total = *layout.row_starts.last()?;
    let target_global = if step < 0 {
        global.checked_sub(1)?
    } else if global + 1 < total {
        global + 1
    } else {
        return None;
    };
    let block_idx = layout.row_starts.partition_point(|&s| s <= target_global) - 1;
    let tblock = layout.blocks.get(block_idx)?;
    let local_row = target_global - *layout.row_starts.get(block_idx)?;
    let local_x = (abs_x - tblock.indent).max(Pixels::ZERO);
    // 目标行行高可能与当前块不同（如正文→标题），取目标块行高算行中点
    let tline_px = layout.block_line_height(block_idx);
    let local_y = tline_px * local_row as f32 + tline_px / 2.;
    let tbyte = tblock
        .shaped
        .closest_index_for_position(point(local_x, local_y), tline_px)
        .unwrap_or_else(|ix| ix);
    let tcol = tblock.col_of_byte(tbyte);
    Some(tblock.source_of_col(tcol))
}

/// 主窗口内容：文档视图（投影渲染 + 多光标/选区）。
#[derive(Debug, Clone)]
struct Cursor {
    /// 光标位置（源字节偏移）。
    caret: usize,
    /// 选区锚点；`None` = 无选区（仅光标）。
    anchor: Option<usize>,
    /// IME 组合文本范围（仅主光标使用）。
    marked: Option<Range<usize>>,
}

impl Cursor {
    fn new(caret: usize) -> Self {
        Self {
            caret,
            anchor: None,
            marked: None,
        }
    }

    /// 当前选区范围（有序，含首不含尾）。
    fn selection(&self) -> Option<Range<usize>> {
        let a = self.anchor?;
        if a == self.caret {
            return None;
        }
        Some(a.min(self.caret)..a.max(self.caret))
    }
}

/// 查找/替换条的状态（Phase 3 编辑器扩展；SPEC §11.8）。
///
/// 查找/替换逻辑本身在核心 `mazha_core::{find_all, replace_all}`（纯函数、可单测）；
/// 本结构只存 UI 态（输入文本、当前匹配序号、聚焦哪个框）。
#[derive(Default)]
struct FindState {
    /// 查找条是否展开。
    open: bool,
    /// `true` = 焦点在替换框（`Tab` 切换）。
    focus_replace: bool,
    /// 是否区分大小写（默认 `false`，随按键切换）。
    case_sensitive: bool,
    /// 查找输入框文本。
    query: String,
    /// 替换输入框文本。
    replace: String,
    /// 当前查询在全文中的所有匹配（源字节区间，按出现顺序）。
    matches: Vec<Range<usize>>,
    /// 当前高亮匹配的下标（`matches` 内；越界钳到 0）。
    index: usize,
}

/// 命令面板状态（`Cmd+P` 展开；SPEC D38）。
#[derive(Default)]
#[allow(dead_code)]
struct PaletteState {
    /// 面板是否展开。
    open: bool,
    /// 输入框文本。
    query: String,
    /// 当前选中项下标（过滤后列表内）。
    selected: usize,
}

/// 命令面板条目：标题 + 快捷键提示 + 动作标签。
#[allow(dead_code)]
struct PaletteItem {
    title: String,
    hint: String,
    action: PaletteAction,
}

/// 命令面板可执行命令（按标签分派到 `DocView` 方法）。
#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum PaletteAction {
    NewTab,
    CloseTab,
    CloseAllTabs,
    OpenFile,
    Save,
    SaveAs,
    ExportPdf,
    Find,
    ToggleTree,
    ToggleOutline,
    MoveCurrentFile,
}

#[allow(dead_code)]
struct Tab {
    path: Option<PathBuf>,
    doc: Document,
    cursors: Vec<Cursor>,
    primary: usize,
    selecting: bool,
    layout: Option<DocLayout>,
    highlighter: Option<Highlighter>,
    find: FindState,
    saved_text: Option<String>,
}

#[allow(dead_code)]
impl Tab {
    fn new(text: String, path: Option<PathBuf>) -> Self {
        let saved = if path.is_some() {
            Some(text.clone())
        } else {
            None
        };
        Self {
            path,
            doc: Document::new(&text),
            cursors: vec![Cursor::new(0)],
            primary: 0,
            selecting: false,
            layout: None,
            highlighter: None,
            find: FindState::default(),
            saved_text: saved,
        }
    }
    fn is_dirty(&self) -> bool {
        if self.path.is_none() {
            return false;
        }
        match &self.saved_text {
            Some(saved) => saved.as_str() != self.doc.text(),
            None => true,
        }
    }
    fn name(&self) -> String {
        let base = match &self.path {
            Some(p) => p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            None => "内置示例".to_string(),
        };
        if self.is_dirty() {
            format!("* {base}")
        } else {
            base
        }
    }
    fn full(&self) -> String {
        match &self.path {
            Some(p) => p.display().to_string(),
            None => "内置示例（未保存）".to_string(),
        }
    }
}

/// 主窗口内容：文档视图（投影渲染 + 多光标/选区）。
struct DocView {
    #[allow(dead_code)]
    tabs: Vec<Tab>,
    #[allow(dead_code)]
    active: usize,
    /// 打开的文件路径（`None` = 使用内置示例）。
    path: Option<PathBuf>,
    /// 文档（源文本真相 + 投影缓存）。
    doc: Document,
    /// 所有光标（至少有一个）。
    cursors: Vec<Cursor>,
    /// 主光标索引（用于 IME、键盘导航基准、状态栏显示）。
    primary: usize,
    /// 是否正在拖拽选区（鼠标左键按下未松开）。
    selecting: bool,
    /// 最近一次渲染的布局几何（供鼠标命中测试读取）。
    layout: Option<DocLayout>,
    /// 编辑区滚动句柄：由 `#editor` 滚动容器 `track_scroll` 绑定，
    /// prepaint 的 clamp 会回填其视口 bounds / 滚动量（可见区、光标跟随用）。
    scroll: gpui::ScrollHandle,
    /// 上一帧算出的内容总高（`DocElement::request_layout` 上报的可滚动高度）。
    ///
    /// 行数/宽度变了才变，变化时 prepaint 补 `notify()` 一帧收敛。
    content_height: Pixels,
    /// 上次滚动跟随的光标行 `(顶边, 行高)`（内容坐标）；变化才再次介入滚动，
    /// 避免把用户手动滚上去的视口又拽回来。
    caret_scroll_key: Option<(Pixels, Pixels)>,
    /// 焦点句柄：键盘事件只在获得焦点后送达。
    focus: FocusHandle,
    /// 系统字体族（正文 run 的字体）。
    ui_font: SharedString,
    /// 等宽字体族（来自主题，代码块/行内代码使用）。
    mono_font: SharedString,
    /// 等宽字号（来自主题）。
    mono_font_size: Pixels,
    /// 主题色快照。
    palette: Palette,
    /// 代码块语法高亮器（首次渲染时惰性创建并复用；`SyntaxSet` 较大）。
    highlighter: Option<Highlighter>,
    /// 查找/替换条状态（`Cmd+F` 展开）。
    find: FindState,
    /// 工作区根（文件树）
    workspace_root: Option<PathBuf>,
    /// Workspace（本地文件系统）
    workspace: Option<Workspace<FileSystemProvider>>,
    /// 文件树是否展开
    tree_open: bool,
    /// 大纲（目录导航）是否展开
    outline_open: bool,
    /// 已打开文件的保存快照（脏状态用）
    saved_text: Option<String>,
    /// 命令面板状态（`Cmd+P`，D38）。
    #[allow(dead_code)]
    cmd_palette: PaletteState,
    /// 正在重命名的文件树条目：`(原路径, 编辑中的新名)`；`None` 表示无。
    renaming: Option<(PathBuf, String)>,
}

impl DocView {
    #[allow(dead_code)]
    fn cur_tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }
    #[allow(dead_code)]
    fn cur_tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    /// 读入文件（或内置示例）并构建文档，光标置于文首。
    fn new(path: Option<&PathBuf>, cx: &mut App) -> Self {
        let text = match path {
            Some(p) => std::fs::read_to_string(p).unwrap_or_else(|e| format!("> 读取失败：{e}\n")),
            None => SAMPLE.to_string(),
        };
        let theme = cx.theme();
        let palette = Palette::from_theme(theme);
        let saved_text = path.is_some().then(|| text.clone());
        // 设置工作区根为文件所在目录或命令行目录
        let ws_root = path
            .as_ref()
            .and_then(|p| p.parent().map(|x| x.to_path_buf()))
            .or_else(|| {
                std::env::args()
                    .nth(1)
                    .and_then(|a| PathBuf::from(a).parent().map(|x| x.to_path_buf()))
            });
        let (ws_root, ws) = ws_root
            .and_then(|r| {
                FileSystemProvider::open(&r)
                    .ok()
                    .map(|p| (Some(r.clone()), Some(Workspace::new(p))))
            })
            .unwrap_or((None, None));
        let t = Tab {
            path: path.cloned(),
            doc: Document::new(&text),
            cursors: vec![Cursor::new(0)],
            primary: 0,
            selecting: false,
            layout: None,
            highlighter: None,
            find: FindState::default(),
            saved_text: path.is_some().then(|| text.clone()),
        };
        Self {
            tabs: vec![t],
            active: 0,
            path: path.cloned(),
            doc: Document::new(&text),
            cursors: vec![Cursor::new(0)],
            primary: 0,
            selecting: false,
            layout: None,
            scroll: gpui::ScrollHandle::default(),
            content_height: Pixels::ZERO,
            caret_scroll_key: None,
            focus: cx.focus_handle(),
            ui_font: theme.font_family.clone(),
            mono_font: theme.mono_font_family.clone(),
            mono_font_size: theme.mono_font_size,
            palette,
            highlighter: None,
            find: FindState::default(),
            workspace_root: ws_root,
            workspace: ws,
            tree_open: true,
            outline_open: true,
            saved_text,
            cmd_palette: PaletteState::default(),
            renaming: None,
        }
    }

    fn is_dirty(&self) -> bool {
        if self.path.is_none() {
            return false;
        }
        match &self.saved_text {
            Some(saved) => *saved != self.doc.text(),
            None => true,
        }
    }

    fn header_text(&self) -> String {
        let base = match &self.path {
            Some(p) => p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            None => "内置示例".to_string(),
        };
        if self.is_dirty() {
            format!("* {base}")
        } else {
            base
        }
    }

    fn header_full_path(&self) -> String {
        match &self.path {
            Some(p) => p.display().to_string(),
            None => "内置示例（未保存）".to_string(),
        }
    }

    /// 当前主光标的选区（源字节 `Range`），有序区间；无选区时返回 `None`。
    fn primary_selection(&self) -> Option<Range<usize>> {
        self.cursors[self.primary].selection()
    }

    /// 所有光标的选区集合（去重、合并重叠）。
    fn all_selections(&self) -> Vec<Range<usize>> {
        let mut ranges: Vec<Range<usize>> =
            self.cursors.iter().filter_map(|c| c.selection()).collect();
        ranges.sort_by_key(|r| r.start);
        // 合并重叠/相邻
        let mut merged: Vec<Range<usize>> = Vec::new();
        for r in ranges {
            if let Some(last) = merged.last_mut() {
                if r.start <= last.end {
                    last.end = last.end.max(r.end);
                } else {
                    merged.push(r);
                }
            } else {
                merged.push(r);
            }
        }
        merged
    }

    /// 获取主光标的不可变引用（用于只读访问）。
    fn primary_cursor(&self) -> &Cursor {
        &self.cursors[self.primary]
    }

    /// 获取主光标的可变引用（用于修改）。
    fn primary_cursor_mut(&mut self) -> &mut Cursor {
        &mut self.cursors[self.primary]
    }

    /// 所有光标左移一个字符边界。
    fn move_left(&mut self, shift: bool) {
        let doc = &self.doc;
        for c in &mut self.cursors {
            let new_caret = doc.prev_char_boundary(c.caret);
            if shift {
                if c.anchor.is_none() {
                    c.anchor = Some(c.caret);
                }
            } else {
                c.anchor = None;
            }
            c.caret = doc.snap_to_boundary(new_caret);
        }
    }

    /// 所有光标右移一个字符边界。
    fn move_right(&mut self, shift: bool) {
        let doc = &self.doc;
        for c in &mut self.cursors {
            let new_caret = doc.next_char_boundary(c.caret);
            if shift {
                if c.anchor.is_none() {
                    c.anchor = Some(c.caret);
                }
            } else {
                c.anchor = None;
            }
            c.caret = doc.snap_to_boundary(new_caret);
        }
    }

    /// 所有光标到当前行首。
    fn move_home(&mut self, shift: bool) {
        let projection = self.doc.projection();
        for c in &mut self.cursors {
            let start = projection.lines[projection.line_of_source(c.caret)]
                .source_range
                .start;
            if shift {
                if c.anchor.is_none() {
                    c.anchor = Some(c.caret);
                }
            } else {
                c.anchor = None;
            }
            c.caret = start;
        }
    }

    /// 所有光标到当前行尾。
    fn move_end(&mut self, shift: bool) {
        let projection = self.doc.projection();
        for c in &mut self.cursors {
            let end = projection.lines[projection.line_of_source(c.caret)]
                .source_range
                .end;
            if shift {
                if c.anchor.is_none() {
                    c.anchor = Some(c.caret);
                }
            } else {
                c.anchor = None;
            }
            c.caret = end;
        }
    }

    /// 所有光标上移一个**视觉行**（软换行：按行折行后逐行移动，保留水平位置）。
    fn move_up(&mut self, shift: bool) {
        self.move_visual_vertical(-1, shift);
    }

    /// 所有光标下移一个**视觉行**。
    fn move_down(&mut self, shift: bool) {
        self.move_visual_vertical(1, shift);
    }

    /// 所有光标上/下移动：`step` 为 -1（上）或 +1（下）。
    ///
    /// 用文档坐标 [`DocPos`] 保留**显示列**（可见字符列），而非字节相对偏移，
    /// 避免 CJK 等多字节行上下移动时列漂移；目标行较短时列自动钳到行尾。
    fn move_vertical_all(&mut self, step: isize, shift: bool) {
        let projection = self.doc.projection();
        for c in &mut self.cursors {
            let pos = projection.source_to_doc_pos(c.caret);
            // 展开态（光标在链接内）显示列更长：取块几何换算后的列，保持视觉水平位置
            let col = self
                .layout
                .as_ref()
                .and_then(|l| l.blocks.get(pos.line))
                .map_or(pos.col, |b| b.col_of_source(c.caret));
            let target_line = if step < 0 {
                pos.line.checked_sub(1)
            } else {
                (pos.line + 1 < projection.len()).then_some(pos.line + 1)
            };
            let new_caret = match target_line {
                Some(line) => projection.doc_pos_to_source(DocPos { line, col }),
                // 越界：上移落到文首，下移落到文末
                None => {
                    if step < 0 {
                        0
                    } else {
                        projection.lines.last().map_or(0, |l| l.source_range.end)
                    }
                }
            };
            if shift {
                if c.anchor.is_none() {
                    c.anchor = Some(c.caret);
                }
            } else {
                c.anchor = None;
            }
            c.caret = new_caret;
        }
    }

    /// 所有光标上/下移动一个**视觉行**（软换行 D33 收尾），尽量保留水平像素位置。
    ///
    /// 有布局时按视觉行走（绕过软换行）；首帧前布局未就绪时回退到逻辑行移动。
    /// 全文首/末视觉行处不动。不调用 `cx.notify`（由调用方统一触发重绘）。
    fn move_visual_vertical(&mut self, step: isize, shift: bool) {
        // 布局未就绪（首帧渲染前）：回退到逻辑行移动
        if self.layout.is_none() {
            self.move_vertical_all(step, shift);
            return;
        }
        // 之后 `layout`（self.layout）、`projection`（self.doc）、`self.cursors` 三处借用字段互不重叠
        let Some(layout) = self.layout.as_ref() else {
            return;
        };
        let projection = self.doc.projection();
        for c in &mut self.cursors {
            let new_caret =
                visual_vertical_target(layout, projection, c.caret, step).unwrap_or(c.caret);
            if shift {
                if c.anchor.is_none() {
                    c.anchor = Some(c.caret);
                }
            } else {
                c.anchor = None;
            }
            c.caret = new_caret;
        }
    }

    /// 应用一次编辑到主光标（单光标操作入口，供 IME 等使用）。
    ///
    /// `new_caret` 是编辑**完成后**希望主光标落在的源字节偏移（会被 `snap_to_boundary` 校正）。
    /// 编辑失败（非法边界等）时保持文档与光标不变。成功后塌缩主光标选区并清空 IME 组合区。
    fn apply_edit(&mut self, edit: Edit, new_caret: usize, cx: &mut Context<Self>) {
        if self.doc.apply(edit).is_ok() {
            // 先算好校正后的位置（结束对 self.doc 的借用），再改光标，避免借用冲突
            let snapped = self.doc.snap_to_boundary(new_caret);
            let c = self.primary_cursor_mut();
            c.anchor = None;
            c.marked = None;
            c.caret = snapped;
            cx.notify();
        }
    }

    /// 撤销/重做等"绕过 `apply_edit`"的变更之后，统一收敛光标/选区状态并重绘。
    fn after_doc_mutation(&mut self, cx: &mut Context<Self>) {
        for c in &mut self.cursors {
            c.caret = self.doc.snap_to_boundary(c.caret);
            c.anchor = None;
            c.marked = None;
        }
        cx.notify();
    }

    /// 应用行内格式命令（粗体/斜体/删除线/行内代码）到所有光标。
    fn apply_inline(&mut self, mark: InlineMark, cx: &mut Context<Self>) {
        // 对每个光标单独应用格式（基于各自的选区/光标位置）
        let ranges: Vec<Range<usize>> = self
            .cursors
            .iter()
            .map(|c| c.selection().unwrap_or(c.caret..c.caret))
            .collect();
        // 倒序处理，避免偏移变化影响后续光标
        for range in ranges.iter().rev() {
            let edit = toggle_inline(&self.doc.text(), range.clone(), mark);
            if self.doc.apply(edit).is_ok() {
                // 简化：应用成功后重置所有光标
            }
        }
        // 简化：应用后重置所有光标到第一个位置
        if let Some(first) = self.cursors.first() {
            let new_caret = self.doc.snap_to_boundary(first.caret);
            for c in &mut self.cursors {
                c.caret = new_caret;
                c.anchor = None;
                c.marked = None;
            }
        }
        cx.notify();
    }

    /// 应用块级样式命令（标题/列表/引用/段落）到所有光标。
    fn apply_block(&mut self, target: BlockTarget, cx: &mut Context<Self>) {
        // 收集所有光标覆盖的行范围
        let projection = self.doc.projection();
        let mut all_ranges = Vec::new();
        for c in &self.cursors {
            let sel = c.selection().unwrap_or(c.caret..c.caret);
            let start_line = projection
                .lines
                .iter()
                .position(|l| l.source_range.end >= sel.start)
                .unwrap_or(0);
            let end_line = projection
                .lines
                .iter()
                .position(|l| l.source_range.start >= sel.end)
                .unwrap_or(projection.lines.len());
            let range = projection.lines[start_line].source_range.start
                ..projection.lines[end_line.min(projection.lines.len() - 1)]
                    .source_range
                    .end;
            all_ranges.push(range);
        }
        // 去重合并
        let mut merged = all_ranges;
        merged.sort_by_key(|r| r.start);
        let mut merged_ranges: Vec<Range<usize>> = Vec::new();
        for r in merged {
            if let Some(last) = merged_ranges.last_mut() {
                if r.start <= last.end {
                    last.end = last.end.max(r.end);
                } else {
                    merged_ranges.push(r);
                }
            } else {
                merged_ranges.push(r);
            }
        }
        // 对每个合并范围应用块样式
        for range in merged_ranges.iter().rev() {
            let edit = set_block(&self.doc.text(), range.clone(), &target);
            let _ = self.doc.apply(edit);
        }
        // 简化：重置光标
        if let Some(first) = self.cursors.first() {
            let new_caret = self.doc.snap_to_boundary(first.caret);
            for c in &mut self.cursors {
                c.caret = new_caret;
                c.anchor = None;
                c.marked = None;
            }
        }
        cx.notify();
    }

    /// 表格结构编辑（增删行列），作用于主光标。
    ///
    /// 光标不在表格内、或操作不产生变化（删唯一列 / 在表头删行）时静默忽略。
    fn apply_table(&mut self, op: TableOp, cx: &mut Context<Self>) {
        let caret = self.primary_cursor().caret;
        if let Some(edit) = edit_table(&self.doc.text(), caret, op) {
            // 表格整体被重写，光标难以精确映射；保守地落到表格起点并吸附到字符边界。
            let new_caret = edit.source_range().start;
            self.apply_edit(edit, new_caret, cx);
        }
    }

    /// 删除选区（或光标左侧一个字符，即退格）。返回是否发生了删除。
    fn delete_backward(&mut self, cx: &mut Context<Self>) -> bool {
        let mut any_deleted = false;
        for c in &mut self.cursors {
            if let Some(sel) = c.selection() {
                let caret = sel.start;
                if self.doc.apply(Edit::Delete { range: sel }).is_ok() {
                    c.caret = self.doc.snap_to_boundary(caret);
                    c.anchor = None;
                    c.marked = None;
                    any_deleted = true;
                }
            } else {
                let prev = self.doc.prev_char_boundary(c.caret);
                if prev != c.caret
                    && self
                        .doc
                        .apply(Edit::Delete {
                            range: prev..c.caret,
                        })
                        .is_ok()
                {
                    c.caret = self.doc.snap_to_boundary(prev);
                    c.anchor = None;
                    c.marked = None;
                    any_deleted = true;
                }
            }
        }
        if any_deleted {
            cx.notify();
        }
        any_deleted
    }

    /// 删除光标右侧一个字符（即 Delete 键）。返回是否发生了删除。
    fn delete_forward(&mut self, cx: &mut Context<Self>) -> bool {
        let mut any_deleted = false;
        for c in &mut self.cursors {
            let range = c.selection().unwrap_or_else(|| {
                let next = self.doc.next_char_boundary(c.caret);
                c.caret..next
            });
            if !range.is_empty()
                && self
                    .doc
                    .apply(Edit::Delete {
                        range: range.clone(),
                    })
                    .is_ok()
            {
                c.caret = self.doc.snap_to_boundary(range.start);
                c.anchor = None;
                c.marked = None;
                any_deleted = true;
            }
        }
        if any_deleted {
            cx.notify();
        }
        any_deleted
    }

    /// 所有光标处插入换行。有选区则先删除选区。
    fn insert_newline(&mut self, cx: &mut Context<Self>) {
        for c in &mut self.cursors {
            let range = c.selection().unwrap_or(c.caret..c.caret);
            let at = range.start;
            if self
                .doc
                .apply(Edit::Replace {
                    range,
                    text: "\n".into(),
                })
                .is_ok()
            {
                c.caret = self.doc.snap_to_boundary(at + 1);
                c.anchor = None;
                c.marked = None;
            }
        }
        cx.notify();
    }

    /// 所有光标处插入制表符（Tab）。有选区则先删除选区。
    fn insert_tab(&mut self, cx: &mut Context<Self>) {
        for c in &mut self.cursors {
            let range = c.selection().unwrap_or(c.caret..c.caret);
            let at = range.start;
            if self
                .doc
                .apply(Edit::Replace {
                    range,
                    text: "    ".into(),
                })
                .is_ok()
            {
                c.caret = self.doc.snap_to_boundary(at + 4);
                c.anchor = None;
                c.marked = None;
            }
        }
        cx.notify();
    }

    /// 复制：把所有光标选中的文本（按文档顺序、以换行拼接）写入系统剪贴板。
    ///
    /// 无任何选区时不做任何事（空选区不覆盖剪贴板，与多数编辑器一致）。
    fn copy_selection(&self, cx: &mut Context<Self>) {
        let sels = self.all_selections();
        if sels.is_empty() {
            return;
        }
        let text = self.doc.text();
        let joined = sels
            .iter()
            .map(|r| text[r.clone()].to_string())
            .collect::<Vec<_>>()
            .join("\n");
        cx.write_to_clipboard(ClipboardItem::new_string(joined));
    }

    /// 剪切：先复制选区文本，再删除所有光标处的选区。
    fn cut_selection(&mut self, cx: &mut Context<Self>) {
        self.copy_selection(cx);
        self.delete_all_selections(cx);
    }

    /// 删除所有光标处的选区（无选区则不动）。返回是否发生了删除。
    ///
    /// 按选区**起点倒序**处理：删除靠后的选区不会改变靠前选区的偏移，
    /// 避免顺序删除导致后续光标落在错误位置。
    fn delete_all_selections(&mut self, cx: &mut Context<Self>) -> bool {
        let mut order: Vec<(usize, usize)> = self
            .cursors
            .iter()
            .enumerate()
            .filter_map(|(i, c)| c.selection().map(|r| (i, r.start)))
            .collect();
        order.sort_by_key(|&(_, pos)| Reverse(pos));
        let mut changed = false;
        for (i, _) in order {
            let Some(sel) = self.cursors[i].selection() else {
                continue;
            };
            let start = sel.start;
            if self.doc.apply(Edit::Delete { range: sel }).is_ok() {
                let c = &mut self.cursors[i];
                c.caret = self.doc.snap_to_boundary(start);
                c.anchor = None;
                c.marked = None;
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
        changed
    }

    /// 粘贴：读取系统剪贴板的纯文本，替换每个光标处的选区（无选区则插入）。
    ///
    /// 按光标**位置倒序**处理，避免靠前插入使靠后光标偏移失效；剪贴板无文本或为空时不动。
    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        if text.is_empty() {
            return;
        }
        let mut order: Vec<(usize, usize)> = self
            .cursors
            .iter()
            .enumerate()
            .map(|(i, c)| (i, c.selection().map_or(c.caret, |r| r.start)))
            .collect();
        order.sort_by_key(|&(_, pos)| Reverse(pos));
        for (i, _) in order {
            let range = self.cursors[i]
                .selection()
                .unwrap_or_else(|| self.cursors[i].caret..self.cursors[i].caret);
            let at = range.start;
            let inserted = text.len();
            if self
                .doc
                .apply(Edit::Replace {
                    range,
                    text: text.clone(),
                })
                .is_ok()
            {
                let c = &mut self.cursors[i];
                c.caret = self.doc.snap_to_boundary(at + inserted);
                c.anchor = None;
                c.marked = None;
            }
        }
        cx.notify();
    }

    // ==================== 查找/替换（Cmd+F）====================

    /// 展开查找条；若有单行非空选区则把它作为初始查询，随后跳到第一个匹配。
    fn open_find(&mut self, cx: &mut Context<Self>) {
        self.find.open = true;
        self.find.focus_replace = false;
        // 单行非空选区 → 作为初始查询（跨行选区不作为查询）
        if let Some(sel) = self.primary_selection() {
            let t = self.doc.slice(sel).into_owned();
            if !t.is_empty() && !t.contains('\n') {
                self.find.query = t;
            }
        }
        self.recompute_matches();
        if !self.find.matches.is_empty() {
            self.goto_match(0, cx);
        } else {
            cx.notify();
        }
    }

    /// 收起查找条（保留输入文本，便于下次展开）。
    fn close_find(&mut self, cx: &mut Context<Self>) {
        self.find.open = false;
        cx.notify();
    }

    /// 按当前查询重新计算全部匹配，并把序号钳到有效范围。
    fn recompute_matches(&mut self) {
        let text = self.doc.text();
        let query = self.find.query.clone();
        let cs = self.find.case_sensitive;
        self.find.matches = find_all(&text, &query, cs);
        if self.find.index >= self.find.matches.len() {
            self.find.index = 0;
        }
    }

    /// 跳到第 `i` 个匹配：把它设为唯一选区（`anchor=start`、`caret=end`）以高亮并移动光标。
    fn goto_match(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(m) = self.find.matches.get(i).cloned() else {
            return;
        };
        self.find.index = i;
        self.cursors = vec![Cursor {
            caret: m.end,
            anchor: Some(m.start),
            marked: None,
        }];
        self.primary = 0;
        cx.notify();
    }

    /// 收集当前文档的大纲（标题列表）：`(级别, 标题文本, 源字节偏移)`。
    ///
    /// 数据来源是投影：每个 `ProjectedLine` 若 `block.heading` 为 `Some(level)`，
    /// 则该行是标题行，其 `display_text()` 是已隐藏 `#` 标记后的可见标题文本。
    fn outline_items(&mut self) -> Vec<(u8, String, usize)> {
        let projection = self.doc.projection();
        let mut out = Vec::new();
        for line in &projection.lines {
            if let Some(level) = line.block.heading {
                let title = line.display_text();
                let title = title.trim().to_string();
                out.push((level, title, line.source_range.start));
            }
        }
        out
    }

    /// 把光标移到指定源偏移（大纲/目录导航跳转用），并聚焦编辑区。
    fn goto_source(&mut self, src: usize, cx: &mut Context<Self>) {
        let src = self.doc.snap_to_boundary(src);
        self.cursors = vec![Cursor::new(src)];
        self.primary = 0;
        self.selecting = false;
        cx.notify();
    }

    /// 跳到下一个匹配（循环）。
    fn find_next(&mut self, cx: &mut Context<Self>) {
        if self.find.matches.is_empty() {
            return;
        }
        let n = self.find.matches.len();
        self.goto_match((self.find.index + 1) % n, cx);
    }

    /// 跳到上一个匹配（循环）。
    fn find_prev(&mut self, cx: &mut Context<Self>) {
        if self.find.matches.is_empty() {
            return;
        }
        let n = self.find.matches.len();
        self.goto_match((self.find.index + n - 1) % n, cx);
    }

    /// 用替换文本替换**当前**匹配，然后重算并跳到其后最近的匹配。
    fn replace_current(&mut self, cx: &mut Context<Self>) {
        let Some(m) = self.find.matches.get(self.find.index).cloned() else {
            return;
        };
        let rep = self.find.replace.clone();
        if self
            .doc
            .apply(Edit::Replace {
                range: m.clone(),
                text: rep,
            })
            .is_ok()
        {
            self.recompute_matches();
            if self.find.matches.is_empty() {
                // 替换后无更多匹配：光标停在被替换处
                self.find.index = 0;
                self.cursors = vec![Cursor::new(self.doc.snap_to_boundary(m.start))];
                self.primary = 0;
            } else {
                // 停在原序号（其后最近的下一个匹配），越界则回首项
                let i = self.find.index.min(self.find.matches.len() - 1);
                self.goto_match(i, cx);
            }
        }
        cx.notify();
    }

    /// 替换**全部**匹配（单次全文 `Edit::Replace`，作为一条撤销记录）。
    fn replace_all_now(&mut self, cx: &mut Context<Self>) {
        let text = self.doc.text();
        let query = self.find.query.clone();
        let rep = self.find.replace.clone();
        let cs = self.find.case_sensitive;
        if let Some(edit) = replace_all_edit(&text, &query, &rep, cs)
            && self.doc.apply(edit).is_ok()
        {
            // 全文替换后旧偏移失效：光标收敛到文首
            self.cursors = vec![Cursor::new(0)];
            self.primary = 0;
            self.recompute_matches();
        }
        cx.notify();
    }

    /// 用 `text` 替换主光标处的指定区间 `target`（不先取当前选区）；IME 的内部替换原语。
    ///
    /// 与 [`Self::replace_selection_with`] 的区别：后者以"当前选区"为目标，
    /// 本方法以调用方给定的 `target` 为目标（IME 常传入 marked 区间）。
    fn replace_selection_with_inner(
        &mut self,
        target: Range<usize>,
        text: &str,
        cx: &mut Context<Self>,
    ) {
        let start = self.doc.snap_to_boundary(target.start);
        let end = self.doc.snap_to_boundary(target.end);
        let range = start.min(end)..start.max(end);
        let new_caret = range.start + text.len();
        if self
            .doc
            .apply(Edit::Replace {
                range,
                text: text.to_string(),
            })
            .is_ok()
        {
            // 先算好校正后的位置，再改主光标，避免同时借用 self.doc 与 self.cursors
            let snapped = self.doc.snap_to_boundary(new_caret);
            let c = self.primary_cursor_mut();
            c.caret = snapped;
            c.anchor = None;
            c.marked = None;
            cx.notify();
        }
    }

    /// 计算 IME 写入/替换的目标区间（源字节）。
    ///
    /// 优先级：调用方给的 UTF-16 区间 → 当前组合（marked）区间 → 选区/光标。
    /// `setMarkedText:`/`insertText:` 在**更新或提交组合**时通常传 `NSNotFound`
    /// （映射为 `None`），此时必须替换已有 marked 区间；若退回到光标处，组合的每一帧
    /// 都会再插入一份（表现为拼音字母逐个上屏、文档里堆满 "y" "yp" "yps"）。
    fn ime_target(&self, range: Option<Range<usize>>) -> Range<usize> {
        if let Some(r) = range
            && let Some((start, end)) = self.source_range_from_utf16(r)
        {
            return start..end;
        }
        if let Some(marked) = self.primary_cursor().marked.clone() {
            return marked;
        }
        let caret = self.cursors[self.primary].caret;
        self.primary_selection().unwrap_or(caret..caret)
    }

    /// UTF-8 字节区间 → UTF-16 单元区间（IME 选区查询用）。
    ///
    /// 越界时钳到文档末尾；保证 `lo <= hi`。
    fn utf16_range(&self, start: usize, end: usize) -> (usize, usize) {
        let text = self.doc.text();
        let bytes = text.as_bytes();
        let clamp = |b: usize| -> usize {
            let b = b.min(bytes.len());
            // 钳到最近的字符边界，避免切在多字节序列中间
            let mut i = b;
            while i > 0 && i < bytes.len() && (bytes[i] & 0xC0) == 0x80 {
                i -= 1;
            }
            i
        };
        let (s, e) = (clamp(start), clamp(end));
        let (lo, hi) = if s <= e { (s, e) } else { (e, s) };
        let u16_lo = text[..lo].encode_utf16().count();
        let u16_hi = text[..hi].encode_utf16().count();
        (u16_lo, u16_hi)
    }

    /// UTF-16 单元区间 → UTF-8 字节区间（IME 设置选区用）。
    ///
    /// 线性扫描每个字符的 UTF-16 宽度，累加得到目标单元边界对应的字节偏移。
    /// `u_target` 为 `None` 表示未到达该边界（钳到文档末）。
    fn source_range_from_utf16(&self, range: Range<usize>) -> Option<(usize, usize)> {
        let text = self.doc.text();
        let total_u16 = text.encode_utf16().count();
        let clamp = |u: usize| u.min(total_u16);
        let (u_lo, u_hi) = (clamp(range.start), clamp(range.end));
        let (u_lo, u_hi) = if u_lo <= u_hi {
            (u_lo, u_hi)
        } else {
            (u_hi, u_lo)
        };

        // 第一遍：找 u_lo 对应的字节起点
        let mut u_acc = 0usize;
        let mut lo_byte = text.len();
        for (b, ch) in text.char_indices() {
            if u_acc >= u_lo {
                lo_byte = b;
                break;
            }
            u_acc += ch.len_utf16();
        }
        // 第二遍：找 u_hi 对应的字节终点
        let mut u_acc = 0usize;
        let mut hi_byte = text.len();
        for (b, ch) in text.char_indices() {
            if u_acc >= u_hi {
                hi_byte = b;
                break;
            }
            u_acc += ch.len_utf16();
        }
        Some((lo_byte, hi_byte))
    }

    /// 点击任务复选框：命中则把源文本里的 `[ ]` ↔ `[x]`（`[X]` 归一为 `[ ]`），返回是否命中。
    ///
    /// 复选框矩形与其源字节区间来自 prepaint 记录的 [`DocLayout::task_boxes`]；
    /// `[ ]`/`[x]` 等长替换，光标偏移无需调整，但按契约仍走 `apply_edit` 统一收敛。
    fn toggle_task_at(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) -> bool {
        let target = self
            .layout
            .as_ref()
            .and_then(|l| l.task_boxes.iter().find(|(r, _)| r.contains(&position)))
            .map(|(_, range)| range.clone());
        let Some(range) = target else {
            return false;
        };
        let Ok(cur) = self.doc.try_slice(range.clone()).map(|s| s.into_owned()) else {
            return false;
        };
        let new = match cur.as_str() {
            "[ ]" => "[x]",
            "[x]" | "[X]" => "[ ]",
            // 防御：几何与源文本对不上（布局刚失效）时不动文档
            _ => return false,
        };
        let caret = self.cursors[self.primary].caret;
        self.apply_edit(
            Edit::Replace {
                range,
                text: new.to_string(),
            },
            caret,
            cx,
        );
        true
    }

    /// 从窗口坐标命中一个**源字节偏移**（整篇命中：先算显示索引，再经 OffsetMap 映射）。
    fn hit_source(&self, position: Point<Pixels>) -> Option<usize> {
        let layout = self.layout.as_ref()?;
        let (row, col) = layout.hit(position)?;
        let block = &layout.blocks[row];
        // 显示字符列 → 源字节偏移（空行等无显示文本的行落到行首）
        let src = self.doc.snap_to_boundary(block.source_of_col(col));
        Some(src)
    }

    /// 命中窗口坐标处的**链接目标**（Cmd+点击用）；不在链接上返回 `None`。
    fn link_at(&self, position: Point<Pixels>) -> Option<String> {
        let layout = self.layout.as_ref()?;
        let (row, col) = layout.hit(position)?;
        let block = &layout.blocks[row];
        let range = block.proj.source_range.clone();
        let src = block.source_of_col(col);
        let line_src = self.doc.try_slice(range.clone()).ok()?;
        let rel = src.saturating_sub(range.start).min(line_src.len());
        let reveal = inline_reveals(&line_src)
            .into_iter()
            .find(|r| rel >= r.range.start && rel <= r.range.end && r.link_dest.is_some())?;
        let dest = reveal.link_dest.as_ref()?;
        let raw = line_src.get(dest.clone())?.trim();
        let url = raw
            .strip_prefix('<')
            .and_then(|u| u.strip_suffix('>'))
            .unwrap_or(raw);
        (!url.is_empty()).then(|| url.to_string())
    }

    /// 导出当前文档为 PDF 并保存到文件。
    ///
    /// 异步保存对话框（`AsyncFileDialog` + `cx.spawn`，D41），避免同步对话框嵌套泵
    /// runloop 触发 gpui 键盘布局回调的 `RefCell` 争用。
    fn export_pdf(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let markdown = self.doc.text().to_string();
        cx.spawn(async move |_this, _cx| {
            let picked = AsyncFileDialog::new()
                .set_title("导出 PDF")
                .add_filter("PDF 文件", &["pdf"])
                .set_file_name("document.pdf")
                .save_file()
                .await;
            let Some(handle) = picked else { return };
            let path = handle.path().to_path_buf();
            match to_pdf(&markdown) {
                Ok(pdf_doc) => {
                    if let Err(e) = std::fs::write(&path, &pdf_doc.bytes) {
                        eprintln!("导出 PDF 失败：{}", e);
                    }
                }
                Err(e) => {
                    eprintln!("PDF 编译失败：{}", e);
                }
            }
        })
        .detach();
    }

    /// 命令面板：内置命令表（D38）。
    fn palette_commands(&self) -> Vec<PaletteItem> {
        use PaletteAction::*;
        let mk = |title: &str, hint: &str, action: PaletteAction| PaletteItem {
            title: title.to_string(),
            hint: hint.to_string(),
            action,
        };
        vec![
            mk("新建标签页", "Cmd+T", NewTab),
            mk("关闭标签页", "Cmd+W", CloseTab),
            mk("关闭全部标签页", "Cmd+Shift+W", CloseAllTabs),
            mk("打开文件…", "Cmd+O", OpenFile),
            mk("保存", "Cmd+S", Save),
            mk("另存为…", "Cmd+Shift+S", SaveAs),
            mk("导出 PDF", "Cmd+E", ExportPdf),
            mk("查找/替换", "Cmd+F", Find),
            mk("切换文件树", "", ToggleTree),
            mk("切换大纲", "Cmd+Shift+O", ToggleOutline),
            mk("移动当前文件…", "", MoveCurrentFile),
        ]
    }

    /// 命令面板：按查询过滤命令（大小写不敏感子串），返回匹配到的原下标。
    fn palette_filtered(&self) -> Vec<usize> {
        let q = self.cmd_palette.query.to_lowercase();
        self.palette_commands()
            .iter()
            .enumerate()
            .filter(|(_, it)| q.is_empty() || it.title.to_lowercase().contains(&q))
            .map(|(i, _)| i)
            .collect()
    }

    /// 打开命令面板（清空查询、选中置首）。
    fn open_palette(&mut self, cx: &mut Context<Self>) {
        self.cmd_palette.open = true;
        self.cmd_palette.query.clear();
        self.cmd_palette.selected = 0;
        cx.notify();
    }

    /// 关闭命令面板。
    fn close_palette(&mut self, cx: &mut Context<Self>) {
        self.cmd_palette.open = false;
        cx.notify();
    }

    /// 执行命令面板选中的命令。
    fn run_palette_action(
        &mut self,
        action: PaletteAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use PaletteAction::*;
        self.cmd_palette.open = false;
        match action {
            NewTab => self.new_tab_empty(cx),
            CloseTab => self.close_current_tab(cx),
            CloseAllTabs => self.close_all_tabs(cx),
            OpenFile => self.open_file_dialog(window, cx),
            Save => self.save_current(window, cx),
            SaveAs => self.save_as(window, cx),
            ExportPdf => self.export_pdf(window, cx),
            Find => self.open_find(cx),
            ToggleTree => {
                self.tree_open = !self.tree_open;
                cx.notify();
            }
            ToggleOutline => {
                self.outline_open = !self.outline_open;
                cx.notify();
            }
            MoveCurrentFile => self.move_current_file_dialog(window, cx),
        }
    }

    /// 打开 Markdown 文件对话框。
    ///
    /// **关键约束（D41）**：绝不能在 gpui 回调里用 rfd 的**同步** `FileDialog`。macOS 同步
    /// 对话框走 `NSApp runModal`，会**嵌套泵主 runloop**；其间系统重查键盘布局会回调 gpui 的
    /// `on_keyboard_layout_change` → `app.borrow_mut()`，而此时 gpui 仍持有 app 的 `RefCell`，
    /// 于是 panic（`app.rs` `RefCell already borrowed`）。`window.defer` 也无效，因为 defer 的闭包
    /// 仍在同一次借用内执行。
    ///
    /// 正解：用 rfd 的**异步** `AsyncFileDialog`（macOS 走 `beginSheetModalForWindow`，不阻塞、
    /// 不嵌套泵 runloop），在 gpui 的 `cx.spawn` 里 `await`；完成后经弱引用 `update` 回写结果。
    fn open_file_dialog(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let picked = AsyncFileDialog::new()
                .set_title("打开 Markdown")
                .add_filter("Markdown", &["md", "markdown", "txt"])
                .pick_file()
                .await;
            if let Some(handle) = picked {
                let path = handle.path().to_path_buf();
                let _ = this.update(cx, |view, cx| view.open_path(path, cx));
            }
        })
        .detach();
    }

    #[allow(dead_code)]
    fn switch_tab_dir(&mut self, delta: i32, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            return;
        }
        let mut idx = self.active as i32 + delta;
        let n = self.tabs.len() as i32;
        if idx < 0 {
            idx = n - 1;
        }
        if idx >= n {
            idx = 0;
        }
        self.switch_tab(idx as usize, cx);
    }

    #[allow(dead_code)]
    fn switch_tab(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx >= self.tabs.len() {
            return;
        }
        self.active = idx;
        let t = &self.tabs[self.active];
        self.path = t.path.clone();
        self.doc = Document::new(&t.doc.text());
        self.cursors = t.cursors.clone();
        self.primary = t.primary;
        self.selecting = t.selecting;
        self.layout = None;
        self.caret_scroll_key = None;
        self.highlighter = None;
        self.find = FindState::default();
        self.saved_text = t.saved_text.clone();
        cx.notify();
    }

    #[allow(dead_code)]
    fn close_all_tabs(&mut self, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            return;
        }
        let t = &self.tabs[0];
        self.tabs = vec![Tab {
            path: t.path.clone(),
            doc: Document::new(&t.doc.text()),
            cursors: t.cursors.clone(),
            primary: t.primary,
            selecting: t.selecting,
            layout: None,
            highlighter: None,
            find: FindState::default(),
            saved_text: t.saved_text.clone(),
        }];
        self.active = 0;
        self.path = self.tabs[0].path.clone();
        self.doc = Document::new(&self.tabs[0].doc.text());
        self.cursors = self.tabs[0].cursors.clone();
        self.primary = self.tabs[0].primary;
        self.selecting = self.tabs[0].selecting;
        self.layout = None;
        self.caret_scroll_key = None;
        self.highlighter = None;
        self.find = FindState::default();
        self.saved_text = self.tabs[0].saved_text.clone();
        cx.notify();
    }

    #[allow(dead_code)]
    fn close_current_tab(&mut self, cx: &mut Context<Self>) {
        if self.tabs.len() <= 1 {
            return;
        }
        self.tabs.remove(self.active);
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
        // sync to current fields
        let t = &self.tabs[self.active];
        self.path = t.path.clone();
        self.doc = Document::new(&t.doc.text());
        self.cursors = t.cursors.clone();
        self.primary = t.primary;
        self.selecting = t.selecting;
        self.layout = t.layout.clone();
        self.highlighter = None;
        self.find = FindState::default();
        self.saved_text = t.saved_text.clone();
        cx.notify();
    }

    #[allow(dead_code)]
    fn new_tab_empty(&mut self, cx: &mut Context<Self>) {
        self.tabs.push(Tab::new(String::new(), None));
        self.active = self.tabs.len() - 1;
        cx.notify();
    }

    #[allow(dead_code)]
    fn open_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let full = path.canonicalize().unwrap_or_else(|_| path.clone());
        // check existing
        for (i, t) in self.tabs.iter_mut().enumerate() {
            if t.path
                .as_ref()
                .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
                .as_ref()
                == Some(&full)
            {
                self.active = i;
                self.layout = None;
                self.caret_scroll_key = None;
                self.highlighter = None;
                cx.notify();
                return;
            }
        }
        if let Ok(text) = std::fs::read_to_string(&full) {
            self.tabs.push(Tab::new(text.clone(), Some(full.clone())));
            self.active = self.tabs.len() - 1;
            self.path = Some(full.clone());
            self.doc = Document::new(&text);
            self.cursors = vec![Cursor::new(0)];
            self.primary = 0;
            self.selecting = false;
            self.layout = None;
            self.caret_scroll_key = None;
            self.highlighter = None;
            self.find = FindState::default();
            self.saved_text = Some(text.clone());
            if let Some(parent) = full.parent() {
                if self.workspace.is_none() {
                    if let Ok(p) = FileSystemProvider::open(parent) {
                        self.workspace_root = Some(parent.to_path_buf());
                        self.workspace = Some(Workspace::new(p));
                        self.tree_open = true;
                    }
                } else {
                    self.workspace_root = Some(parent.to_path_buf());
                }
            }
            cx.notify();
            return;
        }
        // fallback
        self.tabs.push(Tab::new(String::new(), Some(full)));
        self.active = self.tabs.len() - 1;
        cx.notify();
    }

    fn save_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.path.clone() {
            if let Err(e) = std::fs::write(&path, self.doc.text().as_bytes()) {
                eprintln!("保存失败：{}", e);
            } else {
                self.saved_text = Some(self.doc.text().to_string());
                cx.notify();
            }
        } else {
            self.save_as(window, cx);
        }
    }

    #[allow(dead_code)]
    fn new_markdown_in_root(&mut self, cx: &mut Context<Self>) {
        if let Some(_ws) = self.workspace.as_ref()
            && let Some(ref root) = self.workspace_root
        {
            let mut name = "未命名.md".to_string();
            let mut i = 1;
            loop {
                let candidate = root.join(&name);
                if !candidate.exists() {
                    break;
                }
                name = format!("未命名{}.md", i);
                i += 1;
                if i > 1000 {
                    break;
                }
            }
            let full = root.join(&name);
            let _ = std::fs::write(
                &full,
                "# 新文档
",
            );
            if let Ok(p) = FileSystemProvider::open(root) {
                self.workspace = Some(Workspace::new(p));
            }
            self.open_path(full, cx);
        }
    }

    /// 开始重命名（文件树条目）。
    fn start_rename(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        self.renaming = Some((path, name));
        cx.notify();
    }

    /// 确认重命名。
    fn commit_rename(&mut self, cx: &mut Context<Self>) {
        if let Some((path, name)) = self.renaming.clone() {
            self.renaming = None;
            self.rename_file(path, &name, cx);
        }
    }

    /// 取消重命名。
    fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        self.renaming = None;
        cx.notify();
    }

    /// 删除指定路径的文件（文件树 CRUD）。
    fn delete_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if let Err(e) = std::fs::remove_file(&path) {
            eprintln!("删除失败：{}", e);
        } else {
            // 若当前打开的是该文件，清空为未保存示例（避免悬挂路径）
            if self.path.as_ref().is_some_and(|p| p == &path) {
                self.path = None;
                self.saved_text = None;
                self.layout = None;
                self.caret_scroll_key = None;
                self.highlighter = None;
            }
            self.refresh_workspace(cx);
        }
    }

    /// 重命名指定文件（同目录下的新文件名）。
    fn rename_file(&mut self, path: PathBuf, new_name: &str, cx: &mut Context<Self>) {
        if new_name.trim().is_empty() {
            return;
        }
        let Some(parent) = path.parent() else { return };
        let target = parent.join(new_name.trim());
        if target.exists() {
            eprintln!("重命名失败：目标已存在");
            return;
        }
        if let Err(e) = std::fs::rename(&path, &target) {
            eprintln!("重命名失败：{}", e);
        } else {
            if self.path.as_ref().is_some_and(|p| p == &path) {
                self.path = Some(target.clone());
            }
            self.refresh_workspace(cx);
        }
    }

    /// 移动当前文件到选定目录（命令面板入口）。
    ///
    /// 同 `open_file_dialog`：用异步 `AsyncFileDialog` + `cx.spawn`，避免同步对话框嵌套泵
    /// runloop 触发 gpui 键盘布局回调的 `RefCell` 争用（D41）。
    fn move_current_file_dialog(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let cur = self.path.clone();
        let Some(cur) = cur else { return };
        cx.spawn(async move |this, cx| {
            let picked = AsyncFileDialog::new()
                .set_title("移动到目录")
                .pick_folder()
                .await;
            if let Some(handle) = picked {
                let dir = handle.path().to_path_buf();
                let _ = this.update(cx, |view, cx| view.move_file(cur, dir, cx));
            }
        })
        .detach();
    }

    /// 把文件移动到目标目录（文件树 CRUD）。
    fn move_file(&mut self, path: PathBuf, target_dir: PathBuf, cx: &mut Context<Self>) {
        let Some(name) = path.file_name() else { return };
        let target = target_dir.join(name);
        if target == path {
            return;
        }
        if target.exists() {
            eprintln!("移动失败：目标已存在");
            return;
        }
        if let Err(e) = std::fs::rename(&path, &target) {
            eprintln!("移动失败：{}", e);
        } else {
            if self.path.as_ref().is_some_and(|p| p == &path) {
                self.path = Some(target.clone());
            }
            self.refresh_workspace(cx);
        }
    }

    /// 重建 Workspace（CRUD 后刷新文件树）。
    fn refresh_workspace(&mut self, cx: &mut Context<Self>) {
        if let Some(ref root) = self.workspace_root
            && let Ok(p) = FileSystemProvider::open(root.clone())
        {
            self.workspace = Some(Workspace::new(p));
        }
        self.layout = None;
        self.caret_scroll_key = None;
        cx.notify();
    }

    /// 另存为：弹原生保存对话框（经 `window.defer` 延迟，规避键盘布局回调的 `RefCell` 争用）。
    /// 另存为：异步保存对话框（`AsyncFileDialog` + `cx.spawn`，D41），避免同步对话框
    /// 嵌套泵 runloop 触发 gpui 键盘布局回调的 `RefCell` 争用。
    fn save_as(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let text = self.doc.text().to_string();
        cx.spawn(async move |this, cx| {
            let picked = AsyncFileDialog::new()
                .set_title("另存为")
                .add_filter("Markdown", &["md", "markdown"])
                .set_file_name("document.md")
                .save_file()
                .await;
            let Some(handle) = picked else { return };
            let path = handle.path().to_path_buf();
            if let Err(e) = std::fs::write(&path, text.as_bytes()) {
                eprintln!("保存失败：{}", e);
            } else {
                let _ = this.update(cx, |view, cx| {
                    view.path = Some(path.clone());
                    view.saved_text = Some(view.doc.text().to_string());
                    if let Some(parent) = path.parent()
                        && let Ok(p) = FileSystemProvider::open(parent)
                    {
                        view.workspace_root = Some(parent.to_path_buf());
                        view.workspace = Some(Workspace::new(p));
                        view.tree_open = true;
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Alt+Click：在点击位置添加新光标，或移除已有光标。
    fn add_or_remove_cursor(&mut self, src: usize) {
        let src = self.doc.snap_to_boundary(src);
        // 检查是否已有光标在此位置（或极近）
        if let Some(idx) = self.cursors.iter().position(|c| c.caret == src) {
            // 已有光标：移除（但保留至少一个）
            if self.cursors.len() > 1 {
                self.cursors.remove(idx);
                if self.primary >= self.cursors.len() {
                    self.primary = self.cursors.len() - 1;
                }
            }
        } else {
            // 新增光标
            self.cursors.push(Cursor::new(src));
            self.primary = self.cursors.len() - 1; // 新光标设为主光标
        }
    }
}

impl Render for DocView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 编辑区滚动：`ScrollHandle` 在 `DocElement::prepaint`（与 `Scrollable` 同一
        // 元素栈上下文）取回并回存到 `self.scroll`，供光标 scroll-into-view 使用
        let palette = self.palette;
        let _header = self.header_text();
        let caret_count = self.cursors.len();
        let line_count = self.doc.projection().len();
        let focus = self.focus.clone();
        // 查找/替换条（`Cmd+F` 展开）：查询框 + 替换框 + 匹配计数 + 大小写开关 + 提示
        // 命令面板（`Cmd+P` 展开）：查询框 + 过滤后命令列表
        let palette_overlay = if self.cmd_palette.open {
            let filtered = self.palette_filtered();
            let cmds = self.palette_commands();
            let mut rows: Vec<gpui::AnyElement> = Vec::new();
            for (rank, &idx) in filtered.iter().enumerate() {
                let it = &cmds[idx];
                let act = rank == self.cmd_palette.selected;
                rows.push(
                    div()
                        .id(gpui::ElementId::Name(format!("pal-{idx}").into()))
                        .w_full()
                        .px_2()
                        .py_1()
                        .text_sm()
                        .flex()
                        .justify_between()
                        .when(act, |d| d.bg(palette.selection))
                        .child(it.title.clone())
                        .child(it.hint.clone())
                        .into_any_element(),
                );
            }
            Some(
                div()
                    .id("palette")
                    .absolute()
                    .top(px(60.))
                    .left(px(0.))
                    .right(px(0.))
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .id("palette-box")
                            .w(px(480.))
                            .max_h(px(320.))
                            .flex()
                            .flex_col()
                            .border_1()
                            .border_color(palette.border)
                            .bg(palette.subtle)
                            .child(
                                div()
                                    .id("palette-q")
                                    .px_3()
                                    .py_2()
                                    .text_sm()
                                    .border_b_1()
                                    .border_color(palette.border)
                                    .child(if self.cmd_palette.query.is_empty() {
                                        "输入命令…".to_string()
                                    } else {
                                        self.cmd_palette.query.clone()
                                    }),
                            )
                            .children(rows),
                    ),
            )
        } else {
            None
        };
        let find_bar = if self.find.open {
            let query = self.find.query.clone();
            let replace = self.find.replace.clone();
            let total = self.find.matches.len();
            let cur = if total == 0 { 0 } else { self.find.index + 1 };
            let case_label = if self.find.case_sensitive {
                "Aa·开"
            } else {
                "aa·关"
            };
            Some(
                h_flex()
                    .id("findbar")
                    .w_full()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .h(px(34.))
                    .border_b_1()
                    .border_color(palette.border)
                    .text_sm()
                    .child(find_field("find-q", &query, !self.find.focus_replace, &palette))
                    .child(find_field("find-r", &replace, self.find.focus_replace, &palette))
                    .child(format!("{cur}/{total} 处"))
                    .child(case_label)
                    .child("Enter=下一个 · Shift+Enter=上一个 · Cmd+Enter=替换 · Cmd+Shift+Enter=全部 · Tab=切换框 · Esc=关闭"),
            )
        } else {
            None
        };

        // 文件树
        let mut sb: Vec<gpui::AnyElement> = Vec::new();
        sb.push(
            div()
                .id("sb-h")
                .px_2()
                .h(px(28.))
                .items_center()
                .border_b_1()
                .border_color(palette.border)
                .child("资源管理器")
                .into_any_element(),
        );
        // 大纲（目录导航）：列出当前文档标题，按级别缩进，点击跳转
        if self.outline_open {
            sb.push(
                div()
                    .id("ol-h")
                    .px_2()
                    .h(px(24.))
                    .items_center()
                    .text_xs()
                    .text_color(palette.border)
                    .child("大纲")
                    .into_any_element(),
            );
            let items = self.outline_items();
            if items.is_empty() {
                sb.push(
                    div()
                        .id("ol-empty")
                        .px_3()
                        .py_1()
                        .text_xs()
                        .text_color(palette.border)
                        .child("（无标题）")
                        .into_any_element(),
                );
            } else {
                for (i, (level, title, src)) in items.into_iter().enumerate() {
                    // 缩进按级别：level=1 无缩进，每级 +8px
                    let indent = ((level.saturating_sub(1)) as f32) * 8.0 + 8.0;
                    sb.push(
                        div()
                            .id(gpui::ElementId::Name(format!("ol-{i}").into()))
                            .pl(px(indent))
                            .pr_2()
                            .py_0p5()
                            .text_sm()
                            .cursor_pointer()
                            .hover(|d| d.bg(palette.selection))
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(move |view, _ev, _w, cx| {
                                    view.goto_source(src, cx);
                                }),
                            )
                            .child(title)
                            .into_any_element(),
                    );
                }
                sb.push(
                    div()
                        .id("ol-sep")
                        .h(px(1.))
                        .mt_1()
                        .bg(palette.border)
                        .into_any_element(),
                );
            }
        }
        if self.tree_open
            && let Some(ref ws) = self.workspace
            && let Some(ref root) = self.workspace_root
            && let Ok(list) = ws.list_markdown(&DocPath::root())
        {
            for entry in list {
                let name = entry.name.clone();
                let rel = entry.path.to_string();
                let full = root.join(&rel);
                let cur = self.path.as_ref().is_some_and(|cp| cp.ends_with(&rel));
                // 重命名态：该行渲染为可编辑输入
                let renaming_this = self.renaming.as_ref().is_some_and(|(rp, _)| rp == &full);
                if renaming_this {
                    let edit_text = self
                        .renaming
                        .as_ref()
                        .map(|(_, t)| t.clone())
                        .unwrap_or_default();
                    sb.push(
                        div()
                            .id(gpui::ElementId::Name(format!("sb-edit-{rel}").into()))
                            .px_3()
                            .py_1()
                            .text_sm()
                            .bg(palette.selection)
                            .child(edit_text)
                            .into_any_element(),
                    );
                } else {
                    let full_open = full.clone();
                    let full_ren = full.clone();
                    let full_del = full.clone();
                    sb.push(
                        div()
                            .id(gpui::ElementId::Name(format!("sb-{rel}").into()))
                            .px_3()
                            .py_1()
                            .text_sm()
                            .flex()
                            .justify_between()
                            .items_center()
                            .when(cur, |d| d.bg(palette.selection))
                            .child(
                                div()
                                    .flex_1()
                                    .cursor_pointer()
                                    .on_mouse_down(
                                        gpui::MouseButton::Left,
                                        cx.listener(move |view, _ev, _w, cx| {
                                            view.open_path(full_open.clone(), cx);
                                        }),
                                    )
                                    .child(name),
                            )
                            .child(
                                div()
                                    .cursor_pointer()
                                    .text_xs()
                                    .on_mouse_down(
                                        gpui::MouseButton::Left,
                                        cx.listener(move |view, _ev, _w, cx| {
                                            view.start_rename(full_ren.clone(), cx);
                                        }),
                                    )
                                    .child("✎"),
                            )
                            .child(
                                div()
                                    .cursor_pointer()
                                    .text_xs()
                                    .on_mouse_down(
                                        gpui::MouseButton::Left,
                                        cx.listener(move |view, _ev, _w, cx| {
                                            view.delete_file(full_del.clone(), cx);
                                        }),
                                    )
                                    .child("✕"),
                            )
                            .into_any_element(),
                    );
                }
            }
        }
        let sidebar_el = div()
            .id("sidebar")
            .w(px(220.))
            .flex_none()
            .h_full()
            .border_r_1()
            .border_color(palette.border)
            .bg(palette.subtle)
            .flex()
            .flex_col()
            .children(sb);
        // 标签栏
        let mut tbc: Vec<gpui::AnyElement> = Vec::new();
        for (i, t) in self.tabs.iter().enumerate() {
            let name = t.name();
            let act = i == self.active;
            tbc.push(
                div()
                    .id(gpui::ElementId::Name(format!("tab-{i}").into()))
                    .px_2()
                    .py_0p5()
                    .text_sm()
                    .cursor_pointer()
                    .when(act, |d| d.bg(palette.selection))
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |v, _ev, _w, cx| {
                            v.active = i;
                            cx.notify();
                        }),
                    )
                    .child(name)
                    .into_any_element(),
            );
        }
        tbc.push(
            div()
                .id("tab-new")
                .px_2()
                .py_0p5()
                .cursor_pointer()
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|v, _ev, _w, cx| {
                        v.new_tab_empty(cx);
                    }),
                )
                .child("+")
                .into_any_element(),
        );
        let tabbar_el = h_flex()
            .id("tabbar")
            .w_full()
            .h(px(26.))
            .items_center()
            .border_b_1()
            .border_color(palette.border)
            .bg(palette.subtle)
            .children(tbc);
        div()
            .id("app")
            .size_full()
            .flex()
            .flex_row()
            .bg(palette.background)
            .text_color(palette.foreground)
            .child(sidebar_el)
            // 右侧主列：标签栏 + 信息条 + 查找/命令浮层 + 编辑区（纵向排列）
            .child(
                div()
                    .id("doc")
                    .h_full()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.))
                    .child(tabbar_el)
                    .key_context("mazha-doc")
                    .track_focus(&focus)
                    .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                        handle_key(view, event, window, cx);
                    }))
                    // 左键按下：先试任务复选框切换，再放置光标
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|view, event: &MouseDownEvent, window, cx| {
                            // Cmd+点击（Windows/Linux 为 Ctrl+点击）链接：交给系统默认应用打开，不移动光标
                            if event.modifiers.secondary()
                                && let Some(url) = view.link_at(event.position)
                            {
                                cx.open_url(&url);
                                return;
                            }
                            // 点在任务复选框上：切换 `[ ]` ↔ `[x]`（命中失败则继续放光标）
                            view.toggle_task_at(event.position, cx);
                            if let Some(src) = view.hit_source(event.position) {
                                if event.modifiers.alt {
                                    // Alt+Click: 添加/移除光标
                                    view.add_or_remove_cursor(src);
                                } else {
                                    // 普通点击: 重置为单光标
                                    view.cursors = vec![Cursor::new(src)];
                                    view.primary = 0;
                                    view.selecting = true;
                                }
                            }
                            let focus = view.focus.clone();
                            window.focus(&focus, cx);
                            cx.notify();
                        }),
                    )
                    // 拖动中：持续更新主光标
                    .on_mouse_move(cx.listener(|view, event: &MouseMoveEvent, _, cx| {
                        if !view.selecting {
                            return;
                        }
                        if let Some(src) = view.hit_source(event.position) {
                            if let Some(c) = view.cursors.get_mut(view.primary) {
                                c.caret = view.doc.snap_to_boundary(src);
                                c.anchor = Some(c.anchor.unwrap_or(c.caret)); // 保持锚点
                            }
                            cx.notify();
                        }
                    }))
                    // 左键松开：结束拖选
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|view, _event: &MouseUpEvent, _, cx| {
                            view.selecting = false;
                            cx.notify();
                        }),
                    )
                    // 顶部信息条
                    .child(
                        h_flex()
                            .id("header")
                            .w_full()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .h(px(28.))
                            .border_b_1()
                            .border_color(palette.border)
                            .text_sm()
                            .text_color(palette.muted)
                            .child(
                                self.tabs
                                    .get(self.active)
                                    .map(|t| t.name())
                                    .unwrap_or_else(|| self.header_text()),
                            )
                            .child(format!(
                                "· {}",
                                self.tabs
                                    .get(self.active)
                                    .map(|t| t.full())
                                    .unwrap_or_else(|| self.header_full_path())
                            ))
                            .child(format!(
                                "· {line_count} 行 · 光标×{} · Cmd+O 打开 · Cmd+S 保存 · Cmd+Shift+S 另存为 · Cmd+F 查找替换",
                                caret_count
                            )),
                    )
                    // 查找/替换条（`Cmd+F` 展开时才渲染）
                    .children(find_bar)
                    .children(palette_overlay)
                    // 正文滚动区：滚动**只包正文**——标签栏/信息条不随之卷走。
                    // 自己 `track_scroll` 绑定 `self.scroll`：可见区/滚动量回填到该句柄，
                    // 供命中测试与光标滚动跟随使用；DocElement 上报绝对像素高度，
                    // 内容超视口才有滚动量（D65）。
                    .child(
                        // 外层只负责定位：滚动条叠加在视口上（不随内容位移），
                        // 内层才是滚动容器（内容随 `self.scroll` 滚动）。
                        div()
                            .id("editor")
                            .relative()
                            .flex_1()
                            .min_h(px(0.))
                            .child(
                                div()
                                    .id("editor-scroll")
                                    .absolute()
                                    .inset_0()
                                    .overflow_y_scroll()
                                    .track_scroll(&self.scroll)
                                    .child(DocElement { view: cx.entity() }),
                            )
                            .child(
                                div().absolute().inset_0().child(
                                    Scrollbar::vertical(&self.scroll)
                                        .id("editor-scrollbar")
                                        .viewport_from_layout(),
                                ),
                            ),
                    ),
            )
    }
}

/// IME / 文本输入契约：把 macOS/系统 IME 的组合、提交、选区查询桥接到源文本模型。
///
/// 偏移约定：`EntityInputHandler` 的区间参数/返回值一律是 **UTF-16 码元下标**
/// （AppKit `NSRange` 语义），而本视图的 `caret`/`anchor`/`Document::apply` 用源文本
/// **UTF-8 字节偏移**。因此每个方法都必须显式换算；漏换算会在含中文的文档里错位，
/// 并可能让输入法无法建立组合（表现为输入法把字母直接上屏）。
impl EntityInputHandler for DocView {
    /// 查询 `range`（UTF-16）对应的文本，供 IME 剪贴/转换使用。
    ///
    /// `adjusted_range` 回填"实际取用"的区间（UTF-16，IME 据此调整 marked text）。
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let (start, end) = self.source_range_from_utf16(range)?;
        let text = self.doc.try_slice(start..end).ok()?.into_owned();
        let (u_lo, u_hi) = self.utf16_range(start, end);
        *adjusted_range = Some(u_lo..u_hi);
        Some(text)
    }

    /// 当前选区（UTF-16 表示）；`reversed` 表示锚点在区间末端（反向选区）。
    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let c = self.primary_cursor();
        let (start, end, reversed) = match c.anchor {
            Some(anchor) if anchor != c.caret => {
                let (lo, hi) = if anchor < c.caret {
                    (anchor, c.caret)
                } else {
                    (c.caret, anchor)
                };
                (lo, hi, anchor > c.caret)
            }
            _ => (c.caret, c.caret, false),
        };
        let (u16_start, u16_end) = self.utf16_range(start, end);
        Some(UTF16Selection {
            range: u16_start..u16_end,
            reversed,
        })
    }

    /// IME 组合文本（marked text）对应的 **UTF-16** 区间；`None` = 无组合。
    ///
    /// 内部 `marked` 存的是源字节区间，返回给 AppKit 前必须换算成 UTF-16，
    /// 否则 `setMarkedText:` 的替换区间会与文档错位，组合文本会不断被追加/错删。
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        let marked = self.primary_cursor().marked.clone()?;
        let (lo, hi) = self.utf16_range(marked.start, marked.end);
        Some(lo..hi)
    }

    /// IME 取消组合：删除 marked 区间文本，光标回到组合起点。
    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let c = self.primary_cursor_mut();
        if let Some(marked) = c.marked.take() {
            let at = marked.start;
            self.apply_edit(Edit::Delete { range: marked }, at, cx);
        }
    }

    /// 用 `text` 替换 `range`（UTF-16；`None` = 当前选区）。IME 每次提交字符都走这里。
    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = self.ime_target(range.clone());
        self.replace_selection_with_inner(target, text, cx);
    }

    /// 替换并进入组合状态：写入 `new_text`，把该区间标记为 marked，并设定组合内选区。
    /// 替换并进入组合状态：写入 `new_text`，把该区间标记为 marked，并设定组合内选区。
    ///
    /// `range`/`new_selected_range` 都是 UTF-16（后者相对 `new_text` 起点）。
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = self.ime_target(range.clone());
        let start = target.start;
        self.replace_selection_with_inner(target, new_text, cx);
        let marked_end = start + new_text.len();
        let c = self.primary_cursor_mut();
        c.marked = (marked_end > start).then_some(start..marked_end);
        // 组合内选区：相对 marked 起点的 UTF-16 下标 → 相对字节 → 绝对源偏移
        if let Some(rel) = new_selected_range {
            let abs_start = self
                .doc
                .snap_to_boundary(start + utf16_index_to_byte(new_text, rel.start));
            let abs_end = self
                .doc
                .snap_to_boundary(start + utf16_index_to_byte(new_text, rel.end));
            let c = self.primary_cursor_mut();
            c.anchor = Some(abs_start);
            c.caret = abs_end;
        }
        cx.notify();
    }

    /// IME 需要在屏幕上高亮某个 UTF-16 区间时（如组合下划线），换算成屏幕矩形。
    ///
    /// 用最近一次渲染缓存的块几何（`layout`）定位；无几何时返回 `None`（跳过高亮）。
    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let (start, end) = self.source_range_from_utf16(range_utf16)?;
        // 文档级映射：起止点各得（行号, 显示列）；跨行时高亮覆盖到终点所在行
        let (p0, p1) = {
            let projection = self.doc.projection();
            (
                projection.source_to_doc_pos(start),
                projection.source_to_doc_pos(end),
            )
        };
        let layout = self.layout.as_ref()?;
        let b0 = layout.blocks.get(p0.line)?;
        let line0 = layout.block_line_height(p0.line);
        // 列一律经块几何换算（展开态显示文本更长，投影列会画偏）
        let pos0 = b0
            .shaped
            .position_for_index(b0.byte_of_col(b0.col_of_source(start)), line0)
            .unwrap_or_else(|| point(Pixels::ZERO, Pixels::ZERO));
        let x0 = layout.origin.x + b0.indent + pos0.x;
        let y0 = layout.origin.y + layout.row_top(layout.row_starts[p0.line]) + pos0.y;
        let (x1, y1) = match layout.blocks.get(p1.line) {
            Some(b1) => {
                let line1 = layout.block_line_height(p1.line);
                let pos1 = b1
                    .shaped
                    .position_for_index(b1.byte_of_col(b1.col_of_source(end)), line1)
                    .unwrap_or_else(|| point(Pixels::ZERO, Pixels::ZERO));
                (
                    layout.origin.x + b1.indent + pos1.x,
                    layout.origin.y + layout.row_top(layout.row_starts[p1.line]) + pos1.y,
                )
            }
            None => (x0, y0),
        };
        // 跨视觉行的候选框：取起止点包络（含起点所在视觉行的高度）
        let top = y0.min(y1);
        let height = (y0.max(y1) + line0) - top;
        let width = if x1 > x0 { x1 - x0 } else { x0 - x1 };
        let width = if width < px(2.) { px(2.) } else { width };
        Some(bounds(
            point(x0.min(x1), top),
            size(width, height.max(line0)),
        ))
    }

    /// 屏幕点 → 文档 UTF-8 字节偏移（供 IME 判断光标跟随位置）。
    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        self.hit_source(point)
    }

    /// IME 主动设置选区（UTF-16 区间）；映射回源字节并更新主光标的 caret/anchor。
    fn set_selected_text_range(
        &mut self,
        range_utf16: Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((start, end)) = self.source_range_from_utf16(range_utf16) {
            let c = self.primary_cursor_mut();
            if start == end {
                c.anchor = None;
                c.caret = start;
            } else {
                c.anchor = Some(start);
                c.caret = end;
            }
            cx.notify();
        }
    }

    /// 整篇文档长度（UTF-16 单元数）。
    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        let text = self.doc.text();
        Some(text.encode_utf16().count())
    }
}

/// 字符串内的 UTF-16 下标 → UTF-8 字节偏移（越界钳到末尾）。
///
/// IME 的组合内选区以 UTF-16 表示且相对刚提交的 `new_text`，而文档模型用字节偏移。
fn utf16_index_to_byte(text: &str, index: usize) -> usize {
    let mut u16_seen = 0usize;
    for (byte, ch) in text.char_indices() {
        if u16_seen >= index {
            return byte;
        }
        u16_seen += ch.len_utf16();
    }
    text.len()
}

/// 键盘事件 → 光标移动 / 编辑命令 / 撤销重做 / 全选。
///
/// 字符输入主要经 IME（`EntityInputHandler::replace_text_in_range`）送达；
/// 这里兜底处理无 IME 的 ASCII 直键，以及退格/删除/回车/Tab/撤销/重做/全选。
fn handle_key(
    view: &mut DocView,
    event: &KeyDownEvent,
    window: &mut Window,
    cx: &mut Context<DocView>,
) {
    let m = &event.keystroke.modifiers;
    let shift = m.shift;

    // 重命名文件树条目时：按键作为文本编辑（Enter 确认、Esc 取消、Backspace 删除）
    if view.renaming.is_some() {
        let key = event.keystroke.key.as_str();
        match key {
            "enter" | "return" => view.commit_rename(cx),
            "escape" => view.cancel_rename(cx),
            "backspace" => {
                if let Some((_, t)) = view.renaming.as_mut() {
                    t.pop();
                }
                cx.notify();
            }
            k if k.chars().count() == 1 && !k.chars().next().unwrap().is_control() => {
                if let Some((_, t)) = view.renaming.as_mut() {
                    t.push(k.chars().next().unwrap());
                }
                cx.notify();
            }
            _ => {}
        }
        return;
    }

    // 命令面板展开时：所有按键优先交给面板处理
    if view.cmd_palette.open {
        handle_palette_key(view, event, window, cx);
        return;
    }

    // 查找条展开时：所有按键优先交给查找处理（避免输入查询时误编辑正文）
    if view.find.open {
        handle_find_key(view, event, cx);
        return;
    }

    // Cmd/Ctrl 组合键：撤销/重做/全选/剪贴板/格式化/块类型；其余交给系统，避免误拦截
    if m.control || m.platform {
        match event.keystroke.key.as_str() {
            "z" if m.platform => {
                if shift {
                    view.doc.redo();
                } else {
                    view.doc.undo();
                }
                view.after_doc_mutation(cx);
            }
            "a" if m.platform => {
                // 全选：所有光标合并为一个，覆盖全文
                let len = view.doc.len();
                view.cursors = vec![Cursor::new(len)];
                view.cursors[0].anchor = Some(0);
                view.primary = 0;
                cx.notify();
            }
            // 剪贴板（Cmd+C/X/V）：复制/剪切选区、粘贴
            "c" if m.platform => {
                view.copy_selection(cx);
            }
            "x" if m.platform && !shift => {
                view.cut_selection(cx);
            }
            "v" if m.platform => {
                view.paste(cx);
            }
            // 打开查找/替换条（展开时 Cmd+F 由 `handle_find_key` 关闭）
            "f" if m.platform => {
                view.open_find(cx);
            }
            "p" if m.platform => {
                view.open_palette(cx);
            }
            "o" if m.platform && shift => {
                view.outline_open = !view.outline_open;
                cx.notify();
            }
            "o" if m.platform => {
                view.open_file_dialog(window, cx);
            }
            "t" if m.platform => {
                view.new_tab_empty(cx);
            }
            k if m.platform && k.len() == 1 && k.chars().next().unwrap().is_ascii_digit() => {
                let c = k.chars().next().unwrap();
                if c == '0' {
                    view.switch_tab(view.tabs.len().saturating_sub(1), cx);
                } else if let Some(d) = c.to_digit(10) {
                    let idx = (d as usize).saturating_sub(1);
                    view.switch_tab(idx, cx);
                }
            }
            "w" if m.platform && !shift => {
                view.close_current_tab(cx);
            }
            "w" if m.platform && shift => {
                view.close_all_tabs(cx);
            }
            "s" if m.platform && !shift => {
                view.save_current(window, cx);
            }
            "s" if m.platform && shift => {
                view.save_as(window, cx);
            }
            // 行内格式（Cmd+键）
            "b" if m.platform => {
                view.apply_inline(InlineMark::Strong, cx);
            }
            "i" if m.platform => {
                view.apply_inline(InlineMark::Emphasis, cx);
            }
            "x" if m.platform && shift => {
                view.apply_inline(InlineMark::Strikethrough, cx);
            }
            "`" if m.platform => {
                view.apply_inline(InlineMark::Code, cx);
            }
            // 块类型（Cmd+数字/Shift+数字）
            k if m.platform
                && k.len() == 1
                && k.chars().next().is_some_and(|c| c.is_ascii_digit()) =>
            {
                let level = k.parse::<u8>().unwrap_or(1);
                if (1..=6).contains(&level) {
                    view.apply_block(BlockTarget::Heading(level), cx);
                }
            }
            "8" if m.platform && shift => {
                view.apply_block(BlockTarget::List { ordered: false }, cx);
            }
            "9" if m.platform && shift => {
                view.apply_block(BlockTarget::List { ordered: true }, cx);
            }
            "0" if m.platform && shift => {
                view.apply_block(BlockTarget::BlockQuote, cx);
            }
            "-" if m.platform && shift => {
                view.apply_block(BlockTarget::Paragraph, cx);
            }
            // 表格结构编辑（Cmd+Option+方向键 / Cmd+Option+Delete）
            "up" if m.platform && m.alt => {
                view.apply_table(TableOp::InsertRowAbove, cx);
            }
            "down" if m.platform && m.alt => {
                view.apply_table(TableOp::InsertRowBelow, cx);
            }
            "left" if m.platform && m.alt => {
                view.apply_table(TableOp::InsertColumnLeft, cx);
            }
            "right" if m.platform && m.alt => {
                view.apply_table(TableOp::InsertColumnRight, cx);
            }
            "backspace" | "delete" if m.platform && m.alt && shift => {
                view.apply_table(TableOp::DeleteColumn, cx);
            }
            "backspace" | "delete" if m.platform && m.alt => {
                view.apply_table(TableOp::DeleteRow, cx);
            }
            "e" if m.platform => {
                view.export_pdf(window, cx);
            }
            _ => {}
        }
        return;
    }

    match event.keystroke.key.as_str() {
        "left" => view.move_left(shift),
        "right" => view.move_right(shift),
        "up" => view.move_up(shift),
        "down" => view.move_down(shift),
        "home" => view.move_home(shift),
        "end" => view.move_end(shift),
        "backspace" => {
            view.delete_backward(cx);
        }
        "delete" => {
            view.delete_forward(cx);
        }
        "enter" | "return" => {
            view.insert_newline(cx);
        }
        "tab" => {
            view.insert_tab(cx);
        }
        // 可打印字符不在这里插入：交给 IME（`replace_text_in_range`）。
        // 若在这里直插，会抢在输入法组合之前把字母写进文档（中文输入法下表现为
        // "字母直接上屏、无法组词"），且英文输入时也会与 IME 提交重复。
        _ => return,
    }
    cx.notify();
}

/// 查找条展开时的按键处理（`handle_key` 在 `find.open` 时全部转到这里）。
///
/// 语义：`Enter`/`Shift+Enter` 上/下一个匹配、`Tab` 切换查找/替换框、`Backspace`/`Delete`
/// 编辑聚焦框、`Esc` 关闭、`Cmd+F` 再按关闭、`Cmd+Enter` 替换当前、`Cmd+Shift+Enter`
/// 替换全部、`Cmd+Alt+C` 切换大小写敏感；其余 `Cmd` 组合一律忽略（不在查找时改正文）。
/// 命令面板展开时的按键处理（`handle_key` 在 `cmd_palette.open` 时全部转到这里）。
///
/// 语义：可打印字符追加到查询、`Backspace` 删除、`↑`/`↓` 移动选择、`Enter` 执行、
/// `Esc`/`Cmd+P` 关闭；其余 `Cmd` 组合忽略。
fn handle_palette_key(
    view: &mut DocView,
    event: &KeyDownEvent,
    window: &mut Window,
    cx: &mut Context<DocView>,
) {
    let m = &event.keystroke.modifiers;
    let key = event.keystroke.key.as_str();
    // Cmd+P 再按：关闭
    if m.platform && key == "p" {
        view.close_palette(cx);
        return;
    }
    // Esc：关闭
    if key == "escape" {
        view.close_palette(cx);
        return;
    }
    let filtered = view.palette_filtered();
    match key {
        "up" => {
            if !filtered.is_empty() {
                view.cmd_palette.selected =
                    (view.cmd_palette.selected + filtered.len() - 1) % filtered.len();
            }
        }
        "down" => {
            if !filtered.is_empty() {
                view.cmd_palette.selected = (view.cmd_palette.selected + 1) % filtered.len();
            }
        }
        "enter" | "return" => {
            if let Some(&idx) = filtered.get(view.cmd_palette.selected) {
                let action = view.palette_commands()[idx].action;
                view.run_palette_action(action, window, cx);
            }
            return;
        }
        "backspace" => {
            view.cmd_palette.query.pop();
            view.cmd_palette.selected = 0;
        }
        _ => {
            // 可打印 ASCII 直键兜底（命令面板无 IME）
            if key.chars().count() == 1 && !key.chars().next().unwrap().is_control() {
                view.cmd_palette.query.push(key.chars().next().unwrap());
                view.cmd_palette.selected = 0;
            }
        }
    }
    cx.notify();
}

fn handle_find_key(view: &mut DocView, event: &KeyDownEvent, cx: &mut Context<DocView>) {
    let m = &event.keystroke.modifiers;
    let key = event.keystroke.key.as_str();

    if m.control || m.platform {
        match key {
            // Cmd+F：关闭查找条（再次按下切换）
            "f" if m.platform => {
                view.close_find(cx);
            }
            // Cmd+Enter：替换当前匹配；Cmd+Shift+Enter：替换全部
            "enter" | "return" if m.platform => {
                if m.shift {
                    view.replace_all_now(cx);
                } else {
                    view.replace_current(cx);
                }
            }
            // Cmd+Alt+C：切换大小写敏感并重算
            "c" if m.platform && m.alt => {
                view.find.case_sensitive = !view.find.case_sensitive;
                view.recompute_matches();
                if view.find.matches.is_empty() {
                    cx.notify();
                } else {
                    view.goto_match(0, cx);
                }
            }
            _ => {}
        }
        return;
    }

    match key {
        "escape" => {
            view.close_find(cx);
        }
        // Tab：在查找/替换框之间切换焦点
        "tab" => {
            view.find.focus_replace = !view.find.focus_replace;
            cx.notify();
        }
        // Enter：下一个匹配；Shift+Enter：上一个匹配
        "enter" | "return" => {
            if m.shift {
                view.find_prev(cx);
            } else {
                view.find_next(cx);
            }
        }
        // 退格/删除：编辑聚焦框的文本
        "backspace" => {
            if view.find.focus_replace {
                view.find.replace.pop();
            } else {
                view.find.query.pop();
                view.recompute_matches();
            }
            cx.notify();
        }
        "delete" => {
            if view.find.focus_replace {
                view.find.replace.clear();
            } else {
                view.find.query.clear();
                view.recompute_matches();
            }
            cx.notify();
        }
        // 可打印字符：追加到聚焦的输入框；改查找框时即时重算并跳到首个匹配
        k if k.chars().count() == 1 && !k.chars().next().unwrap().is_control() => {
            let ch = k.chars().next().unwrap();
            if view.find.focus_replace {
                view.find.replace.push(ch);
                cx.notify();
            } else {
                view.find.query.push(ch);
                view.recompute_matches();
                if view.find.matches.is_empty() {
                    cx.notify();
                } else {
                    view.goto_match(0, cx);
                }
            }
        }
        _ => {}
    }
}

/// 渲染查找/替换条里的一个输入框（静态文本 + 聚焦高亮边框；非 gpui 输入组件）。
///
/// `focused` 时边框用强调色并在空框里显示占位光标 `_`。
fn find_field(id: &'static str, value: &str, focused: bool, palette: &Palette) -> impl IntoElement {
    let shown = if value.is_empty() && focused {
        "_".to_string()
    } else {
        value.to_string()
    };
    div()
        .id(id)
        .min_w(px(160.))
        .flex_none()
        .items_center()
        .px_2()
        .h(px(22.))
        .rounded_md()
        .border_1()
        .border_color(if focused {
            palette.accent
        } else {
            palette.border
        })
        .bg(palette.subtle)
        .text_color(palette.foreground)
        .child(shown)
}

/// 把 core 的块样式映射为排版字号（标题放大、代码块等宽字号）。
fn block_font_size(block: &BlockStyle, base: Pixels, mono: Pixels) -> Pixels {
    match block.heading {
        Some(1) => base * 1.75,
        Some(2) => base * 1.5,
        Some(3) => base * 1.25,
        Some(4) => base * 1.1,
        Some(5) => base * 1.05,
        Some(6) => base * 1.0,
        // 代码块整行等宽字号（行内代码仍是正文行高内的 run 样式，不放大）
        _ if block.code_block => mono,
        _ => base,
    }
}

/// 本块**每个视觉行**的行高：字号 × 1.5（标题 × 1.6 留出上下呼吸），且不小于 [`LINE_HEIGHT`]。
///
/// 标题字号大于正文，若仍用统一行高，字形会溢出行盒压到相邻行（见 `WrappedLine::paint`
/// 的垂直居中：`padding_top = (line_height - ascent - descent) / 2`）。
fn block_line_px(block: &BlockStyle, font_size: Pixels) -> Pixels {
    let factor = if block.heading.is_some() { 1.6 } else { 1.5 };
    (font_size * factor).max(LINE_HEIGHT)
}

fn italic_fallbacks() -> gpui_kit::FontFallbacks {
    gpui_kit::FontFallbacks::from_fonts(vec![
        "PingFang SC".into(),
        "Hiragino Sans GB".into(),
        "Songti SC".into(),
        "Heiti SC".into(),
        "Apple Symbols".into(),
        "Apple Color Emoji".into(),
    ])
}

/// 把一个 core [`TextRun`]（片段 + 样式）转成 gpui 的 [`gpui::TextRun`]（字形级样式）。
///
/// 对中文斜体：仅 CJK 字符需自绘合成，其他字符走 gpui 正常渲染；
/// 中文片段置为透明占位以避免直立重复绘制。
fn to_gpui_runs(
    run: &DocTextRun,
    base_font: &Font,
    mono_font: &Font,
    palette: &Palette,
) -> (String, Vec<gpui::TextRun>, Vec<oblique::ObliqueSpan>) {
    // 默认按正文样式；代码块整体覆盖为等宽字体（字号统一在 shape_line 时按块应用）
    let mut font = base_font.clone();
    let mut color = palette.foreground;
    let mut background = None;
    let mut underline = None;
    let mut strikethrough = None;

    match run.style {
        RunStyle::Normal => {}
        RunStyle::Emphasis => {
            font = font.italic();
            font.fallbacks = Some(italic_fallbacks());
        }
        RunStyle::Strong => font = font.bold(),
        RunStyle::Strikethrough => {
            strikethrough = Some(StrikethroughStyle {
                thickness: px(1.),
                color: Some(color),
            });
        }
        RunStyle::InlineCode => {
            font = mono_font.clone();
            color = palette.foreground;
            background = Some(palette.code_bg);
        }
        // 行内公式体（D34 ④）：斜体 + 主题色，与普通文本区分
        RunStyle::Math => {
            font = font.italic();
            font.fallbacks = Some(italic_fallbacks());
            color = palette.accent;
        }
        // 脚注标签（D35 ⑤）：主题色区分。gpui 字号按行设置（shape_line），
        // 无法逐 run 缩小字号，故这里只改颜色，不做上标/小号处理。
        RunStyle::Footnote => {
            color = palette.accent;
        }
        RunStyle::LinkText => {
            color = palette.link;
            underline = Some(UnderlineStyle {
                thickness: px(1.),
                color: Some(palette.link),
                wavy: false,
            });
        }
    }

    let text = run.text.clone();

    if matches!(run.style, RunStyle::Emphasis | RunStyle::Math) {
        let base_family = font.family.as_ref();
        let mut runs_out: Vec<gpui::TextRun> = Vec::new();
        let mut oblique_list: Vec<oblique::ObliqueSpan> = Vec::new();
        let mut pos = 0;
        for (range, is_cjk) in oblique::split_segments(&text) {
            let seg = &text[range.clone()];
            if seg.is_empty() {
                continue;
            }
            if is_cjk && !oblique::primary_covers(base_family, seg.chars().next().unwrap_or('中'))
            {
                runs_out.push(gpui::TextRun {
                    len: seg.len(),
                    font: font.clone(),
                    color: Hsla { a: 0.0, ..color },
                    background_color: background,
                    underline: None,
                    strikethrough: None,
                });
                oblique_list.push(oblique::ObliqueSpan {
                    range: pos..pos + seg.len(),
                    color,
                });
            } else {
                runs_out.push(gpui::TextRun {
                    len: seg.len(),
                    font: font.clone(),
                    color,
                    background_color: background,
                    underline: None,
                    strikethrough: None,
                });
            }
            pos += seg.len();
        }
        if !runs_out.is_empty() {
            return (text, runs_out, oblique_list);
        }
    }

    let gpui_run = gpui::TextRun {
        len: run.text.len(),
        font,
        color,
        background_color: background,
        underline,
        strikethrough,
    };
    (run.text.clone(), vec![gpui_run], Vec::new())
}

/// 把一整块投影行（引用前缀 + 可见片段）拼成一条不换行的可见文本 + 对应 gpui 样式段。
///
/// 返回 `(完整可见文本, gpui_runs, oblique_spans)`。引用前缀 `> ` 用 muted 色，其余用片段自身样式。
/// `highlight` 为代码块行的语法高亮片段（相对该行显示文本；`None` = 不高亮）。
///
/// 块级样式（标题整块加粗、已完成任务删除线 + 弱化）在这里逐 run 施加。
fn build_line_runs(
    line: &ProjectedLine,
    base_font: &Font,
    mono_font: &Font,
    palette: &Palette,
    highlight: Option<&[ColoredRun]>,
) -> (SharedString, Vec<gpui::TextRun>, Vec<oblique::ObliqueSpan>) {
    let mut text = String::new();
    let mut runs: Vec<gpui::TextRun> = Vec::new();
    let mut oblique_spans: Vec<oblique::ObliqueSpan> = Vec::new();

    // 引用块：为每个嵌套深度补一个 muted 的 "> " 前缀段
    for _ in 0..line.block.quote_depth {
        let prefix = "> ";
        runs.push(gpui::TextRun {
            len: prefix.len(),
            font: base_font.clone(),
            color: palette.muted,
            background_color: None,
            underline: None,
            strikethrough: None,
        });
        text.push_str(prefix);
    }

    // 代码块行 + 有语法高亮：按高亮区间切分可见文本，逐段着色（等宽 + 弱底色）。
    if line.block.code_block
        && let Some(spans) = highlight
    {
        let content = line.display_text();
        for span in spans {
            let Some(seg) = content.get(span.range.clone()) else {
                continue; // 防御：越界片段跳过
            };
            if seg.is_empty() {
                continue;
            }
            runs.push(gpui::TextRun {
                len: seg.len(),
                font: mono_font.clone(),
                color: rgb_to_hsla(span.color),
                background_color: Some(palette.subtle),
                underline: None,
                strikethrough: None,
            });
            text.push_str(seg);
        }
    } else {
        // 其余情况（含未高亮的代码块）：按投影片段拼接
        for run in &line.runs {
            if line.block.code_block {
                // 代码块行：强制等宽字体 + 整行弱底色
                let gp_run = gpui::TextRun {
                    len: run.text.len(),
                    font: mono_font.clone(),
                    color: palette.foreground,
                    background_color: Some(palette.subtle),
                    underline: None,
                    strikethrough: None,
                };
                text.push_str(&run.text);
                runs.push(gp_run);
                continue;
            }
            let (seg_text, seg_runs, seg_obs) = to_gpui_runs(run, base_font, mono_font, palette);
            for obs in seg_obs {
                oblique_spans.push(oblique::ObliqueSpan {
                    range: (text.len() + obs.range.start)..(text.len() + obs.range.end),
                    color: obs.color,
                });
            }
            text.push_str(&seg_text);
            runs.extend(seg_runs);
        }
    }

    // 标题整块加粗（gpui 字体权重按 run 施加；字号在 shape 时按块给定）
    if line.block.heading.is_some() {
        for r in &mut runs {
            r.font = r.font.clone().bold();
        }
    }
    // 已完成任务：整行删除线 + 弱化色，一眼区分"做完的待办"
    if line.block.task == Some(true) {
        for r in &mut runs {
            r.color = palette.muted;
            r.strikethrough = Some(StrikethroughStyle {
                thickness: px(1.),
                color: Some(palette.muted),
            });
        }
    }

    // 分割线：不在此处画字符，改由 `paint` 依块级 widget 画一条真实细线（SPEC §11.5、D30）。

    (text.into(), runs, oblique_spans)
}

/// 把 `(r, g, b)`（syntect 前景色）转成 gpui 的 [`Hsla`]。
fn rgb_to_hsla((r, g, b): (u8, u8, u8)) -> Hsla {
    let hex = (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
    gpui::rgb(hex).into()
}

/// 把单行显示文本按 `wrap_width` 形状化为一条**软换行**的 [`WrappedLine`]。
///
/// 输入不含 `\n`，故 `shape_text` 恰好返回一条 `WrappedLine`（内部可能含多个换行边界）。
/// 形状化失败或返回空（理论上不发生：空文本/覆盖完整的 runs 都能成功）时，退化为空行，
/// 保证布局块序列与投影行一一对应。
fn shape_wrapped_line(
    window: &mut Window,
    text: SharedString,
    font_size: Pixels,
    runs: &[gpui::TextRun],
    wrap_width: Pixels,
) -> WrappedLine {
    let ts = window.text_system().clone();
    if let Ok(mut lines) = ts.shape_text(text, font_size, runs, Some(wrap_width), None)
        && let Some(line) = lines.pop()
    {
        return line;
    }
    ts.shape_text(
        SharedString::default(),
        font_size,
        &[],
        Some(wrap_width),
        None,
    )
    .ok()
    .and_then(|mut lines| lines.pop())
    .expect("空行形状化必然成功")
}

/// 已换行行的每视觉行起止**字节偏移**（长度 = 视觉行数 + 1）。
///
/// 换行边界以 `run_ix`/`glyph_ix` 指向的**字形**表示，取该字形的字节 `index` 即为下一
/// 视觉行的起始字节。用于把选区的字节区间按视觉行切分。
fn row_byte_bounds(w: &WrappedLine) -> Vec<usize> {
    let mut v = Vec::with_capacity(w.wrap_boundaries().len() + 2);
    v.push(0);
    for b in w.wrap_boundaries() {
        let idx = w
            .unwrapped_layout
            .runs
            .get(b.run_ix)
            .and_then(|r| r.glyphs.get(b.glyph_ix))
            .map_or_else(|| w.len(), |g| g.index);
        v.push(idx);
    }
    v.push(w.len());
    v
}

/// 块在屏幕上的横向缩进（引用深度 + 列表缩进），单位为像素。
fn block_indent(line: &ProjectedLine, base: Pixels) -> Pixels {
    let mut x = base;
    // 引用每层缩进一个基数
    x += base * line.block.quote_depth as f32;
    // 列表项符号占位（真实渲染列表符号需字体字形，这里用缩进近似）
    if line.block.bullet.is_some() {
        x += base * 1.5;
    }
    // 代码块整体右缩进
    if line.block.code_block {
        x += base * 1.5;
    }
    x
}

/// 任务复选框矩形：放在 [`block_indent`] 为列表项预留的 `1.5*BASE` 标记区中央（12×12）。
///
/// `zone_right` = 标记区右缘（即块文本左缘 `indent + origin.x`）；`line_top`/`line_px` =
/// 该行顶边与行高（窗口坐标），框随行垂直居中。prepaint（命中）与 paint 共用，避免两处漂移。
fn task_box_rect(zone_right: Pixels, line_top: Pixels, line_px: Pixels) -> Bounds<Pixels> {
    let zone = BASE * 1.5;
    let box_x = zone_right - zone + (zone - px(12.)) / 2.;
    let box_y = line_top + (line_px - px(12.)) / 2.;
    bounds(point(box_x, box_y), size(px(12.), px(12.)))
}

/// 在装饰面板右上角画语言标签（代码块/图表共用）：距右边框 6px、距上边框 3px。
fn paint_corner_label(
    label: &ShapedLine,
    right: Pixels,
    top: Pixels,
    window: &mut Window,
    cx: &mut App,
) {
    let lx = right - label.width() - px(6.);
    let ly = top + px(3.);
    let _ = label.paint(
        point(lx, ly),
        LINE_HEIGHT - px(6.),
        TextAlign::Left,
        None,
        window,
        cx,
    );
}

/// 用四条 1px 矩形描出一个矩形边框（gpui 的 `paint_quad` 只有填充，故四边各画一条）。
fn paint_rect_border(window: &mut Window, rect: Bounds<Pixels>, color: Hsla, thickness: Pixels) {
    let Bounds {
        origin,
        size: rect_size,
    } = rect;
    let w = rect_size.width;
    let h = rect_size.height;
    // 上边
    window.paint_quad(fill(bounds(origin, size(w, thickness)), color));
    // 下边
    window.paint_quad(fill(
        bounds(
            point(origin.x, origin.y + h - thickness),
            size(w, thickness),
        ),
        color,
    ));
    // 左边
    window.paint_quad(fill(bounds(origin, size(thickness, h)), color));
    // 右边
    window.paint_quad(fill(
        bounds(
            point(origin.x + w - thickness, origin.y),
            size(thickness, h),
        ),
        color,
    ));
}

/// 文档正文的自写元素：逐块 `shape_text`（软换行）连续布局 + 绘制，并把几何回存给视图。
///
/// 之所以自写 [`Element`] 而不用 `div`+`StyledText`：需要在 `prepaint` 里拿到
/// 每块的 `WrappedLine` 几何（换行边界 + `position_for_index`/`closest_index_for_position`）、
/// 每块视觉行数，以及全局视觉行表（`row_starts`），
/// 一次整篇命中测试即可把窗口坐标换算成源偏移（见 [`DocLayout::hit`]）。
struct DocElement {
    /// 视图实体：prepaint 里读取光标/选区并回存 [`DocLayout`]。
    view: Entity<DocView>,
}

/// 排版基数（正文行高 / 内边距）。
const BASE: Pixels = px(16.);
/// 统一行高（正文与等宽同高，保证命中测试的行序与块序一致）。
const LINE_HEIGHT: Pixels = px(24.);

impl Element for DocElement {
    /// 布局阶段无需保留状态（几何在 prepaint 里生成）。
    type RequestLayoutState = ();
    /// 前绘制阶段生成的布局几何（供 `paint` 绘制 + 视图回存）。
    type PrepaintState = DocLayout;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        // 高度 = 上一帧算出的内容总高（prepaint 末尾回填，变化时补一帧收敛）。
        // 这里曾上报 `relative(1.)`：在自适应容器里解析为 0，滚动区 content_size 恒为 0、
        // scroll_max ≤ 0，整篇文档就滚不动了（D65）。
        let content_height = self.view.read(cx).content_height;
        let style = Style {
            size: Size {
                width: Length::Definite(relative(1.)),
                height: Length::Definite(content_height.into()),
            },
            ..Style::default()
        };
        let layout_id = window.request_layout(style, std::iter::empty::<LayoutId>(), cx);
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        // 需要 &mut 投影缓存；先取快照所需的只读字段（caret_line_index 需 &mut，放到下面 update 里算）
        let (palette, base_font, mono_font, base_size, mono_size, selection) = {
            let snapshot = self.view.read(cx);
            (
                snapshot.palette,
                gpui::font(snapshot.ui_font.clone()),
                gpui::font(snapshot.mono_font.clone()),
                BASE,
                snapshot.mono_font_size,
                snapshot.all_selections(),
            )
        };
        let indent_base = BASE;

        // update 提供 &mut DocView（投影缓存需惰性建立），闭包返回几何数据避免借用越界
        let content_width_total = bounds.size.width;
        let (blocks, widgets, row_starts, row_tops, caret_row, task_cand) =
            self.view.update(cx, |view, _| {
                // 需先取所有光标的源偏移，随后 projection() 会可变借用 view.doc（借用不相交）
                let caret_sources: Vec<usize> = view.cursors.iter().map(|c| c.caret).collect();
                let caret = view.cursors[view.primary].caret;
                // 光标所在行源码：用于"光标在链接内时展开该链接的原文"（SPEC §11.2 的
                // 链接局部版本）。先借投影取行号/行区间（借用立即结束），再切出行文本，
                // 避免为一行复制整篇文档。
                let (caret_line, caret_line_range) = {
                    let projection = view.doc.projection();
                    let line = projection.line_of_source(caret);
                    (line, projection.lines[line].source_range.clone())
                };
                let caret_line_src = view.doc.slice(caret_line_range.clone()).into_owned();
                // 光标落在可展开组内（链接、粗体、斜体、删除线、行内代码）才展开
                let caret_reveal = inline_reveals(&caret_line_src).into_iter().find(|r| {
                    caret >= caret_line_range.start + r.range.start
                        && caret <= caret_line_range.start + r.range.end
                });
                let caret_pos = view.doc.projection().source_to_doc_pos(caret);
                // 光标在代码块内（含围栏行）时，把该块所有围栏行的源码显示出来：
                // 围栏行投影后整行隐藏，不显示的话光标落上去"看不见"，也无法编辑 ```lang。
                let fence_ranges: Vec<(usize, Range<usize>)> = {
                    let projection = view.doc.projection();
                    let mut out = Vec::new();
                    if projection
                        .lines
                        .get(caret_pos.line)
                        .is_some_and(|l| l.block.code_block)
                    {
                        let lines = &projection.lines;
                        let mut start = caret_pos.line;
                        while start > 0 && lines[start - 1].block.code_block {
                            start -= 1;
                        }
                        let mut end = caret_pos.line;
                        while end + 1 < lines.len() && lines[end + 1].block.code_block {
                            end += 1;
                        }
                        for (i, line) in lines.iter().enumerate().take(end + 1).skip(start) {
                            out.push((i, line.source_range.clone()));
                        }
                    }
                    out
                };
                let fence_texts: Vec<(usize, String)> = fence_ranges
                    .into_iter()
                    .filter_map(|(i, range)| {
                        let src = view.doc.slice(range).into_owned();
                        let text = src.trim_end_matches(['\n', '\r']);
                        let head = text.trim_start();
                        (head.starts_with("```") || head.starts_with("~~~"))
                            .then(|| (i, text.to_string()))
                    })
                    .collect();
                let projection = view.doc.projection();
                // 各光标所在逻辑行：用于判定块级 widget 是否处于编辑态（SPEC §11.5）
                let caret_lines: Vec<usize> = caret_sources
                    .iter()
                    .map(|&s| projection.line_of_source(s))
                    .collect();
                // 块级 widget 装饰：光标在其内=编辑态（不画装饰），否则=渲染态（画装饰）
                let widgets: Vec<WidgetDecor> = projection
                    .block_widgets()
                    .iter()
                    .map(|w| {
                        let editing = caret_lines.iter().any(|&l| w.line_range.contains(&l));
                        let indent = projection
                            .lines
                            .get(w.line_range.start)
                            .map_or(indent_base, |l| block_indent(l, indent_base));
                        // 语言标签：用等宽字体在 prepaint 里 shape 好，paint 直接画（代码块/图表共用）
                        let shape_label = |lang: &str| {
                            let run = gpui::TextRun {
                                len: lang.len(),
                                font: mono_font.clone(),
                                color: palette.muted,
                                background_color: None,
                                underline: None,
                                strikethrough: None,
                            };
                            Box::new(window.text_system().shape_line(
                                lang.to_string().into(),
                                mono_size,
                                &[run],
                                None,
                            ))
                        };
                        let kind = match &w.kind {
                            BlockWidgetKind::Code { lang, .. } => WidgetDecorKind::Code {
                                label: lang.as_deref().map(shape_label),
                            },
                            // 图表块（mermaid 等）：同样画语言标签，便于用户知道该块的语言
                            BlockWidgetKind::Chart { lang } => WidgetDecorKind::Chart {
                                label: Some(shape_label(lang)),
                            },
                            BlockWidgetKind::Table { .. } => WidgetDecorKind::Table,
                            BlockWidgetKind::Rule => WidgetDecorKind::Rule,
                            BlockWidgetKind::Math => WidgetDecorKind::Math,
                        };
                        WidgetDecor {
                            kind,
                            line_range: w.line_range.clone(),
                            indent,
                            editing,
                        }
                    })
                    .collect();
                // 选区高亮：用核心的文档级映射把每个选区的源区间分解成"每行显示列区间"，
                // 再按行归并（多光标可能同行重叠）。这样 paint 阶段无需再做源→显示换算。
                let mut sel_by_line: Vec<Option<Range<usize>>> = vec![None; projection.lines.len()];
                for sel in &selection {
                    for ldr in
                        projection.selection_display_ranges(Selection::new(sel.start, sel.end))
                    {
                        match &mut sel_by_line[ldr.line] {
                            Some(existing) => {
                                existing.start = existing.start.min(ldr.cols.start);
                                existing.end = existing.end.max(ldr.cols.end);
                            }
                            none => *none = Some(ldr.cols),
                        }
                    }
                }
                // 代码块语法高亮：把连续代码行聚成一段，按语言一次性着色（SPEC §11.5、§12.2）。
                // 浅色主题用浅色高亮主题，深色主题用深色主题。
                let theme_name = if palette.background.l < 0.5 {
                    "base16-ocean.dark"
                } else {
                    "InspiredGitHub"
                };
                let highlighter = view
                    .highlighter
                    .get_or_insert_with(|| Highlighter::with_theme(theme_name));
                let mut code_hl: Vec<Option<Vec<ColoredRun>>> = vec![None; projection.lines.len()];
                let mut ci = 0usize;
                while ci < projection.lines.len() {
                    if !projection.lines[ci].block.code_block {
                        ci += 1;
                        continue;
                    }
                    let start = ci;
                    let lang = projection.lines[ci].block.code_lang.clone();
                    let mut code = String::new();
                    while ci < projection.lines.len() && projection.lines[ci].block.code_block {
                        if ci > start {
                            code.push('\n');
                        }
                        code.push_str(&projection.lines[ci].display_text());
                        ci += 1;
                    }
                    let highlighted = highlighter.highlight(&code, lang.as_deref());
                    for (k, spans) in highlighted.into_iter().enumerate() {
                        if let Some(slot) = code_hl.get_mut(start + k) {
                            *slot = Some(spans);
                        }
                    }
                }
                let mut v: Vec<BlockGeom> = Vec::with_capacity(projection.lines.len());
                let mut row_starts: Vec<usize> = Vec::with_capacity(projection.lines.len() + 1);
                // 每个全局视觉行的顶边（内容坐标）：标题/代码行高放大后不能再用"行号 × 统一行高"
                let mut row_tops: Vec<Pixels> = Vec::with_capacity(projection.lines.len() + 1);
                let mut row_acc = 0usize;
                // 内容坐标 y 累计（逐块行高可能不同）
                let mut top_acc = Pixels::ZERO;
                // 光标所在视觉行（内容坐标；供 paint 做滚动跟随，见 DocView::caret_scroll_key）
                let mut caret_row: Option<(Pixels, Pixels)> = None;
                // 任务复选框候选：`(源行区间, 缩进, 块顶, 行高)`；源文本在 projection 借用期读不了，
                // 先记几何，update 返回后再解析 `[ ]`/`[x]` 的源字节区间。
                let mut task_cand: Vec<(Range<usize>, Pixels, Pixels, Pixels)> = Vec::new();
                for (idx, line) in projection.lines.iter().enumerate() {
                    // 光标在可展开组内：把标记还原成可见原文；其它行仍按投影
                    // 隐藏标记（光标移开后自动复原）。
                    let revealed_line = if idx == caret_line {
                        caret_reveal
                            .as_ref()
                            .and_then(|r| reveal_inline(line, &caret_line_src, r))
                    } else {
                        None
                    };
                    let is_revealed = revealed_line.is_some();
                    let line = revealed_line.as_ref().unwrap_or(line);
                    // 围栏行：光标在代码块内时显示源码（含 ```lang），并做 1:1 字节映射
                    let fence_src: Option<SharedString> = fence_texts
                        .iter()
                        .find(|(i, _)| *i == idx)
                        .map(|(_, src)| src.clone().into());
                    // 展开态显示文本 = 整行源码（含标记），光标列按行内字节直接换算
                    let caret_col = if let Some(src) = &fence_src {
                        if caret_pos.line == idx {
                            let rel = caret.saturating_sub(line.source_range.start).min(src.len());
                            Some(src.get(..rel).map_or(0, |t| t.chars().count()))
                        } else {
                            None
                        }
                    } else if is_revealed {
                        // 展开态仍有其它隐藏标记（同行其它链接/强调），按展开后的
                        // OffsetMap 换算，不能假设显示字节 == 源字节
                        Some(
                            line.offset_map
                                .source_to_display(caret.saturating_sub(line.source_range.start))
                                .get(),
                        )
                    } else if caret_pos.line == idx {
                        Some(caret_pos.col)
                    } else {
                        None
                    };
                    let (text, runs, oblique_spans) = if let Some(src) = &fence_src {
                        // 围栏源码：等宽 + 弱化色，不做语法高亮/斜体合成
                        let run = gpui::TextRun {
                            len: src.len(),
                            font: mono_font.clone(),
                            color: palette.muted,
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        };
                        (src.clone(), vec![run], Vec::new())
                    } else {
                        build_line_runs(
                            line,
                            &base_font,
                            &mono_font,
                            &palette,
                            code_hl[idx].as_deref(),
                        )
                    };
                    // 前缀字节数 = 形状文本长度 - 投影显示文本长度（引用 "> " 等投影外文本）
                    // 围栏源码是整行原文，没有额外前缀
                    let prefix_bytes = if fence_src.is_some() {
                        0
                    } else {
                        text.len().saturating_sub(line.display_text().len())
                    };
                    // 逐块字号/行高：标题按级别放大，行高同步放大，避免字形压到相邻行
                    let font_size = block_font_size(&line.block, base_size, mono_size);
                    let line_px = block_line_px(&line.block, font_size);
                    let indent = block_indent(line, indent_base);
                    // 可用宽度 = 元素宽度 - 左缩进（含 BASE 左内边距）- 右侧 BASE 内边距
                    let mut wrap_width = (content_width_total - indent - BASE).max(px(48.));
                    if line.block.code_block {
                        wrap_width += px(24.);
                    }
                    // 逐 run 字体（粗/斜/等宽族）由 build_line_runs 决定，**不再统一覆盖**：
                    // 覆盖会把代码块的等宽族、标题的粗体都抹平，看起来就是"md 没渲染"。
                    let shaped = shape_wrapped_line(window, text, font_size, &runs, wrap_width);
                    let rows = shaped.wrap_boundaries().len() + 1;

                    // 光标行（内容坐标）：position_for_index 的 y = 视觉行号 × 行高（行顶）
                    if let Some(col) = caret_col {
                        let cbyte = if let Some(src) = &fence_src {
                            char_col_to_byte(src, col)
                        } else {
                            prefix_bytes + line.display_byte_of_col(col)
                        };
                        if let Some(p) = shaped.position_for_index(cbyte, line_px) {
                            caret_row = Some((top_acc + p.y, line_px));
                        }
                    }
                    // 列表符号：任务项改画复选框（见 paint），其余 shape 成文本右对齐进标记区
                    let marker = if line.block.bullet.is_some() && line.block.task.is_none() {
                        let sym = match line.block.bullet.as_ref() {
                            Some(b) if b.ordered => format!("{}.", b.number),
                            _ => "•".to_string(),
                        };
                        let run = gpui::TextRun {
                            len: sym.len(),
                            font: base_font.clone(),
                            color: palette.muted,
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        };
                        Some(Box::new(window.text_system().shape_line(
                            sym.into(),
                            font_size,
                            &[run],
                            None,
                        )))
                    } else {
                        None
                    };
                    // 已完成任务的勾（空框由 paint 画矩形，勾单独 shape 居中放进框内）
                    let check = if line.block.task == Some(true) {
                        let run = gpui::TextRun {
                            len: "✓".len(),
                            font: base_font.clone(),
                            color: palette.foreground,
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        };
                        Some(Box::new(window.text_system().shape_line(
                            "✓".into(),
                            font_size,
                            &[run],
                            None,
                        )))
                    } else {
                        None
                    };
                    if line.block.task.is_some() {
                        task_cand.push((line.source_range.clone(), indent, top_acc, line_px));
                    }
                    // 行内图片占位：`![alt](url)` 整段被投影隐藏，画个小标签补上可见性
                    let mut image_chips = Vec::new();
                    for w in &line.widgets {
                        // 展开态显示文本与源逐字节对齐；投影态用投影给的显示列
                        let byte = prefix_bytes
                            + if is_revealed {
                                let rel =
                                    w.source_range.start.saturating_sub(line.source_range.start);
                                line.display_byte_of_col(
                                    line.offset_map.source_to_display(rel).get(),
                                )
                            } else {
                                line.display_byte_of_col(w.display_col)
                            };
                        let Some(p) = shaped.position_for_index(byte, line_px) else {
                            continue;
                        };
                        let chip_text = w.kind.placeholder();
                        let run = gpui::TextRun {
                            len: chip_text.len(),
                            font: base_font.clone(),
                            color: palette.muted,
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        };
                        let chip = window.text_system().shape_line(
                            chip_text.into(),
                            font_size,
                            &[run],
                            None,
                        );
                        image_chips.push((
                            point(
                                bounds.origin.x + indent + p.x,
                                bounds.origin.y + top_acc + p.y,
                            ),
                            Box::new(chip),
                        ));
                    }

                    row_starts.push(row_acc);
                    row_tops.push(top_acc);
                    row_acc += rows;
                    // 块内各行同高：内容 y 累计 = 行高 × 视觉行数
                    top_acc += line_px * rows as f32;
                    // 选区显示列：展开态显示文本变长（URL/标记可见），按源区间在本行内重算
                    let selection_cols = if is_revealed {
                        let (start, end) = (line.source_range.start, line.source_range.end);
                        let mut agg: Option<Range<usize>> = None;
                        for sel in &selection {
                            let s = sel.start.clamp(start, end);
                            let e = sel.end.clamp(start, end);
                            if s >= e {
                                continue;
                            }
                            let cs = line.offset_map.source_to_display(s - start).get();
                            let ce = line.offset_map.source_to_display(e - start).get();
                            match &mut agg {
                                Some(a) => {
                                    a.start = a.start.min(cs);
                                    a.end = a.end.max(ce);
                                }
                                None => agg = Some(cs..ce),
                            }
                        }
                        agg
                    } else {
                        sel_by_line[idx].clone()
                    };
                    v.push(BlockGeom {
                        proj: line.clone(),
                        shaped: Arc::new(shaped),
                        rows,
                        prefix_bytes,
                        indent,
                        revealed: is_revealed,
                        caret_col,
                        selection: selection_cols,
                        line_height: line_px,
                        marker,
                        check,
                        image_chips,
                        oblique_spans,
                        font_size,
                        fence_src,
                    });
                }
                row_starts.push(row_acc);
                row_tops.push(top_acc);
                (v, widgets, row_starts, row_tops, caret_row, task_cand)
            });

        // 滚动视口：滚动容器（`#editor` 的 `track_scroll`）是本元素的祖先，
        // 其布局先于本元素完成，此刻 `ScrollHandle::bounds()` 即为滚动视口。
        let viewport = self.view.read(cx).scroll.bounds();

        // 任务复选框的源字节区间：`[ ]`/`[x]` 埋在源行（`- [ ] 文本`）里，
        // 等 update 返回（projection 已释放）再读 doc，并算出窗口坐标矩形。
        let mut task_boxes = Vec::new();
        for (range, indent, top, line_px) in task_cand {
            let Ok(src) = self.view.read(cx).doc.try_slice(range.clone()) else {
                continue;
            };
            let Some(brel) = src.find('[') else {
                continue;
            };
            let Some(erel) = src[brel..].find(']').map(|e| brel + e) else {
                continue;
            };
            // 只认任务复选位（`[ ]`/`[x]`/`[X]`），避免把行内 `[链接]` 误当复选框
            if !matches!(&src[brel + 1..erel], " " | "x" | "X") {
                continue;
            }
            task_boxes.push((
                task_box_rect(bounds.origin.x + indent, bounds.origin.y + top, line_px),
                range.start + brel..range.start + erel + 1,
            ));
        }

        DocLayout {
            blocks,
            widgets,
            origin: bounds.origin,
            line_height: LINE_HEIGHT,
            row_starts,
            row_tops,
            caret_row,
            task_boxes,
            viewport,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        element_bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let layout = prepaint.clone();
        let palette = self.view.read(cx).palette;

        // 回存几何供鼠标命中测试（`DocView::hit_source`），同时做两件事：
        // ① 内容高度回填（request_layout 用它上报可滚动高度，变化时补一帧收敛）；
        // ② 光标滚动跟随：仅当光标行几何变化时介入，避免与手动滚动手动打架。
        let layout_for_view = layout.clone();
        let total_height = layout.total_height();
        let caret_target = layout.caret_row;
        let view = self.view.clone();
        window.defer(cx, move |_, cx| {
            view.update(cx, |v, cx| {
                v.layout = Some(layout_for_view);
                if v.content_height != total_height {
                    v.content_height = total_height;
                    cx.notify(); // 高度变了：重排滚动区（内容超视口才会有滚动量）
                }
                if let Some((top, row_h)) = caret_target
                    && v.caret_scroll_key != Some((top, row_h))
                {
                    let sc = v.scroll.clone();
                    let viewport_h = sc.bounds().size.height;
                    if viewport_h > Pixels::ZERO {
                        const MARGIN: Pixels = px(8.);
                        let cur = sc.offset(); // y ∈ [-max_offset, 0]
                        // 行完全落在上下留白内才不动；否则把它拉回留白内（最少滚动）
                        let want = if top + cur.y > viewport_h - MARGIN - row_h {
                            -(top + row_h + MARGIN - viewport_h)
                        } else if top + cur.y < MARGIN {
                            -(top - MARGIN)
                        } else {
                            cur.y
                        };
                        let want = want.clamp(-sc.max_offset().y, Pixels::ZERO);
                        if want != cur.y {
                            sc.set_offset(point(cur.x, want));
                            // set_offset 只改句柄、不触发重绘；不 notify 的话画面仍停在
                            // 旧偏移，表现为"方向键按了但视口不跟随"
                            cx.notify();
                        }
                        v.caret_scroll_key = Some((top, row_h));
                    }
                }
            });
        });

        // 先画块级 widget 装饰（面板/网格/分割线），再画文本，保证文字在上层。
        // 渲染态画装饰；编辑态（光标已在块内）不画，露出可编辑内容（SPEC §11.5）。
        // 纵向范围按 `row_tops`（逐行顶边表）取，软换行/可变行高下都正确。
        if !layout.widgets.is_empty() {
            let content_right = element_bounds.origin.x + element_bounds.size.width - BASE;
            for w in &layout.widgets {
                let top = layout.origin.y + layout.row_top(layout.row_starts[w.line_range.start]);
                let bottom = layout.origin.y + layout.row_top(layout.row_starts[w.line_range.end]);
                let left = layout.origin.x + w.indent - px(8.);
                let right = content_right.max(left + px(24.)) + px(16.);
                let rect = bounds(point(left, top), size(right - left, bottom - top));
                match &w.kind {
                    // 分割线：在行垂直中线画一条细线（取代旧的 "─" 文本占位）
                    WidgetDecorKind::Rule => {
                        let y = top + (bottom - top) / 2.;
                        window.paint_quad(fill(
                            bounds(point(left, y), size(right - left, px(1.))),
                            palette.border,
                        ));
                    }
                    // 代码块：面板浅底 + 渲染态描边与右上角语言标签
                    WidgetDecorKind::Code { label } => {
                        window.paint_quad(fill(rect, palette.code_block_bg));
                        if !w.editing {
                            paint_rect_border(window, rect, palette.border, px(1.));
                            if let Some(label) = label {
                                paint_corner_label(label, right - px(8.), top, window, cx);
                            }
                        }
                    }
                    // 图表块：弱底 + 外框 + 右上角语言标签（真渲染留渲染层，R4）
                    WidgetDecorKind::Chart { label } if !w.editing => {
                        window.paint_quad(fill(rect, palette.subtle));
                        paint_rect_border(window, rect, palette.border, px(1.));
                        if let Some(label) = label {
                            paint_corner_label(label, right, top, window, cx);
                        }
                    }
                    // 显示公式：整体描边（公式体文字自身为斜体主题色）
                    WidgetDecorKind::Math if !w.editing => {
                        paint_rect_border(window, rect, palette.border, px(1.));
                    }
                    // 表格：弱底填充 + 外框 + 每逻辑行之间的水平分隔线
                    WidgetDecorKind::Table if !w.editing => {
                        window.paint_quad(fill(rect, palette.subtle));
                        paint_rect_border(window, rect, palette.border, px(1.));
                        for line in (w.line_range.start + 1)..w.line_range.end {
                            let y = layout.origin.y + layout.row_top(layout.row_starts[line]);
                            window.paint_quad(fill(
                                bounds(point(left, y), size(right - left, px(1.))),
                                palette.border,
                            ));
                        }
                    }
                    _ => {}
                }
            }
        }

        // 逐块绘制：选区高亮底 → 列表符号/复选框/图片占位 → 文本 → 光标竖线
        for (idx, block) in layout.blocks.iter().enumerate() {
            let first_row = layout.row_starts[idx];
            // 块首视觉行顶边（窗口坐标）；块内各行同高 = block.line_height
            let row_top = layout.origin.y + layout.row_top(first_row);
            let line_px = block.line_height;
            let line_x = layout.origin.x + block.indent;

            // 引用块：所有层级都画左竖线和背景；仅最外层加上下内边距，
            // 避免嵌套引用之间产生多余的间隔分隔。
            if block.proj.block.quote_depth > 0 {
                let pad_x = px(4.);
                let is_outer = block.proj.block.quote_depth == 1;
                let pad_y = if is_outer { px(2.) } else { px(0.) };
                let left = layout.origin.x + pad_x;
                let right = element_bounds.origin.x + element_bounds.size.width - pad_x;
                if right > left {
                    let height = line_px * block.rows as f32;
                    let band = bounds(
                        point(left, row_top - pad_y),
                        size(right - left, height + pad_y * 2.),
                    );
                    window.paint_quad(fill(band, palette.quote_bg));
                    window.paint_quad(fill(
                        bounds(
                            point(left, row_top - pad_y),
                            size(px(3.), height + pad_y * 2.),
                        ),
                        palette.border,
                    ));
                }
            }

            // 选区高亮：`selection` 是本块的显示列区间；按视觉行拆成多段矩形
            if let Some(cols) = &block.selection {
                let b0 = block.byte_of_col(cols.start);
                let b1 = block.byte_of_col(cols.end);
                if b1 > b0 {
                    let rb = row_byte_bounds(&block.shaped);
                    for r in 0..block.rows {
                        let (rs, re) = (rb[r], rb[r + 1]);
                        let (s, e) = (b0.max(rs), b1.min(re));
                        if e <= s {
                            continue;
                        }
                        // 视觉行内 x 相对行首；行首本身 x=0（`position_for_index` 在边界字节
                        // 会落到上一行，故行首直接取 0）
                        let x0 = if s == rs {
                            Pixels::ZERO
                        } else {
                            block
                                .shaped
                                .position_for_index(s, line_px)
                                .map_or(Pixels::ZERO, |p| p.x)
                        };
                        let x1 = block
                            .shaped
                            .position_for_index(e, line_px)
                            .map_or(Pixels::ZERO, |p| p.x);
                        let width = if x1 > x0 { x1 - x0 } else { Pixels::ZERO };
                        let y = row_top + line_px * r as f32;
                        window.paint_quad(fill(
                            bounds(point(line_x + x0, y), size(width, line_px)),
                            palette.selection,
                        ));
                    }
                }
            }

            // 列表符号：右对齐到标记区右缘（`block_indent` 为列表预留的 1.5*BASE），纵向随行居中
            if let Some(marker) = &block.marker {
                let mx = line_x - px(4.) - marker.width();
                let _ = marker.paint(
                    point(mx, row_top),
                    line_px,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
            // 任务复选框：空框描边；已完成再把预 shape 的勾居中放进去
            if block.proj.block.task.is_some() {
                let box_rect = task_box_rect(line_x, row_top, line_px);
                paint_rect_border(window, box_rect, palette.muted, px(1.));
                if let Some(check) = &block.check {
                    let check_x = box_rect.origin.x + (box_rect.size.width - check.width()) / 2.;
                    let _ = check.paint(
                        point(check_x, row_top),
                        line_px,
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                }
            }
            // 行内图片占位标签（弱底小方块）
            for (chip_pos, chip) in &block.image_chips {
                let rect = bounds(
                    point(chip_pos.x - px(4.), chip_pos.y),
                    size(chip.width() + px(8.), line_px),
                );
                window.paint_quad(fill(rect, palette.subtle));
                let _ = chip.paint(*chip_pos, line_px, TextAlign::Left, None, window, cx);
            }

            // 文本本体（`WrappedLine::paint` 内部按换行边界逐视觉行绘制）
            let mut text_x = line_x;
            if block.proj.block.code_block {
                text_x += px(12.);
            }
            let _ = block.shaped.paint(
                point(text_x, row_top),
                line_px,
                TextAlign::Left,
                None,
                window,
                cx,
            );
            // 合成斜体：在文本绘制后填充倾斜字形。
            // 这里现取轮廓（而非布局阶段缓存）：需要视觉行基线，而基线由 shaped 的
            // ascent/descent 决定；放在 train 里算几何会与 paint 的坐标换算重复。
            if !block.oblique_spans.is_empty() {
                let line_text = block.shaped.text.as_str();
                for span in &block.oblique_spans {
                    let mut byte = span.range.start;
                    while byte < span.range.end {
                        let Some(ch) = line_text[byte..].chars().next() else {
                            break;
                        };
                        let ch_len = ch.len_utf8();
                        if byte + ch_len > span.range.end {
                            break;
                        }
                        // 形状文本内的字节偏移 = 前缀 + 行内偏移
                        let idx = block.prefix_bytes + byte;
                        if let Some(p) = block.shaped.position_for_index(idx, line_px) {
                            // 基线：视觉行顶 + 行内垂直居中偏移 + ascent
                            let baseline_y = row_top
                                + p.y
                                + (line_px - block.shaped.ascent() - block.shaped.descent()) / 2.
                                + block.shaped.ascent();
                            if let Some(path) = oblique::glyph_path(
                                ch,
                                block.font_size.into(),
                                text_x + p.x,
                                baseline_y,
                            ) {
                                window.paint_path(path, span.color);
                            }
                        }
                        byte += ch_len;
                    }
                }
            }

            // 光标竖线：仅在光标所在块绘制（用 prepaint 预计算的 caret_col 显示列）
            if let Some(col) = block.caret_col
                && let Some(p) = block
                    .shaped
                    .position_for_index(block.byte_of_col(col), line_px)
            {
                // p.y = 光标所在视觉行在块内的顶边偏移（软换行下不再画错行）
                let caret_rect = bounds(
                    point(line_x + p.x, row_top + p.y + px(2.)),
                    size(px(2.), line_px - px(4.)),
                );
                // 光标用正文前景色（对比度最高），而非主题 accent（可能偏浅看不清）
                window.paint_quad(fill(caret_rect, palette.foreground));
            }
        }

        // 挂载 IME 输入处理器（需在 paint 阶段调用，传入当前元素 bounds）
        let view = self.view.clone();
        window.handle_input(
            &view.read(cx).focus,
            ElementInputHandler::new(element_bounds, view),
            cx,
        );
    }
}

impl IntoElement for DocElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

fn main() {
    // 可选参数：要打开的 .md 路径；缺省时渲染内置示例
    let path = std::env::args().nth(1).map(PathBuf::from);
    let title = match &path {
        Some(p) => format!(
            "码札 — {}",
            p.file_name().unwrap_or_default().to_string_lossy()
        ),
        None => "码札 — 内置示例（只读）".to_string(),
    };

    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            ..Default::default()
        }),
        ..Default::default()
    };

    application().run(move |cx| {
        gpui_kit::init(cx);
        let path = path.clone();
        gpui_kit::open_window(options, cx, move |window, cx| {
            let view = cx.new(|cx| DocView::new(path.as_ref(), cx));
            // 打开即聚焦，键盘事件直接可用
            let focus = view.read(cx).focus.clone();
            window.focus(&focus, cx);
            view
        })
        .expect("打开窗口失败");
    });
}

#[cfg(test)]
mod tests {
    // 只引入被测项：`use super::*` 会把 gpui 的 `test` 宏带进来，导致 `#[test]` 递归展开
    use super::{
        DisplayPos, Document, RunStyle, char_col_to_byte, inline_links, inline_reveals, oblique,
        reveal_inline,
    };

    /// 中文斜体必须一路传到渲染层：投影要产出 `RunStyle::Emphasis`，
    /// 且 `to_gpui_runs` 要把 CJK 片段标成合成斜体（透明占位 + ObliqueSpan）。
    #[test]
    fn cjk_emphasis_becomes_oblique_span() {
        let text = "前 *中文斜体* 后";
        let mut doc = Document::new(text);
        let projection = doc.projection();
        let line = &projection.lines[0];
        let emphasis = line
            .runs
            .iter()
            .find(|r| r.style == RunStyle::Emphasis)
            .expect("投影应产出 Emphasis 片段");
        assert_eq!(emphasis.text, "中文斜体");

        let base = gpui_kit::Font {
            family: ".SystemUIFont".into(),
            ..Default::default()
        };
        let mono = base.clone();
        let palette = super::Palette::default_for_test();
        let (out_text, runs, spans) = super::to_gpui_runs(emphasis, &base, &mono, &palette);
        assert_eq!(out_text, "中文斜体");
        // 整段都是 CJK：应被拆成透明占位 run + 对应的合成斜体区间
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].range, 0..out_text.len());
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].len, out_text.len());
        assert_eq!(runs[0].color.a, 0.0, "占位字形必须透明");
    }

    /// CoreText 侧能真的取到中文轮廓（否则自绘阶段会静默跳过，斜体仍不生效）。
    #[cfg(target_os = "macos")]
    #[test]
    fn cjk_glyph_path_is_available() {
        assert!(
            !oblique::primary_covers(".SystemUIFont", '斜'),
            "系统 UI 字体不应覆盖汉字，否则不会走合成斜体"
        );
        let path = oblique::glyph_path('斜', 32.0, gpui_kit::px(0.), gpui_kit::px(100.));
        assert!(path.is_some(), "应能从 CJK 回退字体取到字形轮廓");
    }

    /// 完整链接应被识别为一组：区间覆盖 `[`..`)`，目标去掉两侧空白。
    #[test]
    fn inline_links_groups_full_link() {
        let src = "前 [文档]( https://a.com ) 后";
        let links = inline_links(src);
        assert_eq!(links.len(), 1);
        let link = &links[0];
        assert_eq!(&src[link.range.clone()], "[文档]( https://a.com )");
        assert_eq!(&src[link.dest.clone()], "https://a.com");
        // 4 段标记：`[`、`](`、URL、`)`
        assert_eq!(link.markers.len(), 4);
    }

    /// 未闭合 / 空目标不算链接（避免光标经过时展开出空壳）。
    #[test]
    fn inline_links_rejects_incomplete_and_empty() {
        assert!(inline_links("[文档](https://a.com").is_empty());
        assert!(inline_links("[x]()").is_empty());
        assert!(inline_links("[x](   )").is_empty());
    }

    /// 同一行多个链接互不干扰，目标偏移各自正确。
    #[test]
    fn inline_links_multiple() {
        let src = "[一](a) [二](b)";
        let links = inline_links(src);
        assert_eq!(links.len(), 2);
        assert_eq!(&src[links[0].dest.clone()], "a");
        assert_eq!(&src[links[1].dest.clone()], "b");
    }

    /// 展开只还原当前链接的标记；同行强调标记仍隐藏，且偏移映射按展开后的显示文本成立。
    #[test]
    fn reveal_restores_only_active_link() {
        let text = "**粗** [文档](https://a.com) 尾巴";
        let mut doc = Document::new(text);
        let line = {
            let projection = doc.projection();
            assert_eq!(projection.lines.len(), 1);
            projection.lines[0].clone()
        };
        let line_src = doc.slice(line.source_range.clone()).into_owned();
        let reveal = inline_reveals(&line_src)
            .into_iter()
            .find(|r| r.link_dest.is_some())
            .expect("应识别到链接");
        let dest = reveal.link_dest.clone().unwrap();
        let revealed = reveal_inline(&line, &line_src, &reveal).expect("段落链接应可展开");

        // 投影态：强调/链接标记与 URL 都隐藏
        assert_eq!(line.display_text(), "粗 文档 尾巴");
        // 展开态：链接恢复为原文，强调标记仍隐藏
        assert_eq!(revealed.display_text(), "粗 [文档](https://a.com) 尾巴");

        // URL 逐字符与源偏移对齐（展开后 1 显示字符 = 1 源字节）
        for rel in dest {
            if !line_src.is_char_boundary(rel) {
                continue;
            }
            let col = revealed.offset_map.source_to_display(rel).get();
            assert_eq!(
                revealed.offset_map.display_to_source(DisplayPos(col)),
                rel,
                "源偏移 {rel} 的展开显示列 {col} 应可逆"
            );
        }
    }

    /// 代码块行整行是可见文本：不能再插入标记（否则 `[`/`](`/URL 会重复显示）。
    #[test]
    fn reveal_skips_code_block_line() {
        let text = "```\n[a](b)\n```\n";
        let mut doc = Document::new(text);
        let line = {
            let projection = doc.projection();
            projection.lines[1].clone()
        };
        let line_src = doc.slice(line.source_range.clone()).into_owned();
        let link = inline_reveals(&line_src)
            .into_iter()
            .find(|r| r.link_dest.is_some())
            .expect("代码文本里也能识别出链接组");
        assert!(
            reveal_inline(&line, &line_src, &link).is_none(),
            "代码块行不应展开"
        );
    }
    /// 围栏行 1:1 映射：显示列按字符计，必须落到 UTF-8 字符边界，
    /// 越界钳到行末（与 `BlockGeom::byte_of_col` 的约定一致）。
    #[test]
    fn fence_char_col_maps_to_char_boundary() {
        let src = "```中文 lang\n";
        assert_eq!(char_col_to_byte(src, 0), 0);
        assert_eq!(char_col_to_byte(src, 3), 3); // ``` 之后
        assert_eq!(char_col_to_byte(src, 4), 6); // 「中」占 3 字节
        assert_eq!(char_col_to_byte(src, 99), src.len());
    }
}
