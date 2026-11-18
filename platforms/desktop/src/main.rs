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

use base64::Engine as _;
use rfd::AsyncFileDialog;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::ops::Range;
use std::panic::Location;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::base::h_flex;
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::{ActiveTheme, Theme, ThemeMode};
use gpui_kit::prelude::*;
use gpui_kit::*;
use mazha_core::{
    Align, BlockStyle, BlockTarget, BlockWidgetKind, ColoredRun, DefaultIntentMapper, DisplayPos,
    DocPos, Document, Edit, Highlighter, InlineMark, Intent, IntentMapper, OffsetMap,
    ProjectedLine, Projection, RunStyle, Selection, TableLine, TableOp, TextRun as DocTextRun,
    WidgetKind, Workspace, cell_index_at, cell_step,
    commands::{set_block, toggle_inline},
    edit_table, find_all, replace_all_edit, table_cells,
};
use mazha_export::to_pdf;
use mazha_md::{FootnoteIndex, MediaElement, MediaKind, SpanKind, SpanRole, parse_inline};
#[allow(unused_imports, dead_code)]
use mazha_store::{DocPath, Entry, EntryKind, FileSystemProvider};
mod media;
mod oblique;
mod settings;

use settings::{FONT_STEP, Settings, ThemePreference, clamp_font_size, clamp_mono_font_size};

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
    /// 选中态浅灰底（标签页 / 文件树当前项），与蓝色文本选区区分。
    selected_bg: Hsla,
    /// 选区高亮底色（accent 的低透明度）。
    selection: Hsla,
}

/// 文件树拖拽负载（`on_drag` 的值，也是 `on_drop` / `can_drop` 收到的 `&dyn Any`）。
///
/// 只携带相对工作区根的路径与展示信息：落点据此算出目标目录，再交给 `DocView::move_entry`。
#[derive(Clone)]
struct TreeDrag {
    /// 被拖拽条目的相对工作区根路径（正斜杠分隔，与 `Entry::path` 一致）。
    rel: String,
    /// 条目名（拖拽预览展示）。
    name: String,
    /// 是否为目录；用于在 `can_drop` 中拒绝把目录拖进自身或其后代。
    is_dir: bool,
    /// 预览底色。
    bg: Hsla,
    /// 预览文字色。
    fg: Hsla,
    /// 预览边框色。
    border: Hsla,
}

/// 拖拽跟随预览：跟随光标的半透明小标签（由 `on_drag` 的 constructor 生成）。
struct TreeDragPreview {
    /// 展示文本（条目名）。
    label: String,
    /// 底色。
    bg: Hsla,
    /// 文字色。
    fg: Hsla,
    /// 边框色。
    border: Hsla,
}

impl Render for TreeDragPreview {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .text_sm()
            .bg(self.bg)
            .text_color(self.fg)
            .border_1()
            .border_color(self.border)
            .rounded(px(4.))
            .child(self.label.clone())
    }
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
            // 选中底色：中性浅灰（前景色低透明度），不抢正文也不与蓝色选区混淆
            selected_bg: Hsla {
                a: 0.12,
                ..theme.foreground
            },
            // 选区底色：常规编辑器风格（蓝底高亮）
            selection: Hsla {
                h: 215.0,
                s: 0.9,
                l: 0.55,
                a: 0.28,
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
            selected_bg: fg,
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
                color: None,
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
    // 展开态显示文本变化后，原表格单元格字节范围失效；表格行不走展开（见 prepaint），
    // 这里保守清空，避免按错误范围排版。
    out.table_cells = None;
    Some(out)
}

/// 独占行图片的渲染态数据：已解码图像 + 适配后的逻辑尺寸。
#[derive(Clone)]
struct ImagePreview {
    /// GPUI 图像缓存持有的解码结果（按路径去重，跨帧复用）。
    data: Arc<RenderImage>,
    /// 适配正文宽度后的显示尺寸（不放大）。
    size: Size<Pixels>,
}

/// 独占行音视频的渲染态几何：卡片矩形、控制条文字与进度条命中区。
#[derive(Clone)]
struct MediaPreview {
    /// 播放状态键（`kind:src`）。
    key: String,
    /// 媒体类型。
    kind: MediaKind,
    /// 媒体地址（首次点击时创建播放器）。
    src: String,
    /// 视频帧/音频卡片矩形（窗口坐标）。
    rect: Bounds<Pixels>,
    /// 底部进度条矩形（窗口坐标；点击按横向比例 seek）。
    progress: Bounds<Pixels>,
    /// 控制条文字（标题/播放态/时间），prepaint 阶段 shape 好。
    label: Box<ShapedLine>,
    /// 音频按钮右侧的“当前时间 / 总时长”；视频为 `None`。
    duration_label: Option<Box<ShapedLine>>,
    /// 是否正在等待首次加载或网络缓冲。
    loading: bool,
    /// 视频封面（未取到首帧时显示）。
    poster: Option<Arc<RenderImage>>,
    /// 当前播放位置与总时长（秒；用于进度条比例）。
    position: f64,
    duration: f64,
}

/// 媒体播放器及轮询任务（每个 `kind:src` 一个，首次点击时懒创建）。
struct MediaPlayback {
    /// `None` 表示创建失败，错误见 [`MediaPlayback::error`]。
    player: Option<media::MediaPlayer>,
    /// 媒体类型（决定轮询间隔）。
    kind: MediaKind,
    /// 播放中的取帧/进度轮询；暂停时 drop 即取消。
    task: Option<Task<()>>,
    /// 打开失败原因。
    error: Option<String>,
}

/// 根据脚注索引与点击落点计算跳转目标（D69）。
///
/// - 点击引用标签（`entry.references` 覆盖 `src`）→ 定义块起点；
/// - 点击定义块（`entry.definition` 覆盖 `src`）→ 首个引用标签起点；
/// - 两处都不匹配或没有对应目标 → `None`（交给普通点击放光标）。
///
/// 纯函数，便于在没有 GPUI 布局的单测里覆盖双向跳转。
fn footnote_jump(index: &FootnoteIndex, label: &str, src: usize) -> Option<usize> {
    let entry = index.get(label)?;
    if entry
        .references
        .iter()
        .any(|r| src >= r.start && src <= r.end)
    {
        return entry.definition.as_ref().map(|r| r.start);
    }
    if entry
        .definition
        .as_ref()
        .is_some_and(|r| src >= r.start && src <= r.end)
    {
        return entry.references.first().map(|r| r.start);
    }
    None
}

/// 递归收集文件树的行（`(深度, 条目)`），供侧栏按层级渲染。
///
/// 根目录的子项由调用方直接展开；子目录仅当 `expanded` 含其相对路径时继续下钻。
/// 目录一律显示（即使为空，便于在树里定位/新建），文件只保留 Markdown 文档。
fn collect_tree_rows(
    ws: &Workspace<FileSystemProvider>,
    dir: &DocPath,
    depth: usize,
    expanded: &HashSet<String>,
    rows: &mut Vec<(usize, Entry)>,
) {
    let Ok(entries) = ws.list(dir) else {
        return;
    };
    for entry in entries {
        match entry.kind {
            EntryKind::Dir => {
                let rel = entry.path.as_str().to_string();
                let is_expanded = expanded.contains(&rel);
                rows.push((depth, entry.clone()));
                if is_expanded {
                    collect_tree_rows(ws, &entry.path, depth + 1, expanded, rows);
                }
            }
            EntryKind::File => {
                if entry.path.is_markdown() {
                    rows.push((depth, entry));
                }
            }
        }
    }
}

/// 递归收集工作区内全部 Markdown 文档的相对路径（`/` 分隔，按目录名/文件名排序）。
///
/// 全工作区搜索复用文件树的遍历规则：目录在前、名称排序；不展开就能拿到全量，
/// 与用户当前的展开/折叠状态无关。目录读取失败直接跳过（权限/竞态），不让整次搜索失败。
fn collect_markdown_paths(
    ws: &Workspace<FileSystemProvider>,
    dir: &DocPath,
    out: &mut Vec<String>,
) {
    // 记录已访问目录的规范路径，防止符号链接成环导致无限递归。
    let root = ws.store().root().to_path_buf();
    let mut visited = HashSet::new();
    collect_markdown_paths_inner(ws, &root, dir, &mut visited, out);
}

/// [`collect_markdown_paths`] 的递归实现（`visited` 用规范路径去重）。
fn collect_markdown_paths_inner(
    ws: &Workspace<FileSystemProvider>,
    root: &Path,
    dir: &DocPath,
    visited: &mut HashSet<PathBuf>,
    out: &mut Vec<String>,
) {
    let abs_dir = if dir.is_root() {
        root.to_path_buf()
    } else {
        root.join(dir.as_str())
    };
    // 规范路径解析失败（权限/竞态）时退回原路径，仍可继续遍历。
    let key = abs_dir
        .canonicalize()
        .unwrap_or_else(|_| abs_dir.to_path_buf());
    if !visited.insert(key) {
        return;
    }
    let Ok(entries) = ws.list(dir) else {
        return;
    };
    for entry in entries {
        match entry.kind {
            EntryKind::Dir => {
                collect_markdown_paths_inner(ws, root, &entry.path, visited, out);
            }
            EntryKind::File if entry.path.is_markdown() => {
                out.push(entry.path.as_str().to_string());
            }
            EntryKind::File => {}
        }
    }
}

/// 在一段文件文本中查找查询词，按行产出命中（纯函数，可脱离文件系统单测）。
///
/// 逐行调用核心 `find_all`（大小写语义与文档内查找一致）；行号 1 基，
/// `range` 相对于行文本、`source` 相对于整段文本，供预览高亮与打开后选区使用。
fn search_text_lines(text: &str, query: &str, case_sensitive: bool) -> Vec<WorkspaceTextHit> {
    let mut out = Vec::new();
    if query.is_empty() {
        return out;
    }
    let mut offset = 0usize;
    for (idx, raw) in text.split_inclusive('\n').enumerate() {
        // 行文本去掉行尾换行（CRLF 同时去掉 `\r`），但源偏移仍按原始行长度推进。
        let line = raw.trim_end_matches(['\n', '\r']);
        for range in find_all(line, query, case_sensitive) {
            out.push(WorkspaceTextHit {
                line_no: idx + 1,
                line: line.to_string(),
                source: offset + range.start..offset + range.end,
                range,
            });
        }
        offset += raw.len();
    }
    out
}

/// 取 `s` 末尾最多 `n` 个字符（超出时加前导省略号）；按字符边界截断，避免切坏 UTF-8。
fn tail_chars(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n {
        return s.to_string();
    }
    let mut out = String::from("…");
    out.extend(&chars[chars.len() - n..]);
    out
}

/// 取 `s` 开头最多 `n` 个字符（超出时加尾随省略号）；按字符边界截断。
fn head_chars(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n {
        return s.to_string();
    }
    let mut out: String = chars[..n].iter().collect();
    out.push('…');
    out
}

/// 判断拖拽落点是否合法（供文件树 `can_drop` 与单测复用）。
///
/// 非法落点：拖到条目自身所在的同一位置（`from_rel == target_rel`），
/// 或把目录拖进自身及其后代（`target_rel` 以 `from_rel + "/"` 开头，会把自己搬进自己内部）。
fn drop_allowed(from_rel: &str, from_is_dir: bool, target_rel: &str) -> bool {
    if from_rel == target_rel {
        return false;
    }
    if from_is_dir && target_rel.starts_with(&format!("{from_rel}/")) {
        return false;
    }
    true
}

/// 把展开集合中的旧目录前缀整体迁移为新前缀（目录重命名/移动后保持展开态）。
///
/// 命中条件：等于 `old_rel` 本身，或以 `old_rel/` 开头；迁移时保留其后的子路径后缀。
fn remap_expanded(expanded: &mut HashSet<String>, old_rel: &str, new_rel: &str) {
    let old_prefix = format!("{old_rel}/");
    let moved: Vec<String> = expanded
        .iter()
        .filter(|d| d.as_str() == old_rel || d.starts_with(&old_prefix))
        .cloned()
        .collect();
    for d in moved {
        expanded.remove(&d);
        let suffix = d.strip_prefix(old_rel).unwrap_or("");
        expanded.insert(format!("{new_rel}{suffix}"));
    }
}

/// 为导入到 `target_dir` 的文件名找一个不冲突的目标名。
///
/// 已存在（磁盘上或本批次已占用 `taken`）时，在扩展名前追加 ` 2`、` 3`…；
/// 纯函数，不写文件，便于单测覆盖同名冲突与带多个点的文件名。
fn unique_import_path(target_dir: &Path, name: &OsStr, taken: &HashSet<PathBuf>) -> PathBuf {
    let candidate = target_dir.join(name);
    if !candidate.exists() && !taken.contains(&candidate) {
        return candidate;
    }
    let name_path = Path::new(name);
    let stem = name_path.file_stem().unwrap_or(name).to_string_lossy();
    let extension = name_path
        .extension()
        .map(|ext| format!(".{}", ext.to_string_lossy()))
        .unwrap_or_default();
    for i in 2..10_000 {
        let candidate = target_dir.join(format!("{stem} {i}{extension}"));
        if !candidate.exists() && !taken.contains(&candidate) {
            return candidate;
        }
    }
    // 极端情况下的兜底：仍返回唯一名字，交由后续复制失败路径报错。
    target_dir.join(format!("{} 10000", name.to_string_lossy()))
}

/// 为外部拖入的路径规划 `(来源, 目标)` 清单（纯函数，不执行复制）。
///
/// 已在目标目录的条目直接跳过；同批次同名条目按 [`unique_import_path`] 递增编号；
/// 目录不能复制进自身（目标落在源目录内部）时同样跳过，避免无限递归。
fn plan_imports(paths: &[PathBuf], target_dir: &Path) -> Vec<(PathBuf, PathBuf)> {
    let base = target_dir
        .canonicalize()
        .unwrap_or_else(|_| target_dir.to_path_buf());
    let mut taken: HashSet<PathBuf> = HashSet::new();
    let mut out = Vec::new();
    for src in paths {
        let Some(name) = src.file_name() else {
            continue;
        };
        // 规范路径用于「已在目标目录」与「目录复制进自身」两种判断。
        let src_canon = src.canonicalize().unwrap_or_else(|_| src.clone());
        if src
            .parent()
            .map(|parent| {
                parent
                    .canonicalize()
                    .unwrap_or_else(|_| parent.to_path_buf())
            })
            .is_some_and(|parent| parent == base)
        {
            continue;
        }
        let dst = unique_import_path(target_dir, name, &taken);
        let dst_canon = base.join(dst.file_name().unwrap_or(name));
        if src.is_dir() && dst_canon.starts_with(&src_canon) {
            continue;
        }
        taken.insert(dst.clone());
        out.push((src.clone(), dst));
    }
    out
}

/// 递归复制外部拖入的条目（文件直接 `copy`，目录逐项复制）。
///
/// 符号链接一律跳过：既不跟随目标目录造成环，也不把链接目标内容悄悄复制进工作区。
fn copy_entry(src: &Path, dst: &Path) -> std::io::Result<()> {
    let metadata = std::fs::symlink_metadata(src)?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_dir() {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_entry(&entry.path(), &dst.join(entry.file_name()))?;
        }
    } else {
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(src, dst)?;
    }
    Ok(())
}

/// 是否为可直接作为 Markdown 文档打开的扩展名（与 `DocPath::is_markdown` 一致）。
fn is_markdown_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown")
        })
}

/// 把拖入的非 Markdown 文件转成可插入文档的 Markdown 链接。
///
/// `base_dir` 存在且路径位于其下时写相对路径（便于工作区整体移动），否则写绝对路径；
/// 图片沿用 `![...]` 语法，其余文件用普通链接。
fn dropped_link_markdown(path: &Path, base_dir: Option<&Path>) -> String {
    let image = is_image_path(path);
    format_markdown_link(path, base_dir, image)
}

/// `markdown_image_path` 与拖入链接共用的格式化实现。
fn format_markdown_link(path: &Path, base_dir: Option<&Path>, image: bool) -> String {
    let alt = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(if image { "图片" } else { "文件" })
        .replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]");
    // 相对路径只在能安全剥离前缀时使用；尖括号保留路径中的空格。
    let display = base_dir
        .and_then(|base| path.strip_prefix(base).ok())
        .filter(|rel| !rel.as_os_str().is_empty())
        .unwrap_or(path);
    // `<`/`>` 会破坏尖括号目标边界；百分号形式会由现有路径加载器还原。
    let dest = display
        .to_string_lossy()
        .replace('%', "%25")
        .replace('<', "%3C")
        .replace('>', "%3E")
        .replace('\n', "%0A")
        .replace('\r', "%0D");
    let bang = if image { "!" } else { "" };
    format!("{bang}[{alt}](<{dest}>)")
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
    /// 非独占行的 `![alt](url)` 整段被投影隐藏，若不画占位屏幕上就是空白，
    /// 故补一个可见标签；独占行在加载失败/未加载时也用它兜底。
    image_chips: Vec<(Point<Pixels>, Box<ShapedLine>)>,
    /// 需自绘合成倾斜的中文片段（相对行文本字节区间）。
    ///
    /// 这些片段在 gpui 侧是透明占位（保留排版/光标/选区），字形轮廓在 paint 阶段
    /// 按视觉行基线现取现画，避免在布局阶段重复计算几何。
    oblique_spans: Vec<oblique::ObliqueSpan>,
    /// 本块字号（自绘斜体取轮廓用）。
    font_size: Pixels,
    /// 光标进入隐藏整行时直接显示的源码（围栏行 / 独占行图片）。
    ///
    /// 这些行投影后显示文本为空（整行都是隐藏标记），光标落上去就"看不见"。
    /// 编辑时把源码直接画出来，并令显示列与源字节 1:1 映射。
    raw_src: Option<SharedString>,
    /// 图片独占行在渲染态下的真实图片（已加载并适配宽度）。
    image: Option<ImagePreview>,
    /// 音视频独占行在渲染态下的卡片/播放控件。
    media: Option<MediaPreview>,
    /// 表格行的网格几何（`None` = 非表格行，或该表放不下/含合成斜体而回退流式排版）。
    table: Option<TableRowGeom>,
}

/// 字符列 → UTF-8 字节偏移（越界钳到末尾）。
fn char_col_to_byte(s: &str, col: usize) -> usize {
    s.char_indices().nth(col).map_or(s.len(), |(i, _)| i)
}

impl BlockGeom {
    /// 显示列 → 形状化文本内的字节偏移（含前缀）。
    fn byte_of_col(&self, col: usize) -> usize {
        if let Some(src) = &self.raw_src {
            return char_col_to_byte(src, col);
        }
        self.prefix_bytes + self.proj.display_byte_of_col(col)
    }

    /// 形状化文本内的字节偏移 → 显示列（扣除前缀，越界钳到行末）。
    fn col_of_byte(&self, byte: usize) -> usize {
        if let Some(src) = &self.raw_src {
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
        if self.raw_src.is_some() {
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
        if self.raw_src.is_some() {
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

/// 表格单元格左右内边距（源码里的空格不再作为唯一留白，网格模式统一加内边距）。
const TABLE_CELL_PAD: Pixels = px(10.);
/// 超宽表格压缩列宽时的单元格最小内容宽度（不含左右 padding）。
const TABLE_CELL_MIN_INNER: Pixels = px(48.);
/// 拖动列边界时单列最小宽度（含内边距）。
const TABLE_DRAG_MIN_WIDTH: Pixels = px(48.);
/// 表格边框拖动命中半径。
const TABLE_RESIZE_HIT: Pixels = px(3.);

/// 一张网格化表格的公共几何（所有行共享列宽与边界）。
///
/// `boundaries[0] = 0`，最后一项 = 表格总宽；列 i 的范围是
/// `boundaries[i]..boundaries[i+1]`（相对表格左缘）。
#[derive(Clone)]
struct TableGridGeom {
    /// 表格源起点（会话列宽状态键）。
    source_start: usize,
    /// 每列左/右边界（含首 0 与末尾总宽）。
    boundaries: Vec<Pixels>,
    /// 表格总宽。
    width: Pixels,
}

#[derive(Clone)]
struct TableResizeHandle {
    rect: Bounds<Pixels>,
    /// prepaint 阶段注册，paint 阶段仅用于设置悬浮光标。
    hitbox: Option<Hitbox>,
    table: usize,
    kind: TableResizeKind,
    widths: Vec<Pixels>,
    max_width: Pixels,
    row: usize,
    row_height: Pixels,
    row_min_height: Pixels,
}

#[derive(Clone, Copy)]
enum TableResizeKind {
    Column(usize),
    RightEdge,
    RowEdge,
    Corner,
}

impl TableResizeKind {
    fn priority(self) -> u8 {
        match self {
            Self::Corner => 0,
            Self::RightEdge | Self::RowEdge => 1,
            Self::Column(_) => 2,
        }
    }

    fn cursor(self) -> CursorStyle {
        match self {
            Self::Column(_) | Self::RightEdge => CursorStyle::ResizeLeftRight,
            Self::RowEdge => CursorStyle::ResizeUpDown,
            Self::Corner => CursorStyle::ResizeUpLeftDownRight,
        }
    }
}

struct TableResizeState {
    handle: TableResizeHandle,
    start: Point<Pixels>,
}

/// 一个已排好版的单元格。
#[derive(Clone)]
struct TableCellGeom {
    /// 单元格在投影显示文本内的字节范围（不含竖线/两端空格）。
    display: Range<usize>,
    /// 文本绘制 x（相对表格左缘，已含左内边距）。
    x: Pixels,
    /// 单元格可排版内宽（不含左右 padding）。
    inner_width: Pixels,
    /// 分隔行声明的列对齐方式。
    align: Align,
    /// 按 `<br>` 拆开的排版段；每段内部仍可按列宽软换行。
    parts: Vec<TableCellPart>,
}

/// 单元格内一段强制换行之间的文本几何。
#[derive(Clone)]
struct TableCellPart {
    /// 本段在整行显示文本中的字节范围。
    display: Range<usize>,
    /// 本段相对单元格内容顶边的 y。
    y: Pixels,
    /// 本段视觉行数。
    rows: usize,
    /// 本段形状化文本。
    shaped: Arc<WrappedLine>,
}

/// 一行表格的网格几何。
#[derive(Clone)]
struct TableRowGeom {
    /// 共享列宽/边界。
    grid: Arc<TableGridGeom>,
    /// 本行各列单元格（缺失列用空单元格补齐）。
    cells: Vec<TableCellGeom>,
    /// 文本相对行顶的纵向偏移（表头行加高后垂直居中）。
    offset_y: Pixels,
    /// 单元格内每个视觉行的行高。
    line_height: Pixels,
    /// 整行高度（含单元格换行与表头内边距）。
    height: Pixels,
    /// 内容决定的最小行高（手动拖动不能低于它）。
    natural_height: Pixels,
    /// 该行源起点（会话行高状态键）。
    source_start: usize,
}

/// 最近的单元格：显示字节 → (单元格序号, 单元格内相对字节)。
///
/// 距离用「字节到区间」度量：区间内为 0，区间外为到最近边界的字节数。注意
/// 比较的是**距离**，不能拿相对字节 `local` 当距离——多字单元格末尾的 `local`
/// 会大于下一格的距离，导致光标在视觉上提前跳进下一格。距离相同保留靠前的
/// 单元格（光标停在格尾时仍属于本格）。纯函数便于单测，不依赖排版几何。
fn nearest_cell<'a>(
    cells: impl IntoIterator<Item = &'a Range<usize>>,
    byte: usize,
) -> Option<(usize, usize)> {
    // (单元格序号, 格内相对字节, 距离)；只有距离更小才替换。
    let mut best: Option<(usize, usize, usize)> = None;
    for (i, cell) in cells.into_iter().enumerate() {
        let dist = if byte < cell.start {
            cell.start - byte
        } else {
            byte.saturating_sub(cell.end)
        };
        let local = byte.clamp(cell.start, cell.end) - cell.start;
        match best {
            Some((_, _, best_dist)) if best_dist <= dist => {}
            _ => best = Some((i, local, dist)),
        }
    }
    best.map(|(i, local, _)| (i, local))
}

/// 表格行：显示列 → 最近单元格及其相对字节。
fn table_cell_of_col<'a>(
    row: &'a TableRowGeom,
    line: &ProjectedLine,
    col: usize,
) -> Option<(&'a TableCellGeom, usize)> {
    let byte = line.display_byte_of_col(col);
    let (i, local) = nearest_cell(row.cells.iter().map(|c| &c.display), byte)?;
    row.cells.get(i).map(|cell| (cell, local))
}

/// 表格行：显示列 → 块内坐标（表头含垂直居中偏移）。
fn table_point_of_col(
    row: &TableRowGeom,
    line: &ProjectedLine,
    col: usize,
) -> Option<Point<Pixels>> {
    let (cell, _) = table_cell_of_col(row, line, col)?;
    let byte = line.display_byte_of_col(col);
    let part = cell
        .parts
        .iter()
        .find(|part| byte < part.display.end)
        .or_else(|| cell.parts.last())?;
    let local = byte.clamp(part.display.start, part.display.end) - part.display.start;
    let p = part
        .shaped
        .position_for_index(local, row.line_height)
        .unwrap_or_else(|| point(Pixels::ZERO, Pixels::ZERO));
    let visual_row = (p.y / row.line_height) as usize;
    let align_x = wrapped_row_align_offset(&part.shaped, visual_row, cell.inner_width, cell.align);
    Some(point(cell.x + align_x + p.x, row.offset_y + part.y + p.y))
}

/// 表格行：块内坐标 → 显示列。
fn table_col_of_point(
    row: &TableRowGeom,
    line: &ProjectedLine,
    x: Pixels,
    y: Pixels,
) -> Option<usize> {
    let idx = row
        .grid
        .boundaries
        .partition_point(|&b| b <= x)
        .saturating_sub(1)
        .min(row.cells.len().saturating_sub(1));
    let cell = row.cells.get(idx)?;
    let local_x = x - cell.x;
    let cell_y = (y - row.offset_y).max(Pixels::ZERO);
    let part = cell
        .parts
        .iter()
        .find(|part| cell_y < part.y + row.line_height * part.rows as f32)
        .or_else(|| cell.parts.last())?;
    let max_y = row.line_height * part.rows.saturating_sub(1) as f32;
    let local_y = (cell_y - part.y).clamp(Pixels::ZERO, max_y);
    let visual_row = (local_y / row.line_height) as usize;
    let align_x = wrapped_row_align_offset(&part.shaped, visual_row, cell.inner_width, cell.align);
    let mut byte = part
        .shaped
        .closest_index_for_position(point(local_x - align_x, local_y), row.line_height)
        .unwrap_or_else(|ix| ix);
    while byte > 0 && !part.shaped.text.is_char_boundary(byte) {
        byte -= 1;
    }
    Some(line.display_col_of_byte(part.display.start + byte))
}

impl BlockGeom {
    /// 表格行：显示列 → 块内坐标（文本基线行顶；含表头垂直居中偏移）。
    fn table_point_of_col(&self, col: usize) -> Option<Point<Pixels>> {
        table_point_of_col(self.table.as_ref()?, &self.proj, col)
    }

    /// 表格行：块内坐标 → 显示列。
    ///
    /// x 先按列边界定位列，再在单元格内做最近字节命中；落在单元格右侧内边距时
    /// 归到该格末尾（与点击表格右半部分的行为一致）。
    fn table_col_of_point(&self, x: Pixels, y: Pixels) -> Option<usize> {
        table_col_of_point(self.table.as_ref()?, &self.proj, x, y)
    }

    /// 显示列 → 块内坐标（表格走单元格排版，普通行走整行 shape）。
    fn position_of_col(&self, col: usize) -> Option<Point<Pixels>> {
        if self.table.is_some() {
            self.table_point_of_col(col)
        } else {
            self.shaped
                .position_for_index(self.byte_of_col(col), self.line_height)
        }
    }

    /// 块内坐标 → 显示列（表格走单元格排版，普通行走整行 shape）。
    fn col_of_point(&self, x: Pixels, y: Pixels) -> Option<usize> {
        if self.table.is_some() {
            self.table_col_of_point(x, y)
        } else {
            let byte = self
                .shaped
                .closest_index_for_position(point(x, y), self.line_height)
                .unwrap_or_else(|ix| ix);
            Some(self.col_of_byte(byte))
        }
    }
}

/// 从整行形状文本/样式段中切出一个单元格，保持逐段样式。
fn slice_line_runs(
    text: &str,
    runs: &[gpui::TextRun],
    range: Range<usize>,
) -> (String, Vec<gpui::TextRun>) {
    let end = range.end.min(text.len());
    let start = range.start.min(end);
    let mut out = Vec::new();
    let mut pos = 0usize;
    for run in runs {
        let rs = pos;
        let re = pos + run.len;
        pos = re;
        let s = rs.max(start);
        let e = re.min(end);
        if s < e {
            let mut r = run.clone();
            r.len = e - s;
            out.push(r);
        }
    }
    let mut text = text.to_string();
    text.truncate(end);
    let text = text.split_off(start);
    (text, out)
}

/// 按单元格内的零宽 `<br>` 显示字节位置拆分排版段；连续/边界换行保留空段。
fn table_cell_parts(range: Range<usize>, breaks: &[usize]) -> Vec<Range<usize>> {
    let mut parts = Vec::new();
    let mut start = range.start;
    for &at in breaks {
        if at < start || at > range.end {
            continue;
        }
        parts.push(start..at);
        start = at;
    }
    parts.push(start..range.end);
    parts
}

fn table_text_align(align: Align) -> TextAlign {
    match align {
        Align::Left => TextAlign::Left,
        Align::Center => TextAlign::Center,
        Align::Right => TextAlign::Right,
    }
}

fn wrapped_row_width(line: &WrappedLine, visual_row: usize) -> Pixels {
    let rows = row_byte_bounds(line);
    let Some((&start, &end)) = rows.get(visual_row).zip(rows.get(visual_row + 1)) else {
        return Pixels::ZERO;
    };
    let x0 = line.unwrapped_layout.x_for_index(start);
    let x1 = line.unwrapped_layout.x_for_index(end);
    (x1 - x0).max(Pixels::ZERO)
}

fn wrapped_row_align_offset(
    line: &WrappedLine,
    visual_row: usize,
    inner_width: Pixels,
    align: Align,
) -> Pixels {
    table_alignment_offset(wrapped_row_width(line, visual_row), inner_width, align)
}

fn table_alignment_offset(content_width: Pixels, inner_width: Pixels, align: Align) -> Pixels {
    let extra = (inner_width - content_width).max(Pixels::ZERO);
    match align {
        Align::Left => Pixels::ZERO,
        Align::Center => extra / 2.,
        Align::Right => extra,
    }
}

/// 调整相邻两列宽度，保持两列总宽不变并限制最小列宽。
fn resized_table_columns(widths: &[Pixels], column: usize, delta: Pixels) -> Option<Vec<Pixels>> {
    let right = column.checked_add(1)?;
    let (&left_width, &right_width) = widths.get(column).zip(widths.get(right))?;
    let pair_total = left_width + right_width;
    if pair_total < TABLE_DRAG_MIN_WIDTH * 2. {
        return None;
    }
    let left = (left_width + delta).clamp(TABLE_DRAG_MIN_WIDTH, pair_total - TABLE_DRAG_MIN_WIDTH);
    let mut resized = widths.to_vec();
    resized[column] = left;
    resized[right] = pair_total - left;
    Some(resized)
}

fn resized_table_right_edge(
    widths: &[Pixels],
    delta: Pixels,
    max_width: Pixels,
) -> Option<Vec<Pixels>> {
    let (&last, prefix) = widths.split_last()?;
    let prefix_width: Pixels = prefix.iter().copied().sum();
    let max_last = (max_width - prefix_width).max(TABLE_DRAG_MIN_WIDTH);
    let mut resized = widths.to_vec();
    *resized.last_mut()? = (last + delta).clamp(TABLE_DRAG_MIN_WIDTH, max_last);
    Some(resized)
}

fn resized_table_row_height(height: Pixels, delta: Pixels, minimum: Pixels) -> Pixels {
    (height + delta).max(minimum)
}

/// 表格行内左右方向键的目标：跳过竖线/单元格填充与行内隐藏标记，只落在可见文本上。
///
/// 用**显示文本**（而非源码）逐字符移动，因此 `**粗体**` 这类隐藏标记不会造成
/// "按一次没反应"；到本行单元格边界时跨到相邻表格行（跳过分隔行），到表格边界时
/// 移到相邻逻辑行的行首/行尾。非表格行返回 `None`，由调用方按普通字符移动。
fn table_arrow_step(doc: &mut Document, caret: usize, forward: bool) -> Option<usize> {
    let projection = doc.projection();
    let idx = projection.line_of_source(caret);
    let line = projection.lines.get(idx)?;
    line.block.table_line?;
    let cells = line.table_cells.as_deref()?;
    let display = line.display_text();
    let disp_col = line.offset_map.source_to_display(caret).get();
    let disp_byte = line.display_byte_of_col(disp_col);
    if let Some(target) = cell_step(&display, cells, disp_byte, forward) {
        return Some(source_of_display_byte(line, target));
    }

    // 本行单元格已到边界：跳到相邻表格行的首/末单元格（分隔行显示为空，跳过）
    let mut i = idx;
    loop {
        if forward {
            i += 1;
        } else {
            i = i.checked_sub(1)?;
        }
        let line = projection.lines.get(i)?;
        match line.block.table_line {
            // 表格边界：移到表格外相邻逻辑行的行首/行尾（没有相邻行则交给普通移动）
            None => {
                return Some(if forward {
                    line.source_range.start
                } else {
                    line.source_range.end
                });
            }
            Some(TableLine::Delimiter) => continue,
            Some(_) => {
                let cells = line.table_cells.as_deref()?;
                let target = if forward {
                    cells.first()?.start
                } else {
                    cells.last()?.end
                };
                return Some(source_of_display_byte(line, target));
            }
        }
    }
}

/// 显示字节 → 源字节（隐藏标记会吸附到可见文本边界，正好跳过它们）。
fn source_of_display_byte(line: &ProjectedLine, byte: usize) -> usize {
    line.offset_map
        .display_to_source(DisplayPos(line.display_col_of_byte(byte)))
}

/// 横向移动落到表格行边界时，把目标吸附到**单元格内**。
///
/// 普通字符移动从表格外进入时，会先落在行首（首个 `|` 之前）或行尾
/// （末个 `|` 之后）。这两个位置在网格里都被绘制成首个/末个单元格的边界，
/// 看起来像已经进格，但输入会落到竖线外，Markdown 会把新增文字解析成新单元格。
/// 这里把行首吸附到首格文本起点、行尾吸附到末格文本末尾，保证第一下按键就
/// 真正进入单元格。
fn table_entry_snap(doc: &mut Document, target: usize) -> Option<usize> {
    let projection = doc.projection();
    let line = projection.lines.get(projection.line_of_source(target))?;
    line.block.table_line?;
    let cells = line.table_cells.as_deref()?;
    if target <= line.source_range.start {
        Some(source_of_display_byte(line, cells.first()?.start))
    } else if target >= line.source_range.end {
        Some(source_of_display_byte(line, cells.last()?.end))
    } else {
        None
    }
}

/// 横向移动：表格内/进出表格都走表格规则，其余交给普通字符移动。
fn horizontal_step(doc: &mut Document, caret: usize, forward: bool) -> usize {
    if let Some(target) = table_arrow_step(doc, caret, forward) {
        return target;
    }
    let raw = if forward {
        doc.next_char_boundary(caret)
    } else {
        doc.prev_char_boundary(caret)
    };
    table_entry_snap(doc, raw).unwrap_or(raw)
}

/// 表格网格测量所需的字体/主题/宽度参数（聚在一起避免函数参数过多）。
struct TableGridCtx<'a> {
    content_width_total: Pixels,
    indent_base: Pixels,
    base_font: &'a Font,
    mono_font: &'a Font,
    palette: &'a Palette,
    base_size: Pixels,
    mono_size: Pixels,
}

/// 为所有表格构建按内容列宽的网格几何；放不下（总宽超过可用宽度）的表格返回 `None`，
/// 继续走原来的整行流式排版（不强行压缩/裁字）。
fn build_table_grids(
    window: &mut Window,
    projection: &Projection,
    ctx: &TableGridCtx<'_>,
    overrides: &HashMap<usize, Vec<Pixels>>,
    row_heights: &HashMap<usize, Pixels>,
) -> Vec<Option<TableRowGeom>> {
    let TableGridCtx {
        content_width_total,
        indent_base,
        base_font,
        mono_font,
        palette,
        base_size,
        mono_size,
    } = *ctx;
    let mut out: Vec<Option<TableRowGeom>> = vec![None; projection.lines.len()];
    for widget in projection.block_widgets() {
        let BlockWidgetKind::Table { align } = &widget.kind else {
            continue;
        };
        let indent = projection
            .lines
            .get(widget.line_range.start)
            .map_or(indent_base, |l| block_indent(l, indent_base));
        let avail = (content_width_total - indent - BASE).max(px(48.));

        struct RawRow {
            idx: usize,
            header: bool,
            text: String,
            runs: Vec<gpui::TextRun>,
            prefix: usize,
            font_size: Pixels,
            cells: Vec<Range<usize>>,
            breaks: Vec<usize>,
        }
        let mut raw: Vec<RawRow> = Vec::new();
        let mut cols = align.len();
        let mut bail = false;
        for idx in widget.line_range.clone() {
            // 分隔行显示为空，不参与表格内容排版
            if idx == widget.line_range.start + 1 {
                continue;
            }
            let line = &projection.lines[idx];
            let Some(cells) = line.table_cells.clone() else {
                continue;
            };
            let (text, runs, oblique) = build_line_runs(line, base_font, mono_font, palette, None);
            // 中文合成斜体需要逐字现取轮廓，网格路径暂不支持：整表回退流式排版。
            if !oblique.is_empty() {
                bail = true;
                break;
            }
            let prefix = text.len().saturating_sub(line.display_text().len());
            let font_size = block_font_size(&line.block, base_size, mono_size);
            cols = cols.max(cells.len());
            raw.push(RawRow {
                idx,
                header: line.block.table_line == Some(TableLine::Header),
                text: text.to_string(),
                runs,
                prefix,
                font_size,
                cells,
                breaks: line.html_breaks.clone(),
            });
        }
        if bail || cols == 0 || raw.is_empty() {
            continue;
        }

        let pad = TABLE_CELL_PAD;
        let min_width = (pad * 2. + TABLE_CELL_MIN_INNER).max(px(2.));
        let floor_width = (avail / cols as f32).max(px(2.));
        if avail < pad * 2. * cols as f32 + px(16.) {
            // 可用宽度连最小内边距都放不下，保持旧的流式回退
            continue;
        }
        let min_width = min_width.min(floor_width);

        // 先量自然宽度（列宽包含左右 padding），再决定是否压缩
        let mut widths: Vec<Pixels> = vec![min_width; cols];
        for row in &raw {
            for (c, width) in widths.iter_mut().enumerate() {
                let range = row.cells.get(c).cloned().unwrap_or(0..0);
                for part in table_cell_parts(range, &row.breaks) {
                    let abs = row.prefix + part.start..row.prefix + part.end;
                    let (ctext, cruns) = slice_line_runs(&row.text, &row.runs, abs);
                    let shaped =
                        window
                            .text_system()
                            .shape_line(ctext.into(), row.font_size, &cruns, None);
                    *width = (*width).max(shaped.width() + pad * 2.);
                }
            }
        }
        let natural_total: Pixels = widths.iter().copied().sum();
        if natural_total > avail {
            // 在 [min_width, natural] 之间做线性压缩：总宽超出越多，列越接近最小宽度
            let min_total = min_width * cols as f32;
            let scale = if natural_total > min_total {
                ((avail - min_total) / (natural_total - min_total)).clamp(0., 1.)
            } else {
                0.
            };
            for w in &mut widths {
                let natural = (*w).max(min_width);
                *w = min_width + (natural - min_width) * scale;
            }
        }
        if let Some(saved) = overrides.get(&widget.source_range.start)
            && saved.len() == cols
            && saved.iter().all(|width| *width >= TABLE_DRAG_MIN_WIDTH)
            && saved.iter().copied().sum::<Pixels>() <= avail
        {
            widths.clone_from(saved);
        }
        // 防止浮点累计略超可用宽度；最后一列吸收误差
        let mut total: Pixels = widths.iter().copied().sum();
        if total > avail {
            let last = widths.len() - 1;
            widths[last] = (widths[last] - (total - avail)).max(min_width);
            total = widths.iter().copied().sum();
        }
        let mut boundaries = Vec::with_capacity(cols + 1);
        boundaries.push(Pixels::ZERO);
        for w in &widths {
            let last = *boundaries.last().unwrap();
            boundaries.push(last + *w);
        }
        let grid = Arc::new(TableGridGeom {
            source_start: widget.source_range.start,
            boundaries: boundaries.clone(),
            width: total,
        });

        for row in &raw {
            let line = &projection.lines[row.idx];
            let line_height = block_line_px_with_mono(&line.block, row.font_size, mono_size);
            let mut cells = Vec::with_capacity(cols);
            let mut max_lines = 1usize;
            for (c, w) in widths.iter().enumerate() {
                let range = row.cells.get(c).cloned().unwrap_or(0..0);
                let inner = (*w - pad * 2.).max(Pixels::ZERO);
                let mut parts = Vec::new();
                let mut part_y = Pixels::ZERO;
                for part in table_cell_parts(range.clone(), &row.breaks) {
                    let abs = row.prefix + part.start..row.prefix + part.end;
                    let (ctext, cruns) = slice_line_runs(&row.text, &row.runs, abs);
                    let shaped = shape_wrapped_line(
                        window,
                        ctext.into(),
                        row.font_size,
                        &cruns,
                        inner.max(px(1.)),
                    );
                    let rows = shaped.wrap_boundaries().len() + 1;
                    parts.push(TableCellPart {
                        display: part,
                        y: part_y,
                        rows,
                        shaped: Arc::new(shaped),
                    });
                    part_y += line_height * rows as f32;
                }
                let lines: usize = parts.iter().map(|part| part.rows).sum();
                max_lines = max_lines.max(lines.max(1));
                let align = align.get(c).copied().unwrap_or(Align::Left);
                cells.push(TableCellGeom {
                    display: range.clone(),
                    x: boundaries[c] + pad,
                    inner_width: inner,
                    align,
                    parts,
                });
            }
            let header_pad = if row.header { px(12.) } else { Pixels::ZERO };
            let natural_height = line_height * max_lines as f32 + header_pad;
            let height = row_heights
                .get(&line.source_range.start)
                .copied()
                .map_or(natural_height, |saved| saved.max(natural_height));
            out[row.idx] = Some(TableRowGeom {
                grid: grid.clone(),
                cells,
                offset_y: if row.header {
                    header_pad / 2.
                } else {
                    Pixels::ZERO
                },
                line_height,
                height,
                natural_height,
                source_start: line.source_range.start,
            });
        }
    }
    out
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
    /// 可拖动的内部列边界。
    table_resize_handles: Vec<TableResizeHandle>,
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
        // 命中时允许稍微超出视口（拖拽选区、Shift+点选时常超出编辑区）
        // 只做最基本的范围钳制，避免命中到标题栏等完全无关区域
        let rel_y = position.y - self.origin.y;
        // 落在内容末尾以下也不放光标（行映射会钳到末行，但那会误改光标位置）
        if rel_y < Pixels::ZERO || rel_y >= self.total_height() {
            return None;
        }
        let row = self.row_at(rel_y)?;
        // row_starts[i] <= row < row_starts[i+1] → 块 i（首元素为 0，故减一后非负）
        let block_idx = self.row_starts.partition_point(|&s| s <= row) - 1;
        let block = &self.blocks[block_idx];
        // 块内相对 y（含所在视觉行的偏移；行内 x 需再减块缩进）
        let block_top = self.row_top(self.row_starts[block_idx]);
        let rel_x = position.x - self.origin.x - block.indent;
        let local_y = rel_y - block_top;
        Some((block_idx, block.col_of_point(rel_x, local_y)?))
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
    let p = block.position_of_col(block.col_of_source(caret))?;
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
    let tcol = tblock.col_of_point(local_x, local_y)?;
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

/// 单个文件内的命中行（全工作区搜索的纯文本中间结果）。
#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkspaceTextHit {
    /// 行号（1 基，展示用）。
    line_no: usize,
    /// 该行文本（不含行尾 `\n` / `\r\n`）。
    line: String,
    /// 命中在该行文本中的字节区间（用于预览高亮）。
    range: Range<usize>,
    /// 命中在文件源文本中的字节区间（打开文件后据此选区/定位）。
    source: Range<usize>,
}

/// 全工作区搜索的 UI 状态（`Cmd+Shift+F`，SPEC D40）。
///
/// 结果在每次查询变化时同步重算（本地工作区、仅 Markdown 文件）；每条命中保留
/// 文件相对路径与源偏移，点击后走 `open_path` + 光标选区完成「打开并定位」。
#[derive(Default)]
struct WorkspaceSearchState {
    /// 面板是否展开。
    open: bool,
    /// 查询文本。
    query: String,
    /// 是否区分大小写（默认 `false`）。
    case_sensitive: bool,
    /// 扁平命中列表（按文件、行号排序）。
    hits: Vec<WorkspaceHit>,
    /// 当前选中项下标（`hits` 内）。
    selected: usize,
    /// 命中的文件数（按路径去重）。
    file_count: usize,
    /// 读取失败/非 UTF-8 等原因跳过的文件数。
    skipped: usize,
    /// 是否因结果上限被截断。
    truncated: bool,
}

/// 全工作区搜索的一条命中（带文件信息，供 UI 与跳转使用）。
#[derive(Clone)]
struct WorkspaceHit {
    /// 文件相对工作区根路径（`/` 分隔）。
    rel: String,
    /// 文件绝对路径。
    path: PathBuf,
    /// 行号（1 基）。
    line_no: usize,
    /// 该行文本（不含行尾换行）。
    line: String,
    /// 命中在该行文本中的字节区间。
    range: Range<usize>,
    /// 命中在文件源文本中的字节区间（跳转后选区）。
    source: Range<usize>,
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
enum PaletteAction {
    NewTab,
    CloseTab,
    CloseAllTabs,
    OpenFile,
    NewFile,
    NewFolder,
    Save,
    SaveAs,
    ExportPdf,
    Find,
    WorkspaceSearch,
    ToggleTree,
    ToggleOutline,
    ToggleFocusMode,
    ToggleTypewriterMode,
    ToggleSourceMode,
    MoveCurrentFile,
    SetTheme(ThemePreference),
    CycleUiFont,
    CycleMonoFont,
    FontSizeUp,
    FontSizeDown,
    MonoFontSizeUp,
    MonoFontSizeDown,
    ResetAppearance,
}

/// 菜单命令：复用命令面板动作，并补充编辑、应用、窗口级操作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MenuCommand {
    Palette(PaletteAction),
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    OpenPalette,
    CycleTheme,
    Quit,
    Hide,
    HideOthers,
    ShowAll,
    About,
    Minimize,
    Zoom,
    ToggleFullscreen,
}

#[derive(Clone, PartialEq, Debug, gpui_kit::Action)]
#[action(namespace = mazha_doc, no_json)]
struct MenuAction {
    command: MenuCommand,
}

fn menu_action(command: MenuCommand) -> MenuAction {
    MenuAction { command }
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

fn effective_ui_font(settings: &Settings, theme: &Theme) -> SharedString {
    settings
        .font_family
        .clone()
        .map(SharedString::from)
        .unwrap_or_else(|| theme.font_family.clone())
}

fn effective_mono_font(settings: &Settings, theme: &Theme) -> SharedString {
    settings
        .mono_font_family
        .clone()
        .map(SharedString::from)
        .unwrap_or_else(|| theme.mono_font_family.clone())
}

fn apply_theme_preference(preference: ThemePreference, window: &mut Window, cx: &mut App) {
    match preference {
        ThemePreference::System => Theme::sync_system_appearance(Some(window), cx),
        ThemePreference::Light => Theme::change(ThemeMode::Light, Some(window), cx),
        ThemePreference::Dark => Theme::change(ThemeMode::Dark, Some(window), cx),
    }
}

fn menu_item(name: &str, command: MenuCommand) -> MenuItem {
    MenuItem::action(name, menu_action(command))
}

fn menu_item_checked(name: &str, command: MenuCommand, checked: bool) -> MenuItem {
    menu_item(name, command).checked(checked)
}

fn app_menus(settings: &Settings) -> Vec<Menu> {
    use MenuCommand::Palette;
    use PaletteAction::*;
    let theme = |preference: ThemePreference| {
        let label = match preference {
            ThemePreference::System => "跟随系统",
            ThemePreference::Light => "浅色",
            ThemePreference::Dark => "深色",
        };
        menu_item_checked(
            label,
            Palette(SetTheme(preference)),
            settings.theme == preference,
        )
    };
    vec![
        Menu::new("码札").items([
            menu_item("关于码札", MenuCommand::About),
            MenuItem::separator(),
            MenuItem::os_submenu("服务", SystemMenuType::Services),
            MenuItem::separator(),
            menu_item("隐藏码札", MenuCommand::Hide),
            menu_item("隐藏其他", MenuCommand::HideOthers),
            menu_item("全部显示", MenuCommand::ShowAll),
            MenuItem::separator(),
            menu_item("退出码札", MenuCommand::Quit),
        ]),
        Menu::new("文件").items([
            menu_item("新建标签页", Palette(NewTab)),
            menu_item("打开…", Palette(OpenFile)),
            MenuItem::separator(),
            menu_item("保存", Palette(Save)),
            menu_item("另存为…", Palette(SaveAs)),
            menu_item("导出 PDF…", Palette(ExportPdf)),
            MenuItem::separator(),
            menu_item("关闭标签页", Palette(CloseTab)),
            menu_item("关闭全部标签页", Palette(CloseAllTabs)),
            MenuItem::separator(),
            menu_item("移动当前文件…", Palette(MoveCurrentFile)),
        ]),
        Menu::new("编辑").items([
            menu_item("撤销", MenuCommand::Undo),
            menu_item("重做", MenuCommand::Redo),
            MenuItem::separator(),
            MenuItem::os_action(
                "剪切",
                menu_action(MenuCommand::Cut),
                gpui_kit::OsAction::Cut,
            ),
            MenuItem::os_action(
                "复制",
                menu_action(MenuCommand::Copy),
                gpui_kit::OsAction::Copy,
            ),
            MenuItem::os_action(
                "粘贴",
                menu_action(MenuCommand::Paste),
                gpui_kit::OsAction::Paste,
            ),
            MenuItem::os_action(
                "全选",
                menu_action(MenuCommand::SelectAll),
                gpui_kit::OsAction::SelectAll,
            ),
            MenuItem::separator(),
            menu_item("查找/替换…", Palette(Find)),
            menu_item("全工作区搜索…", Palette(WorkspaceSearch)),
        ]),
        Menu::new("视图").items([
            menu_item("命令面板…", MenuCommand::OpenPalette),
            MenuItem::separator(),
            menu_item("切换文件树", Palette(ToggleTree)),
            menu_item("切换大纲", Palette(ToggleOutline)),
            MenuItem::separator(),
            menu_item_checked("专注模式", Palette(ToggleFocusMode), settings.focus_mode),
            menu_item_checked(
                "打字机模式",
                Palette(ToggleTypewriterMode),
                settings.typewriter_mode,
            ),
            menu_item_checked("源码模式", Palette(ToggleSourceMode), settings.source_mode),
            MenuItem::separator(),
            MenuItem::submenu(Menu::new("主题").items([
                theme(ThemePreference::System),
                theme(ThemePreference::Light),
                theme(ThemePreference::Dark),
                MenuItem::separator(),
                menu_item("循环主题", MenuCommand::CycleTheme),
            ])),
            MenuItem::separator(),
            menu_item("切换正文字体", Palette(CycleUiFont)),
            menu_item("切换等宽字体", Palette(CycleMonoFont)),
            MenuItem::separator(),
            menu_item("增大正文字号", Palette(FontSizeUp)),
            menu_item("减小正文字号", Palette(FontSizeDown)),
            menu_item("增大等宽字号", Palette(MonoFontSizeUp)),
            menu_item("减小等宽字号", Palette(MonoFontSizeDown)),
            MenuItem::separator(),
            menu_item("重置外观", Palette(ResetAppearance)),
        ]),
        Menu::new("窗口").items([
            menu_item("最小化", MenuCommand::Minimize),
            menu_item("缩放", MenuCommand::Zoom),
            MenuItem::separator(),
            menu_item("进入全屏", MenuCommand::ToggleFullscreen),
        ]),
    ]
}

fn menu_key_bindings() -> Vec<KeyBinding> {
    use MenuCommand::Palette;
    use PaletteAction::*;
    let doc = Some("mazha-doc");
    vec![
        KeyBinding::new("secondary-q", menu_action(MenuCommand::Quit), None),
        KeyBinding::new("secondary-h", menu_action(MenuCommand::Hide), None),
        KeyBinding::new(
            "secondary-alt-h",
            menu_action(MenuCommand::HideOthers),
            None,
        ),
        KeyBinding::new("secondary-t", menu_action(Palette(NewTab)), doc),
        KeyBinding::new("secondary-o", menu_action(Palette(OpenFile)), doc),
        KeyBinding::new("secondary-s", menu_action(Palette(Save)), doc),
        KeyBinding::new("secondary-shift-s", menu_action(Palette(SaveAs)), doc),
        KeyBinding::new("secondary-e", menu_action(Palette(ExportPdf)), doc),
        KeyBinding::new("secondary-w", menu_action(Palette(CloseTab)), doc),
        KeyBinding::new("secondary-shift-w", menu_action(Palette(CloseAllTabs)), doc),
        KeyBinding::new("secondary-z", menu_action(MenuCommand::Undo), doc),
        KeyBinding::new("secondary-shift-z", menu_action(MenuCommand::Redo), doc),
        KeyBinding::new("secondary-x", menu_action(MenuCommand::Cut), doc),
        KeyBinding::new("secondary-c", menu_action(MenuCommand::Copy), doc),
        KeyBinding::new("secondary-v", menu_action(MenuCommand::Paste), doc),
        KeyBinding::new("secondary-a", menu_action(MenuCommand::SelectAll), doc),
        KeyBinding::new("secondary-f", menu_action(Palette(Find)), doc),
        KeyBinding::new(
            "secondary-shift-f",
            menu_action(Palette(WorkspaceSearch)),
            doc,
        ),
        KeyBinding::new("secondary-p", menu_action(MenuCommand::OpenPalette), doc),
        KeyBinding::new(
            "secondary-shift-o",
            menu_action(Palette(ToggleOutline)),
            doc,
        ),
        KeyBinding::new(
            "secondary-shift-l",
            menu_action(MenuCommand::CycleTheme),
            doc,
        ),
        KeyBinding::new(
            "secondary-shift-j",
            menu_action(Palette(ToggleFocusMode)),
            doc,
        ),
        KeyBinding::new(
            "secondary-shift-k",
            menu_action(Palette(ToggleTypewriterMode)),
            doc,
        ),
        KeyBinding::new("secondary-/", menu_action(Palette(ToggleSourceMode)), doc),
        KeyBinding::new("secondary-=", menu_action(Palette(FontSizeUp)), doc),
        KeyBinding::new("secondary--", menu_action(Palette(FontSizeDown)), doc),
        KeyBinding::new("secondary-alt-=", menu_action(Palette(MonoFontSizeUp)), doc),
        KeyBinding::new(
            "secondary-alt--",
            menu_action(Palette(MonoFontSizeDown)),
            doc,
        ),
        KeyBinding::new(
            "secondary-alt-0",
            menu_action(Palette(ResetAppearance)),
            doc,
        ),
        KeyBinding::new("secondary-m", menu_action(MenuCommand::Minimize), None),
        KeyBinding::new(
            "ctrl-secondary-f",
            menu_action(MenuCommand::ToggleFullscreen),
            None,
        ),
    ]
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
    /// 上次左键按下时间（双击/三击用）。
    last_click_at: Option<std::time::Instant>,
    /// 上次左键按下位置（同一区域内连续点击才计数）。
    last_click_pos: Option<Point<Pixels>>,
    /// 连续点击次数（1/2/3）。
    click_count: u32,
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
    /// 待处理的大纲跳转源偏移：下次 prepaint 把该行顶边对齐到视口顶部。
    ///
    /// 与 `caret_scroll_key` 分开，避免普通光标移动的“最小滚动”覆盖跳转置顶。
    scroll_to_source_top: Option<usize>,
    /// 焦点句柄：键盘事件只在获得焦点后送达。
    focus: FocusHandle,
    /// 光标当前是否处于闪烁的“可见”相位（每 500ms 翻转）。
    cursor_visible: bool,
    /// 光标闪烁定时任务；重新计时时替换旧任务即可取消旧循环。
    _blink_task: Task<()>,
    /// 焦点/失焦订阅：获得焦点开始闪烁，失焦立即隐藏。
    _blink_subscriptions: Vec<Subscription>,
    /// 系统字体族（正文 run 的字体）。
    ui_font: SharedString,
    /// 等宽字体族（来自设置或主题，代码块/行内代码使用）。
    mono_font: SharedString,
    /// 等宽字号（来自设置或主题）。
    mono_font_size: Pixels,
    /// 正文字号（来自设置或主题）。
    font_size: Pixels,
    /// 持久化的外观设置。
    settings: Settings,
    /// 系统深浅色变化订阅（仅「跟随系统」时生效）。
    _appearance_sub: Subscription,
    /// 主题色快照。
    palette: Palette,
    /// 代码块语法高亮器（首次渲染时惰性创建并复用；`SyntaxSet` 较大）。
    highlighter: Option<Highlighter>,
    /// 查找/替换条状态（`Cmd+F` 展开）。
    find: FindState,
    /// 全工作区搜索状态（`Cmd+Shift+F` 展开，D40）。
    workspace_search: WorkspaceSearchState,
    /// 工作区根（文件树）
    workspace_root: Option<PathBuf>,
    /// Workspace（本地文件系统）
    workspace: Option<Workspace<FileSystemProvider>>,
    /// 文件树是否展开
    tree_open: bool,
    /// 文件树中已展开的目录（存相对工作区根的 `DocPath` 字符串；根目录恒展开，不记录）。
    expanded_dirs: HashSet<String>,
    /// 文件树侧栏宽度（可拖动分隔条缩放，D58）。
    sidebar_width: Pixels,
    /// 是否正在拖动左侧文件树分隔条。
    sidebar_resizing: bool,
    /// 右侧大纲面板宽度（可拖动分隔条缩放）。
    outline_width: Pixels,
    /// 是否正在拖动右侧大纲分隔条。
    outline_resizing: bool,
    /// 大纲（目录导航）是否展开
    outline_open: bool,
    /// 已打开文件的保存快照（脏状态用）
    saved_text: Option<String>,
    /// 命令面板状态（`Cmd+P`，D38）。
    #[allow(dead_code)]
    cmd_palette: PaletteState,
    /// 正在重命名的文件树条目：`(原路径, 编辑中的新名)`；`None` 表示无。
    renaming: Option<(PathBuf, String)>,
    /// 是否有原生文件对话框正在显示。
    dialog_open: bool,
    /// 音视频播放状态；键为 `kind:src`，仅保留当前文档仍引用的媒体。
    media: HashMap<String, MediaPlayback>,
    /// 当前文档会话内的表格列宽（键为表格源起点，不写入 Markdown）。
    table_column_widths: HashMap<usize, Vec<Pixels>>,
    /// 当前文档会话内的表格行高（键为行源起点，不写入 Markdown）。
    table_row_heights: HashMap<usize, Pixels>,
    /// 正在拖动的表格内部列边界。
    table_resizing: Option<TableResizeState>,
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
    fn new(
        path: Option<&PathBuf>,
        settings: Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let text = match path {
            Some(p) => std::fs::read_to_string(p).unwrap_or_else(|e| format!("> 读取失败：{e}\n")),
            None => SAMPLE.to_string(),
        };
        let mut settings = settings;
        settings.normalize();
        let theme = cx.theme();
        let palette = Palette::from_theme(theme);
        // 先复制字体信息，结束 `theme` 对 `cx` 的不可变借用，后面注册焦点订阅需可变借用。
        let ui_font = effective_ui_font(&settings, theme);
        let mono_font = effective_mono_font(&settings, theme);
        let font_size = settings.font_size.map(px).unwrap_or(theme.font_size);
        let mono_font_size = settings
            .mono_font_size
            .map(px)
            .unwrap_or(theme.mono_font_size);
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
        // 恢复该工作区上次的文件树展开状态（D72）；键为工作区根目录字符串。
        let expanded_dirs: HashSet<String> = ws_root
            .as_ref()
            .map(|r| {
                settings
                    .tree_expanded_for(&r.to_string_lossy())
                    .into_iter()
                    .collect()
            })
            .unwrap_or_default();
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
        let focus = cx.focus_handle();
        let focus_sub = cx.on_focus(&focus, window, |view, _, cx| view.start_blink(cx));
        let blur_sub = cx.on_blur(&focus, window, |view, _, cx| view.stop_blink(cx));
        let appearance_sub = cx.observe_window_appearance(window, |view, window, cx| {
            if view.settings.theme == ThemePreference::System {
                Theme::sync_system_appearance(Some(window), cx);
                view.refresh_appearance(cx);
            }
        });
        Self {
            tabs: vec![t],
            active: 0,
            path: path.cloned(),
            doc: Document::new(&text),
            cursors: vec![Cursor::new(0)],
            primary: 0,
            selecting: false,
            last_click_at: None,
            last_click_pos: None,
            click_count: 1,
            layout: None,
            scroll: ScrollHandle::new(),
            content_height: px(0.),
            caret_scroll_key: None,
            scroll_to_source_top: None,
            focus,
            cursor_visible: false,
            _blink_task: Task::ready(()),
            _blink_subscriptions: vec![focus_sub, blur_sub],
            ui_font,
            mono_font,
            mono_font_size,
            font_size,
            settings,
            _appearance_sub: appearance_sub,
            palette,
            highlighter: None,
            find: FindState::default(),
            workspace_search: WorkspaceSearchState::default(),
            workspace_root: ws_root,
            workspace: ws,
            tree_open: true,
            expanded_dirs,
            sidebar_width: px(220.),
            sidebar_resizing: false,
            outline_width: px(220.),
            outline_resizing: false,
            outline_open: true,
            saved_text,
            cmd_palette: PaletteState::default(),
            renaming: None,
            dialog_open: false,
            media: HashMap::new(),
            table_column_widths: HashMap::new(),
            table_row_heights: HashMap::new(),
            table_resizing: None,
        }
    }

    /// 依据当前主题与设置刷新字体、字号、调色板，并丢弃旧布局/高亮缓存。
    fn refresh_appearance(&mut self, cx: &mut Context<Self>) {
        let theme = cx.theme();
        let palette = Palette::from_theme(theme);
        let ui_font = effective_ui_font(&self.settings, theme);
        let mono_font = effective_mono_font(&self.settings, theme);
        let font_size = self.settings.font_size.map(px).unwrap_or(theme.font_size);
        let mono_font_size = self
            .settings
            .mono_font_size
            .map(px)
            .unwrap_or(theme.mono_font_size);
        self.palette = palette;
        self.ui_font = ui_font;
        self.mono_font = mono_font;
        self.font_size = font_size;
        self.mono_font_size = mono_font_size;
        self.layout = None;
        self.highlighter = None;
        self.caret_scroll_key = None;
        cx.notify();
    }

    fn persist_settings(&self) {
        if let Err(error) = self.settings.save() {
            eprintln!("保存外观设置失败：{error}");
        }
    }

    fn begin_dialog(&mut self, cx: &mut Context<Self>) {
        self.dialog_open = true;
        cx.notify();
    }

    fn set_theme_preference(
        &mut self,
        preference: ThemePreference,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings.theme == preference {
            return;
        }
        self.settings.theme = preference;
        self.persist_settings();
        apply_theme_preference(preference, window, cx);
        self.refresh_appearance(cx);
        cx.set_menus(app_menus(&self.settings));
    }

    fn cycle_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let next = self.settings.theme.next();
        self.set_theme_preference(next, window, cx);
    }

    fn cycle_ui_font(&mut self, cx: &mut Context<Self>) {
        self.settings.cycle_ui_font();
        self.persist_settings();
        self.refresh_appearance(cx);
    }

    fn cycle_mono_font(&mut self, cx: &mut Context<Self>) {
        self.settings.cycle_mono_font();
        self.persist_settings();
        self.refresh_appearance(cx);
    }

    fn adjust_font_size(&mut self, delta: f32, cx: &mut Context<Self>) {
        let current: f32 = self.font_size.into();
        self.settings.font_size = Some(clamp_font_size(current + delta));
        self.persist_settings();
        self.refresh_appearance(cx);
    }

    fn adjust_mono_font_size(&mut self, delta: f32, cx: &mut Context<Self>) {
        let current: f32 = self.mono_font_size.into();
        self.settings.mono_font_size = Some(clamp_mono_font_size(current + delta));
        self.persist_settings();
        self.refresh_appearance(cx);
    }

    fn reset_appearance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.reset_appearance();
        self.persist_settings();
        apply_theme_preference(ThemePreference::System, window, cx);
        self.refresh_appearance(cx);
        cx.set_menus(app_menus(&self.settings));
    }

    /// 切换专注模式（D68）：淡化非当前块。
    fn toggle_focus_mode(&mut self, cx: &mut Context<Self>) {
        self.settings.focus_mode = !self.settings.focus_mode;
        self.persist_settings();
        cx.set_menus(app_menus(&self.settings));
        cx.notify();
    }

    /// 切换打字机模式（D68）：编辑/移动光标时尽量将光标行居中。
    fn toggle_typewriter_mode(&mut self, cx: &mut Context<Self>) {
        self.settings.typewriter_mode = !self.settings.typewriter_mode;
        self.persist_settings();
        cx.set_menus(app_menus(&self.settings));
        self.caret_scroll_key = None;
        cx.notify();
    }

    /// 切换源码模式（代码模式，D68）：按原始 Markdown 源码显示与编辑。
    fn toggle_source_mode(&mut self, cx: &mut Context<Self>) {
        self.settings.source_mode = !self.settings.source_mode;
        self.persist_settings();
        self.layout = None;
        self.caret_scroll_key = None;
        self.scroll_to_source_top = Some(self.cursors[self.primary].caret);
        cx.set_menus(app_menus(&self.settings));
        cx.notify();
    }

    /// 开始/重启光标闪烁：立即显示，并每 500ms 翻转一次可见性。
    fn start_blink(&mut self, cx: &mut Context<Self>) {
        self.cursor_visible = true;
        self._blink_task = Self::spawn_blink_task(cx);
    }

    /// 失焦：隐藏光标并取消闪烁任务。
    fn stop_blink(&mut self, cx: &mut Context<Self>) {
        self.cursor_visible = false;
        self._blink_task = Task::ready(());
        cx.notify();
    }

    /// 光标移动/编辑/点选后重置相位，避免刚操作完光标恰好处于隐藏半周期。
    fn reset_blink(&mut self, cx: &mut Context<Self>) {
        self.cursor_visible = true;
        self._blink_task = Self::spawn_blink_task(cx);
        cx.notify();
    }

    /// 闪烁循环：实体或窗口失效时 `update` 返回 `Err`，循环自然结束。
    fn spawn_blink_task(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                let alive = this.update(cx, |view, cx| {
                    view.cursor_visible = !view.cursor_visible;
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        })
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
        let doc = &mut self.doc;
        for c in &mut self.cursors {
            let target = horizontal_step(doc, c.caret, false);
            if shift {
                if c.anchor.is_none() {
                    c.anchor = Some(c.caret);
                }
            } else {
                c.anchor = None;
            }
            c.caret = doc.snap_to_boundary(target);
        }
    }

    /// 所有光标右移一个字符边界。
    fn move_right(&mut self, shift: bool) {
        let doc = &mut self.doc;
        for c in &mut self.cursors {
            let target = horizontal_step(doc, c.caret, true);
            if shift {
                if c.anchor.is_none() {
                    c.anchor = Some(c.caret);
                }
            } else {
                c.anchor = None;
            }
            c.caret = doc.snap_to_boundary(target);
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
    ///
    /// 光标紧跟在独占图片块后面时，退格删除整张图片（视为一个整体）。
    fn delete_backward(&mut self, cx: &mut Context<Self>) -> bool {
        let mut any_deleted = false;
        for i in 0..self.cursors.len() {
            if let Some(sel) = self.cursors[i].selection() {
                let caret = sel.start;
                if self.doc.apply(Edit::Delete { range: sel }).is_ok() {
                    let c = &mut self.cursors[i];
                    c.caret = self.doc.snap_to_boundary(caret);
                    c.anchor = None;
                    c.marked = None;
                    any_deleted = true;
                }
            } else {
                let caret = self.cursors[i].caret;
                // 光标在图片块末尾：整块删除（`![...](...)` 作为一个整体）
                let image_range = self
                    .doc
                    .projection()
                    .block_widgets()
                    .iter()
                    .find(|w| {
                        matches!(w.kind, BlockWidgetKind::Image { .. })
                            && w.source_range.end == caret
                    })
                    .map(|w| w.source_range.clone());
                if let Some(range) = image_range {
                    if self
                        .doc
                        .apply(Edit::Delete {
                            range: range.clone(),
                        })
                        .is_ok()
                    {
                        let c = &mut self.cursors[i];
                        c.caret = self.doc.snap_to_boundary(range.start);
                        c.anchor = None;
                        c.marked = None;
                        any_deleted = true;
                    }
                    continue;
                }
                let prev = self.doc.prev_char_boundary(caret);
                if prev != caret && self.doc.apply(Edit::Delete { range: prev..caret }).is_ok() {
                    let c = &mut self.cursors[i];
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
    ///
    /// 光标紧贴在独占图片块前面时，Delete 删除整张图片（视为一个整体）。
    fn delete_forward(&mut self, cx: &mut Context<Self>) -> bool {
        let mut any_deleted = false;
        for i in 0..self.cursors.len() {
            if let Some(sel) = self.cursors[i].selection() {
                let range = sel;
                if !range.is_empty()
                    && self
                        .doc
                        .apply(Edit::Delete {
                            range: range.clone(),
                        })
                        .is_ok()
                {
                    let c = &mut self.cursors[i];
                    c.caret = self.doc.snap_to_boundary(range.start);
                    c.anchor = None;
                    c.marked = None;
                    any_deleted = true;
                }
                continue;
            }
            let caret = self.cursors[i].caret;
            // 光标在图片块开头：整块删除（`![...](...)` 作为一个整体）
            let image_range = self
                .doc
                .projection()
                .block_widgets()
                .iter()
                .find(|w| {
                    matches!(w.kind, BlockWidgetKind::Image { .. }) && w.source_range.start == caret
                })
                .map(|w| w.source_range.clone());
            if let Some(range) = image_range {
                if self
                    .doc
                    .apply(Edit::Delete {
                        range: range.clone(),
                    })
                    .is_ok()
                {
                    let c = &mut self.cursors[i];
                    c.caret = self.doc.snap_to_boundary(range.start);
                    c.anchor = None;
                    c.marked = None;
                    any_deleted = true;
                }
                continue;
            }
            let next = self.doc.next_char_boundary(caret);
            if caret != next && self.doc.apply(Edit::Delete { range: caret..next }).is_ok() {
                let c = &mut self.cursors[i];
                c.caret = self.doc.snap_to_boundary(caret);
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

    /// 回车：走核心 [`DefaultIntentMapper`] 的 `Newline` 意图。
    ///
    /// - 列表/任务项内：续行并补回标记（有序项序号 +1、任务项补未勾选框），
    ///   新行立刻显示项目符号/复选框——"提前显示对应项的前缀图标"；
    /// - 空列表项上再回车：删掉标记退出列表（即"两次回车结束列表"）；
    /// - 其它情况：普通换行。
    ///
    /// 有选区时按普通"覆盖为新行"处理。多光标按位置**从后往前**应用，
    /// 避免前面的插入改变后面光标对应的偏移。
    fn insert_newline(&mut self, cx: &mut Context<Self>) {
        let mut order: Vec<usize> = (0..self.cursors.len()).collect();
        order.sort_by_key(|&i| {
            let c = &self.cursors[i];
            Reverse(c.selection().map_or(c.caret, |r| r.end))
        });
        for i in order {
            let c = &mut self.cursors[i];
            let range = c.selection().unwrap_or(c.caret..c.caret);
            let at = range.start;
            let (applied, caret) = if range.is_empty() {
                let edits = DefaultIntentMapper.map(&Intent::Newline { at }, &self.doc);
                let mut applied = true;
                for e in &edits {
                    if self.doc.apply(e.clone()).is_err() {
                        applied = false;
                        break;
                    }
                }
                let caret = match edits.as_slice() {
                    [Edit::Insert { text, .. }] => at + text.len(),
                    [Edit::Replace { range, .. }] => range.start,
                    _ => at,
                };
                (applied, caret)
            } else {
                let applied = self
                    .doc
                    .apply(Edit::Replace {
                        range,
                        text: "\n".into(),
                    })
                    .is_ok();
                (applied, at + 1)
            };
            if applied {
                c.caret = self.doc.snap_to_boundary(caret);
                c.anchor = None;
                c.marked = None;
            }
        }
        cx.notify();
    }

    /// 表格单元格内 `Shift+Enter`：插入零宽 HTML `<br>` 强制换行。
    fn insert_table_break(&mut self, cx: &mut Context<Self>) -> bool {
        if self.cursors.len() != 1 {
            return false;
        }
        let cursor = &self.cursors[0];
        let range = cursor.selection().unwrap_or(cursor.caret..cursor.caret);
        let (line_start, line_end, is_table) = {
            let projection = self.doc.projection();
            let line_idx = projection.line_of_source(range.start);
            let line = &projection.lines[line_idx];
            (
                line.source_range.start,
                line.source_range.end,
                matches!(
                    line.block.table_line,
                    Some(TableLine::Header | TableLine::Body)
                ),
            )
        };
        if !is_table || range.end > line_end {
            return false;
        }
        let source = self.doc.text();
        let line_src = source.get(line_start..line_end).unwrap_or_default();
        let cells = table_cells(line_src);
        let start = range.start.saturating_sub(line_start);
        let end = range.end.saturating_sub(line_start);
        let Some(cell) = cell_index_at(&cells, start) else {
            return false;
        };
        if cell_index_at(&cells, end) != Some(cell) {
            return false;
        }
        self.apply_edit(
            Edit::Replace {
                range: range.clone(),
                text: "<br>".to_string(),
            },
            range.start + 4,
            cx,
        );
        true
    }

    /// 光标在表格内时，Tab/Shift+Tab 在单元格之间跳转。
    ///
    /// 只在单光标、无选区时接管；到达表格首/末单元格时**吞掉按键**（不退回
    /// "插入 4 个空格"，否则会在单元格里写入看不见的空白）。返回值表示是否已处理。
    fn table_tab(&mut self, backward: bool, cx: &mut Context<Self>) -> bool {
        if self.cursors.len() != 1 || self.cursors[0].selection().is_some() {
            return false;
        }
        let caret = self.cursors[0].caret;
        let (line_idx, line_start, line_end, role) = {
            let projection = self.doc.projection();
            let line_idx = projection.line_of_source(caret);
            let line = &projection.lines[line_idx];
            let Some(role) = line.block.table_line else {
                return false;
            };
            (
                line_idx,
                line.source_range.start,
                line.source_range.end,
                role,
            )
        };
        let source = self.doc.text();
        let line_src = source.get(line_start..line_end).unwrap_or_default();
        let cells = table_cells(line_src);
        let rel = caret.saturating_sub(line_start).min(line_src.len());
        let Some(cell_idx) = cell_index_at(&cells, rel) else {
            return false;
        };
        // 光标在首格之前（行首竖线/缩进）时，Tab 应进首格；在末格之后则反向进末格。
        let before_first = cells.first().is_some_and(|r| rel < r.start);
        let after_last = cells.last().is_some_and(|r| rel > r.end);
        if before_first && !backward {
            let target = line_start + cells[0].start;
            let c = &mut self.cursors[0];
            c.caret = self.doc.snap_to_boundary(target);
            c.anchor = None;
            c.marked = None;
            cx.notify();
            return true;
        }
        if after_last && backward {
            let target = line_start + cells.last().unwrap().start;
            let c = &mut self.cursors[0];
            c.caret = self.doc.snap_to_boundary(target);
            c.anchor = None;
            c.marked = None;
            cx.notify();
            return true;
        }

        // 1) 同一行内还能移动
        let same_line = if backward {
            cell_idx.checked_sub(1).and_then(|i| cells.get(i))
        } else {
            cells.get(cell_idx + 1)
        };
        let mut target: Option<usize> = same_line.map(|r| line_start + r.start);

        // 2) 跨行：跳过表格分隔行（其显示为空、无法落光标），到相邻行的首/末单元格
        if target.is_none() && role != TableLine::Delimiter {
            let mut i = line_idx;
            loop {
                if backward {
                    if i == 0 {
                        break;
                    }
                    i -= 1;
                } else {
                    i += 1;
                }
                let Some(line) = self.doc.projection().lines.get(i) else {
                    break;
                };
                match line.block.table_line {
                    None => break,
                    Some(TableLine::Delimiter) => continue,
                    Some(_) => {
                        let start = line.source_range.start;
                        let end = line.source_range.end;
                        let src = source.get(start..end).unwrap_or_default();
                        let cells = table_cells(src);
                        let cell = if backward {
                            cells.last()
                        } else {
                            cells.first()
                        };
                        target = cell.map(|r| start + r.start);
                        break;
                    }
                }
            }
        }

        // 到了表格边界：吞掉按键，避免把 Tab 当缩进写进单元格。
        let Some(target) = target else {
            return true;
        };
        let c = &mut self.cursors[0];
        c.caret = self.doc.snap_to_boundary(target);
        c.anchor = None;
        c.marked = None;
        cx.notify();
        true
    }

    /// Tab/Shift+Tab 动作入口：查找条/命令面板/重命名打开时沿用各自语义，
    /// 否则在正文里缩进/反缩进（见 `insert_tab` / `outdent`）。
    fn handle_tab_action(&mut self, outdent: bool, cx: &mut Context<Self>) {
        if self.renaming.is_some() || self.cmd_palette.open {
            return;
        }
        if self.find.open {
            if !outdent {
                self.find.focus_replace = !self.find.focus_replace;
                cx.notify();
            }
            return;
        }
        if self.table_tab(outdent, cx) {
            return;
        }
        if outdent {
            self.outdent(cx);
        } else {
            self.insert_tab(cx);
        }
    }

    /// Tab：无选区时在光标处插入 4 个空格；有选区时把选中的每一行整体缩进 4 格。
    ///
    /// 代码块与正文都适用——代码块里缩进是刚需，正文里也让 Tab 保持"插入缩进"
    /// （Root 的默认 Tab 是焦点切换，会吞掉按键，D44）。
    fn insert_tab(&mut self, cx: &mut Context<Self>) {
        if self.cursors.iter().all(|c| c.selection().is_none()) {
            for c in &mut self.cursors {
                let at = c.caret;
                if self
                    .doc
                    .apply(Edit::Replace {
                        range: at..at,
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
            return;
        }
        self.indent_lines(false, cx);
    }

    /// Shift+Tab：把光标所在行（或选中的每一行）去掉最多 4 个前导空格，或一个 `\t`。
    fn outdent(&mut self, cx: &mut Context<Self>) {
        self.indent_lines(true, cx);
    }

    /// 按行整体缩进/反缩进所有光标覆盖的行。
    ///
    /// 多个光标取各自行区间后合并重叠部分，再**从后往前**应用替换，保证前面的偏移
    /// 仍然有效；光标/锚点按每行前缀的增删量重新映射（落在被删空格内的夹到行首）。
    fn indent_lines(&mut self, outdent: bool, cx: &mut Context<Self>) {
        let text = self.doc.text();
        let mut blocks: Vec<Range<usize>> = Vec::new();
        for c in &self.cursors {
            let sel = c.selection().unwrap_or(c.caret..c.caret);
            let start = text[..sel.start].rfind('\n').map_or(0, |i| i + 1);
            let end = text[sel.end..]
                .find('\n')
                .map_or(text.len(), |i| sel.end + i);
            blocks.push(start..end);
        }
        blocks.sort_by_key(|r| r.start);
        let mut merged: Vec<Range<usize>> = Vec::new();
        for r in blocks {
            if let Some(last) = merged.last_mut()
                && r.start <= last.end + 1
            {
                last.end = last.end.max(r.end);
                continue;
            }
            merged.push(r);
        }

        for range in merged.into_iter().rev() {
            let block = &text[range.clone()];
            let lines: Vec<&str> = block.split('\n').collect();
            let mut new = String::with_capacity(block.len() + lines.len() * 4);
            // 每行被移除的前导字节数（缩进时为 0，统一用前缀增量计算新行首）
            let mut removed: Vec<usize> = Vec::with_capacity(lines.len());
            let mut old_starts: Vec<usize> = Vec::with_capacity(lines.len());
            let mut off = range.start;
            for (i, line) in lines.iter().enumerate() {
                if i > 0 {
                    new.push('\n');
                }
                old_starts.push(off);
                off += line.len() + 1;
                if outdent {
                    let n = if let Some(rest) = line.strip_prefix('\t') {
                        line.len() - rest.len()
                    } else {
                        line.bytes().take_while(|b| *b == b' ').take(4).count()
                    };
                    removed.push(n);
                    new.push_str(&line[n..]);
                } else {
                    removed.push(0);
                    new.push_str("    ");
                    new.push_str(line);
                }
            }
            let delta = new.len() as isize - block.len() as isize;
            if self
                .doc
                .apply(Edit::Replace {
                    range: range.clone(),
                    text: new,
                })
                .is_err()
            {
                continue;
            }
            // 每行的新行首 = 旧行首 + 之前所有行的前缀增量
            let mut new_starts: Vec<usize> = Vec::with_capacity(lines.len());
            let mut shift: isize = 0;
            for (k, &ls) in old_starts.iter().enumerate() {
                new_starts.push((ls as isize + shift) as usize);
                shift += if outdent { -(removed[k] as isize) } else { 4 };
            }
            let map = |o: usize| -> usize {
                if o >= range.end {
                    return (o as isize + delta) as usize;
                }
                if o < range.start {
                    return o;
                }
                let k = old_starts.iter().rposition(|&ls| ls <= o).unwrap_or(0);
                if outdent {
                    if o <= old_starts[k] + removed[k] {
                        new_starts[k]
                    } else {
                        new_starts[k] + (o - old_starts[k] - removed[k])
                    }
                } else {
                    new_starts[k] + (o - old_starts[k])
                }
            };
            for c in &mut self.cursors {
                c.caret = self.doc.snap_to_boundary(map(c.caret));
                c.anchor = c.anchor.map(|a| self.doc.snap_to_boundary(map(a)));
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

    /// 粘贴：图片文件保留路径，内存图片转 data URI，其余按纯文本处理。
    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx
            .read_from_clipboard()
            .as_ref()
            .and_then(clipboard_markdown)
        else {
            return;
        };
        self.insert_text_at_cursors(&text, cx);
    }

    /// 在全部光标处插入同一段文本（粘贴与拖入文件链接共用）。
    ///
    /// 按光标**位置倒序**处理，避免靠前插入使靠后光标偏移失效；空文本不动。
    fn insert_text_at_cursors(&mut self, text: &str, cx: &mut Context<Self>) {
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
                    text: text.to_string(),
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
        // 下一次 prepaint 把目标行顶边对齐视口顶部（普通光标移动仍走最小滚动）。
        self.scroll_to_source_top = Some(src);
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

    // ==================== 全工作区搜索（Cmd+Shift+F，D40）====================

    /// 展开全工作区搜索面板；同时收起命令面板/文档内查找，避免多层浮层抢键盘。
    fn open_workspace_search(&mut self, cx: &mut Context<Self>) {
        self.cmd_palette.open = false;
        self.find.open = false;
        self.workspace_search.open = true;
        self.workspace_search.selected = 0;
        self.recompute_workspace_hits();
        cx.notify();
    }

    /// 收起全工作区搜索面板（保留查询与结果，便于再次展开）。
    fn close_workspace_search(&mut self, cx: &mut Context<Self>) {
        self.workspace_search.open = false;
        cx.notify();
    }

    /// 按当前查询重扫工作区；结果上限 `MAX_WORKSPACE_HITS`，超出标记截断。
    ///
    /// 遍历与读取都走本地文件系统（工作区 MVP 只有 `FileSystemProvider`），
    /// 非 UTF-8/读取失败的文件计入 `skipped` 后跳过，不影响其它文件。
    fn recompute_workspace_hits(&mut self) {
        /// 单次搜索最多保留的命中数，避免超大型工作区把 UI 卡死。
        const MAX_WORKSPACE_HITS: usize = 500;

        let query = self.workspace_search.query.clone();
        let cs = self.workspace_search.case_sensitive;
        self.workspace_search.hits.clear();
        self.workspace_search.file_count = 0;
        self.workspace_search.skipped = 0;
        self.workspace_search.truncated = false;
        if query.is_empty() {
            self.workspace_search.selected = 0;
            return;
        }
        let Some(ws) = self.workspace.as_ref() else {
            self.workspace_search.selected = 0;
            return;
        };
        let root = ws.store().root().to_path_buf();
        let mut rels = Vec::new();
        collect_markdown_paths(ws, &DocPath::root(), &mut rels);

        // 先在局部变量里累积，最后一次性写回，避免同时借用 `workspace` 与 `workspace_search`。
        let mut hits: Vec<WorkspaceHit> = Vec::new();
        let mut file_count = 0usize;
        let mut skipped = 0usize;
        let mut truncated = false;
        let mut last_rel: Option<String> = None;
        for rel in rels {
            if hits.len() >= MAX_WORKSPACE_HITS {
                truncated = true;
                break;
            }
            let path = root.join(&rel);
            let Ok(text) = std::fs::read_to_string(&path) else {
                skipped += 1;
                continue;
            };
            let file_hits = search_text_lines(&text, &query, cs);
            if file_hits.is_empty() {
                continue;
            }
            if last_rel.as_deref() != Some(rel.as_str()) {
                file_count += 1;
                last_rel = Some(rel.clone());
            }
            for hit in file_hits {
                if hits.len() >= MAX_WORKSPACE_HITS {
                    truncated = true;
                    break;
                }
                hits.push(WorkspaceHit {
                    rel: rel.clone(),
                    path: path.clone(),
                    line_no: hit.line_no,
                    line: hit.line,
                    range: hit.range,
                    source: hit.source,
                });
            }
        }
        self.workspace_search.hits = hits;
        self.workspace_search.file_count = file_count;
        self.workspace_search.skipped = skipped;
        self.workspace_search.truncated = truncated;
        if self.workspace_search.selected >= self.workspace_search.hits.len() {
            self.workspace_search.selected = 0;
        }
    }

    /// 打开第 `idx` 条命中：切到对应标签/文件，选中命中文本并把该行滚到视口顶部。
    fn open_workspace_hit(&mut self, idx: usize, cx: &mut Context<Self>) {
        let Some(hit) = self.workspace_search.hits.get(idx).cloned() else {
            return;
        };
        self.workspace_search.open = false;
        // `open_path` 对已打开文件走 `switch_tab`，之后 `self.doc` 即目标文档。
        self.open_path(hit.path, cx);
        let start = self.doc.snap_to_boundary(hit.source.start);
        let end = self.doc.snap_to_boundary(hit.source.end);
        self.cursors = vec![Cursor {
            caret: end,
            anchor: Some(start),
            marked: None,
        }];
        self.primary = 0;
        self.selecting = false;
        self.scroll_to_source_top = Some(start);
        self.reset_blink(cx);
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

    /// 命中独占行媒体：返回 `(key, seek 比例)`；点进度条时带比例，其余为播放/暂停。
    fn media_hit(&self, position: Point<Pixels>) -> Option<(String, Option<f32>)> {
        let layout = self.layout.as_ref()?;
        for block in &layout.blocks {
            let Some(media) = &block.media else {
                continue;
            };
            if media.kind == MediaKind::Video
                && media.duration > 0.0
                && media.progress.contains(&position)
            {
                let fraction = ((position.x - media.progress.origin.x)
                    / media.progress.size.width.max(px(1.)))
                .clamp(0.0, 1.0);
                return Some((media.key.clone(), Some(fraction)));
            }
            if media.rect.contains(&position) {
                return Some((media.key.clone(), None));
            }
        }
        None
    }

    /// 点击媒体：首次点击懒创建播放器并播放；再次点击播放/暂停；点进度条 seek。
    ///
    /// 命中媒体时返回 `true`，调用方应停止后续“放置光标”逻辑，避免点播放器时
    /// 光标跳到源码行。
    fn toggle_media_at(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) -> bool {
        let Some((key, seek)) = self.media_hit(position) else {
            return false;
        };
        let Some((kind, src)) = self.layout.as_ref().and_then(|layout| {
            layout.blocks.iter().find_map(|b| {
                b.media
                    .as_ref()
                    .filter(|m| m.key == key)
                    .map(|m| (m.kind, m.src.clone()))
            })
        }) else {
            return false;
        };
        if let Some(fraction) = seek {
            let sought =
                if let Some(player) = self.media.get_mut(&key).and_then(|p| p.player.as_mut()) {
                    player.seek_fraction(fraction);
                    true
                } else {
                    false
                };
            // 暂停时也在 seek 后轮询一次，让视频输出拿到目标位置的新帧；播放中则
            // 替换为新的持续轮询任务。
            if sought {
                self.start_media_poll(key, cx);
            }
            cx.notify();
            return true;
        }
        if !self.media.contains_key(&key) {
            let playback = match media::MediaPlayer::open(kind, &src, self.path.as_deref()) {
                Ok(player) => MediaPlayback {
                    player: Some(player),
                    kind,
                    task: None,
                    error: None,
                },
                Err(error) => MediaPlayback {
                    player: None,
                    kind,
                    task: None,
                    error: Some(error),
                },
            };
            self.media.insert(key.clone(), playback);
        }
        let mut start = false;
        if let Some(playback) = self.media.get_mut(&key) {
            if let Some(player) = playback.player.as_mut() {
                player.toggle();
                start = player.playing();
            }
            if !start {
                // drop 轮询任务即取消循环。
                playback.task.take();
            }
        }
        if start {
            self.start_media_poll(key, cx);
        }
        cx.notify();
        true
    }

    /// 启动媒体轮询：视频约 33ms 取帧，音频约 100ms 更新进度；暂停/出错后退出。
    fn start_media_poll(&mut self, key: String, cx: &mut Context<Self>) {
        let interval = match self.media.get(&key).map(|p| p.kind) {
            Some(MediaKind::Video) => 33,
            Some(MediaKind::Audio) => 100,
            None => return,
        };
        let poll_key = key.clone();
        let task = cx.spawn(async move |this, cx| {
            let mut idle_polls = 0u8;
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(interval))
                    .await;
                let alive = this.update(cx, |view, cx| {
                    let Some(playback) = view.media.get_mut(&poll_key) else {
                        return false;
                    };
                    let Some(player) = playback.player.as_mut() else {
                        return false;
                    };
                    let changed = player.poll();
                    if changed || player.loading() {
                        cx.notify();
                    }
                    player.playing()
                });
                match alive {
                    Ok(true) => idle_polls = 0,
                    // 暂停 seek 后 AVPlayerItemVideoOutput 可能需要数帧才给出目标画面；
                    // 短暂续轮询，避免只查一次就留下旧帧。
                    Ok(false) if idle_polls < 4 => idle_polls += 1,
                    _ => break,
                }
            }
        });
        if let Some(playback) = self.media.get_mut(&key) {
            playback.task = Some(task);
        }
    }

    /// 命中表格内部竖线后开始调整相邻两列宽度。
    fn start_table_resize(&mut self, position: Point<Pixels>) -> bool {
        let handle = self.layout.as_ref().and_then(|layout| {
            layout
                .table_resize_handles
                .iter()
                .filter(|handle| handle.rect.contains(&position))
                .min_by_key(|handle| handle.kind.priority())
                .cloned()
        });
        let Some(handle) = handle else {
            return false;
        };
        self.table_resizing = Some(TableResizeState {
            handle,
            start: position,
        });
        self.selecting = false;
        true
    }

    /// 拖动表格列边界：只调整左右相邻列，保持两列总宽不变。
    fn update_table_resize(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) -> bool {
        let Some(resize) = self.table_resizing.as_ref() else {
            return false;
        };
        let dx = position.x - resize.start.x;
        let dy = position.y - resize.start.y;
        match resize.handle.kind {
            TableResizeKind::Column(column) => {
                let Some(widths) = resized_table_columns(&resize.handle.widths, column, dx) else {
                    return false;
                };
                self.table_column_widths.insert(resize.handle.table, widths);
            }
            TableResizeKind::RightEdge => {
                let Some(widths) =
                    resized_table_right_edge(&resize.handle.widths, dx, resize.handle.max_width)
                else {
                    return false;
                };
                self.table_column_widths.insert(resize.handle.table, widths);
            }
            TableResizeKind::RowEdge => {
                let height = resized_table_row_height(
                    resize.handle.row_height,
                    dy,
                    resize.handle.row_min_height,
                );
                self.table_row_heights.insert(resize.handle.row, height);
            }
            TableResizeKind::Corner => {
                let Some(widths) =
                    resized_table_right_edge(&resize.handle.widths, dx, resize.handle.max_width)
                else {
                    return false;
                };
                let height = resized_table_row_height(
                    resize.handle.row_height,
                    dy,
                    resize.handle.row_min_height,
                );
                self.table_column_widths.insert(resize.handle.table, widths);
                self.table_row_heights.insert(resize.handle.row, height);
            }
        }
        cx.notify();
        true
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
        if let Some((row, col)) = layout.hit(position) {
            let block = &layout.blocks[row];
            // 渲染态的独占图片：点击左半定位到图片最前面，点击右半定位到最后面，
            // 让光标落在图片语法边界而非进入字符级编辑位置（编辑态 raw_src 仍按原文命中）。
            if let Some(image) = &block.image
                && block.raw_src.is_none()
            {
                let image_x = layout.origin.x + block.indent;
                let mid = image_x + image.size.width / 2.0;
                let src = if position.x <= mid {
                    block.proj.source_range.start
                } else {
                    block.proj.source_range.end
                };
                return Some(self.doc.snap_to_boundary(src));
            }
            // 显示字符列 → 源字节偏移（空行等无显示文本的行落到行首）
            let src = self.doc.snap_to_boundary(block.source_of_col(col));
            return Some(src);
        }
        // 拖拽到视口外时：钳到最近的可见行首/行尾，保证能继续扩选
        if layout.blocks.is_empty() {
            return None;
        }
        let rel_y = position.y - layout.origin.y;
        if rel_y <= Pixels::ZERO {
            // 拖到最上方：落到文首
            let first = &layout.blocks[0];
            let src = self.doc.snap_to_boundary(first.source_of_col(0));
            return Some(src);
        }
        if rel_y >= layout.total_height() {
            // 拖到最下方：落到文末
            let last_idx = layout.blocks.len() - 1;
            let last_row = *layout.row_starts.last().unwrap_or(&0);
            let rel_x =
                (position.x - layout.origin.x - layout.blocks[last_idx].indent).max(Pixels::ZERO);
            let local_y = layout.row_height(last_row) - layout.line_height / 2.;
            if let Some(col) = layout.blocks[last_idx].col_of_point(rel_x, local_y) {
                let src = self
                    .doc
                    .snap_to_boundary(layout.blocks[last_idx].source_of_col(col));
                return Some(src);
            }
            return Some(self.doc.snap_to_boundary(self.doc.len()));
        }
        None
    }

    /// 双击：选中光标处的**词**（用核心 `Document::word_range_at`，支持 ASCII/CJK/符号）。
    fn select_word_at(&mut self, src: usize) {
        let range = self.doc.word_range_at(src);
        if range.start == range.end {
            // 不在词内（空白）：退回到单击行为，不做特殊选区
            return;
        }
        let mut c = Cursor::new(range.end);
        c.anchor = Some(range.start);
        self.cursors = vec![c];
        self.primary = 0;
    }

    /// 三击：选中整行（不含行尾换行符）。
    fn select_line_at(&mut self, src: usize) {
        let range = self.doc.line_range_of(src);
        // 选中整行文本（换行符留在外面，粘贴时更符合常用编辑器）
        let mut c = Cursor::new(range.end);
        c.anchor = Some(range.start);
        self.cursors = vec![c];
        self.primary = 0;
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

    /// 命中坐标处的脚注标签，返回跳转目标源偏移（D69；Cmd+点击用）。
    ///
    /// 只认投影里 `RunStyle::Footnote` 的片段（标签重编号前是 `[^label]` 的 label）：
    /// 引用 → 定义块起点，定义标签 → 首个引用；其余位置返回 `None`，让普通点击放光标。
    fn footnote_target_at(&self, position: Point<Pixels>) -> Option<usize> {
        let layout = self.layout.as_ref()?;
        let (row, col) = layout.hit(position)?;
        let block = &layout.blocks[row];
        let run = block.proj.runs.iter().find(|run| {
            run.style == RunStyle::Footnote
                && col >= run.display_start
                && col < run.display_start + run.text.chars().count()
        })?;
        let src = block.source_of_col(col);
        footnote_jump(&self.doc.footnote_index(), &run.text, src)
    }

    /// 导出当前文档为 PDF 并保存到文件。
    ///
    /// 异步保存对话框（`AsyncFileDialog` + `cx.spawn`，D41），避免同步对话框嵌套泵
    /// runloop 触发 gpui 键盘布局回调的 `RefCell` 争用。
    fn export_pdf(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.begin_dialog(cx);
        let markdown = self.doc.text().to_string();
        cx.spawn(async move |this, cx| {
            let picked = AsyncFileDialog::new()
                .set_title("导出 PDF")
                .add_filter("PDF 文件", &["pdf"])
                .set_file_name("document.pdf")
                .save_file()
                .await;
            let _ = this.update(cx, |view, cx| {
                view.dialog_open = false;
                cx.notify();
            });
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
        let theme_hint = |preference: ThemePreference| {
            if self.settings.theme == preference {
                "当前"
            } else {
                ""
            }
        };
        let font_size: f32 = self.font_size.into();
        let mono_font_size: f32 = self.mono_font_size.into();
        vec![
            mk("新建标签页", "Cmd+T", NewTab),
            mk("关闭标签页", "Cmd+W", CloseTab),
            mk("关闭全部标签页", "Cmd+Shift+W", CloseAllTabs),
            mk("打开文件…", "Cmd+O", OpenFile),
            mk("新建 Markdown", "", NewFile),
            mk("新建文件夹", "", NewFolder),
            mk("保存", "Cmd+S", Save),
            mk("另存为…", "Cmd+Shift+S", SaveAs),
            mk("导出 PDF", "Cmd+E", ExportPdf),
            mk("查找/替换", "Cmd+F", Find),
            mk("全工作区搜索", "Cmd+Shift+F", WorkspaceSearch),
            mk("切换文件树", "", ToggleTree),
            mk("切换大纲", "Cmd+Shift+O", ToggleOutline),
            mk(
                if self.settings.focus_mode {
                    "专注模式：关闭"
                } else {
                    "专注模式：开启"
                },
                "Cmd+Shift+J",
                ToggleFocusMode,
            ),
            mk(
                if self.settings.typewriter_mode {
                    "打字机模式：关闭"
                } else {
                    "打字机模式：开启"
                },
                "Cmd+Shift+K",
                ToggleTypewriterMode,
            ),
            mk(
                if self.settings.source_mode {
                    "源码模式（代码模式）：关闭"
                } else {
                    "源码模式（代码模式）：开启"
                },
                "Cmd+/",
                ToggleSourceMode,
            ),
            mk("移动当前文件…", "", MoveCurrentFile),
            mk(
                "主题：跟随系统",
                theme_hint(ThemePreference::System),
                SetTheme(ThemePreference::System),
            ),
            mk(
                "主题：浅色",
                theme_hint(ThemePreference::Light),
                SetTheme(ThemePreference::Light),
            ),
            mk(
                "主题：深色",
                theme_hint(ThemePreference::Dark),
                SetTheme(ThemePreference::Dark),
            ),
            mk(
                &format!("正文字体：{}", self.settings.ui_font_label()),
                "切换",
                CycleUiFont,
            ),
            mk(
                &format!("等宽字体：{}", self.settings.mono_font_label()),
                "切换",
                CycleMonoFont,
            ),
            mk(
                &format!("增大正文字号（{font_size:.0} px）"),
                "Cmd+=",
                FontSizeUp,
            ),
            mk(
                &format!("减小正文字号（{font_size:.0} px）"),
                "Cmd+-",
                FontSizeDown,
            ),
            mk(
                &format!("增大等宽字号（{mono_font_size:.0} px）"),
                "Cmd+Option+=",
                MonoFontSizeUp,
            ),
            mk(
                &format!("减小等宽字号（{mono_font_size:.0} px）"),
                "Cmd+Option+-",
                MonoFontSizeDown,
            ),
            mk("重置外观设置", "Cmd+Option+0", ResetAppearance),
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
            NewFile => self.new_markdown_in_root(cx),
            NewFolder => self.new_folder_in_root(cx),
            Save => self.save_current(window, cx),
            SaveAs => self.save_as(window, cx),
            ExportPdf => self.export_pdf(window, cx),
            Find => self.open_find(cx),
            WorkspaceSearch => self.open_workspace_search(cx),
            ToggleTree => {
                self.tree_open = !self.tree_open;
                cx.notify();
            }
            ToggleOutline => {
                self.outline_open = !self.outline_open;
                cx.notify();
            }
            ToggleFocusMode => self.toggle_focus_mode(cx),
            ToggleTypewriterMode => self.toggle_typewriter_mode(cx),
            ToggleSourceMode => self.toggle_source_mode(cx),
            MoveCurrentFile => self.move_current_file_dialog(window, cx),
            SetTheme(preference) => self.set_theme_preference(preference, window, cx),
            CycleUiFont => self.cycle_ui_font(cx),
            CycleMonoFont => self.cycle_mono_font(cx),
            FontSizeUp => self.adjust_font_size(FONT_STEP, cx),
            FontSizeDown => self.adjust_font_size(-FONT_STEP, cx),
            MonoFontSizeUp => self.adjust_mono_font_size(FONT_STEP, cx),
            MonoFontSizeDown => self.adjust_mono_font_size(-FONT_STEP, cx),
            ResetAppearance => self.reset_appearance(window, cx),
        }
    }

    fn run_menu_command(
        &mut self,
        command: MenuCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command {
            MenuCommand::Quit => {
                cx.quit();
                return;
            }
            MenuCommand::Hide => {
                cx.hide();
                return;
            }
            MenuCommand::HideOthers => {
                cx.hide_other_apps();
                return;
            }
            MenuCommand::ShowAll => {
                cx.unhide_other_apps();
                return;
            }
            MenuCommand::Minimize => {
                window.minimize_window();
                return;
            }
            MenuCommand::Zoom => {
                window.zoom_window();
                return;
            }
            MenuCommand::ToggleFullscreen => {
                window.toggle_fullscreen();
                return;
            }
            MenuCommand::About => {
                self.begin_dialog(cx);
                let dialog = rfd::AsyncMessageDialog::new()
                    .set_title("关于码札")
                    .set_description(
                        "码札 Mazha\n纯 Rust 构建的 Markdown 编辑器\nAGPL-3.0-or-later",
                    );
                cx.spawn(async move |this, cx| {
                    let _ = dialog.show().await;
                    let _ = this.update(cx, |view, cx| {
                        view.dialog_open = false;
                        cx.notify();
                    });
                })
                .detach();
                return;
            }
            _ => {}
        }

        if self.dialog_open || self.renaming.is_some() {
            return;
        }
        if self.cmd_palette.open {
            if matches!(command, MenuCommand::OpenPalette) {
                self.close_palette(cx);
            }
            return;
        }
        if self.find.open {
            if matches!(command, MenuCommand::Palette(PaletteAction::Find)) {
                self.close_find(cx);
            }
            return;
        }

        match command {
            MenuCommand::Palette(action) => self.run_palette_action(action, window, cx),
            MenuCommand::Undo => {
                self.doc.undo();
                self.after_doc_mutation(cx);
            }
            MenuCommand::Redo => {
                self.doc.redo();
                self.after_doc_mutation(cx);
            }
            MenuCommand::Cut => self.cut_selection(cx),
            MenuCommand::Copy => self.copy_selection(cx),
            MenuCommand::Paste => self.paste(cx),
            MenuCommand::SelectAll => {
                let len = self.doc.len();
                self.cursors = vec![Cursor::new(len)];
                self.cursors[0].anchor = Some(0);
                self.primary = 0;
                self.selecting = false;
                cx.notify();
            }
            MenuCommand::OpenPalette => self.open_palette(cx),
            MenuCommand::CycleTheme => self.cycle_theme(window, cx),
            MenuCommand::Quit
            | MenuCommand::Hide
            | MenuCommand::HideOthers
            | MenuCommand::ShowAll
            | MenuCommand::About
            | MenuCommand::Minimize
            | MenuCommand::Zoom
            | MenuCommand::ToggleFullscreen => unreachable!(),
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
        self.begin_dialog(cx);
        cx.spawn(async move |this, cx| {
            let picked = AsyncFileDialog::new()
                .set_title("打开 Markdown")
                .add_filter("Markdown", &["md", "markdown", "txt"])
                .pick_file()
                .await;
            let _ = this.update(cx, |view, cx| {
                view.dialog_open = false;
                if let Some(handle) = picked {
                    view.open_path(handle.path().to_path_buf(), cx);
                }
                cx.notify();
            });
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

    /// 把当前编辑态写回 `tabs[active]`。
    ///
    /// `DocView` 的 `doc`/`cursors` 等字段是当前标签的工作副本，标签切换前必须先回写，
    /// 否则切走再切回会看到旧文档（这也是“切 Tab 内容不刷新”的根因之一）。
    fn sync_current_tab(&mut self) {
        // `Document` 没有实现 `Clone`（内含解析缓存/撤销栈），这里用 move 保留每个标签的
        // 撤销历史；先换成空文档，避免借用冲突。
        let doc = std::mem::replace(&mut self.doc, Document::new(""));
        let Some(t) = self.tabs.get_mut(self.active) else {
            self.doc = doc;
            return;
        };
        t.path = self.path.clone();
        t.doc = doc;
        t.cursors = self.cursors.clone();
        t.primary = self.primary;
        t.selecting = self.selecting;
        t.layout = self.layout.clone();
        // 高亮器/查找态与视口相关，不跨标签复用。
        t.highlighter = None;
        t.find = FindState::default();
        t.saved_text = self.saved_text.clone();
    }

    /// 从 `tabs[idx]` 装载编辑态到当前字段（调用方负责先回写旧标签）。
    fn activate_tab(&mut self, idx: usize, cx: &mut Context<Self>) {
        let Some(t) = self.tabs.get_mut(idx) else {
            return;
        };
        let path = t.path.clone();
        // 把目标标签的文档 move 出来，切回时撤销/重做历史仍在。
        let doc = std::mem::replace(&mut t.doc, Document::new(""));
        let cursors = t.cursors.clone();
        let primary = t.primary;
        let selecting = t.selecting;
        let saved_text = t.saved_text.clone();
        self.active = idx;
        self.path = path;
        self.doc = doc;
        self.cursors = cursors;
        self.primary = primary;
        self.selecting = selecting;
        // 布局/滚动位置依赖当前视口，切换后重建，避免沿用上一个文档的坐标。
        self.layout = None;
        self.caret_scroll_key = None;
        self.scroll_to_source_top = None;
        self.highlighter = None;
        self.find = FindState::default();
        self.saved_text = saved_text;
        self.media.clear();
        self.table_column_widths.clear();
        self.table_row_heights.clear();
        self.table_resizing = None;
        self.scroll = gpui::ScrollHandle::new();
        self.content_height = px(0.);
        cx.notify();
    }

    #[allow(dead_code)]
    fn switch_tab(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx >= self.tabs.len() || idx == self.active {
            return;
        }
        self.sync_current_tab();
        self.activate_tab(idx, cx);
    }

    #[allow(dead_code)]
    fn close_all_tabs(&mut self, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            return;
        }
        // 保留首个标签；先把当前编辑态回写，避免正在编辑的标签被旧快照覆盖。
        self.sync_current_tab();
        self.tabs.truncate(1);
        self.activate_tab(0, cx);
    }

    #[allow(dead_code)]
    fn close_current_tab(&mut self, cx: &mut Context<Self>) {
        if self.tabs.len() <= 1 {
            return;
        }
        // 关闭的是当前标签，无需回写被丢弃的状态；其余标签在切走时已回写。
        self.tabs.remove(self.active);
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
        self.activate_tab(self.active, cx);
    }

    /// 关闭指定标签页（Tab 上的关闭按钮）。
    ///
    /// 关闭非当前标签只移除快照并修正 `active` 下标，当前编辑器状态不受影响；
    /// 关闭当前标签复用 `close_current_tab`，由它激活相邻标签。
    #[allow(dead_code)]
    fn close_tab(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx >= self.tabs.len() || self.tabs.len() <= 1 {
            return;
        }
        if idx == self.active {
            self.close_current_tab(cx);
            return;
        }
        self.tabs.remove(idx);
        if self.active > idx {
            self.active -= 1;
        }
        cx.notify();
    }

    #[allow(dead_code)]
    fn new_tab_empty(&mut self, cx: &mut Context<Self>) {
        self.sync_current_tab();
        self.tabs.push(Tab::new(String::new(), None));
        self.activate_tab(self.tabs.len() - 1, cx);
    }

    #[allow(dead_code)]
    fn open_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let full = path.canonicalize().unwrap_or_else(|_| path.clone());
        // 已打开过：走统一的切换流程（会先回写当前标签，再装载目标标签）。
        let existing = self.tabs.iter().position(|t| {
            t.path
                .as_ref()
                .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
                == Some(full.clone())
        });
        if let Some(i) = existing {
            self.switch_tab(i, cx);
            return;
        }
        // 新建标签前保存当前编辑态，避免从侧栏打开新文件时旧标签丢改动。
        self.sync_current_tab();
        if let Ok(text) = std::fs::read_to_string(&full) {
            self.tabs.push(Tab::new(text, Some(full.clone())));
            self.activate_tab(self.tabs.len() - 1, cx);
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
        // 读取失败也保留一个空标签，方便用户继续新建/保存。
        self.tabs.push(Tab::new(String::new(), Some(full)));
        self.activate_tab(self.tabs.len() - 1, cx);
    }

    fn save_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.path.clone() {
            if let Err(e) = std::fs::write(&path, self.doc.text().as_bytes()) {
                eprintln!("保存失败：{}", e);
            } else {
                self.saved_text = Some(self.doc.text().to_string());
                // 当前标签标题直接从 `self` 实时读取；`tabs[active]` 在切走时统一回写，
                // 这里不能调用 `sync_current_tab`（它会把 `self.doc` move 走）。
                cx.notify();
            }
        } else {
            self.save_as(window, cx);
        }
    }

    /// 在工作区根目录新建一篇 Markdown 并打开（侧栏「＋」）。
    fn new_markdown_in_root(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.workspace_root.clone() else {
            return;
        };
        self.new_markdown_in(root, cx);
    }

    /// 在指定目录内新建一篇 Markdown 并打开（目录行「＋」）。
    ///
    /// 文件名从「未命名.md」递增，避免覆盖同名；若目标目录已展开，新文件立即可见。
    fn new_markdown_in(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        if !dir.is_dir() {
            return;
        }
        let mut name = "未命名.md".to_string();
        let mut i = 1;
        let full = loop {
            let candidate = dir.join(&name);
            if !candidate.exists() {
                break candidate;
            }
            name = format!("未命名{i}.md");
            i += 1;
            if i > 1000 {
                break candidate;
            }
        };
        if let Err(e) = std::fs::write(&full, "# 新文档\n") {
            eprintln!("新建失败：{}", e);
            return;
        }
        // 让新文件所在目录保持展开（根目录恒展开，无需记录）。
        if let Some(rel) = self.rel_of(&dir)
            && !rel.is_empty()
        {
            self.expanded_dirs.insert(rel);
        }
        self.refresh_workspace(cx);
        self.open_path(full, cx);
    }

    /// 在工作区根目录新建文件夹（侧栏「＋文件夹」）。
    fn new_folder_in_root(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.workspace_root.clone() else {
            return;
        };
        self.new_folder_in(root, cx);
    }

    /// 在指定目录内新建子文件夹，并进入重命名态（侧栏/目录行「＋文件夹」）。
    ///
    /// 名字从「新建文件夹」递增，避免覆盖；父目录保持展开，随后 `start_rename` 让用户改名。
    fn new_folder_in(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        if !dir.is_dir() {
            return;
        }
        let mut name = "新建文件夹".to_string();
        let mut i = 1;
        let full = loop {
            let candidate = dir.join(&name);
            if !candidate.exists() {
                break candidate;
            }
            name = format!("新建文件夹{i}");
            i += 1;
            if i > 1000 {
                break candidate;
            }
        };
        if let Err(e) = std::fs::create_dir(&full) {
            eprintln!("新建文件夹失败：{}", e);
            return;
        }
        // 让父目录保持展开，新目录立即可见（根目录恒展开，无需记录）。
        if let Some(rel) = self.rel_of(&dir)
            && !rel.is_empty()
        {
            self.expanded_dirs.insert(rel);
        }
        self.refresh_workspace(cx);
        self.start_rename(full, cx);
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

    /// 删除指定路径的文件或目录（文件树 CRUD；目录递归删除）。
    fn delete_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let is_dir = path.is_dir();
        let result = if is_dir {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        if let Err(e) = result {
            eprintln!("删除失败：{}", e);
            return;
        }
        // 当前打开的文档落在被删文件/目录内：清空为未保存示例，避免悬挂路径。
        let affected = self
            .path
            .as_ref()
            .is_some_and(|p| p == &path || (is_dir && p.starts_with(&path)));
        if affected {
            self.path = None;
            self.saved_text = None;
            self.layout = None;
            self.caret_scroll_key = None;
            self.highlighter = None;
        }
        // 清理已删除目录（及其子目录）的展开记录。
        if is_dir && let Some(rel) = self.rel_of(&path) {
            let prefix = format!("{rel}/");
            self.expanded_dirs
                .retain(|d| d != &rel && !d.starts_with(&prefix));
        }
        self.refresh_workspace(cx);
    }

    /// 把绝对路径换算成工作区相对路径字符串（不在工作区内则 `None`）。
    fn rel_of(&self, path: &Path) -> Option<String> {
        let root = self.workspace_root.as_ref()?;
        let rel = path.strip_prefix(root).ok()?;
        Some(rel.to_string_lossy().replace('\\', "/"))
    }

    /// 重命名指定文件/目录（同父目录下的新名字）。
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
            return;
        }
        let is_dir = target.is_dir();
        // 当前文档正好是该文件，或落在被重命名的目录内：跟随更新绝对路径。
        if let Some(cur) = self.path.clone() {
            if cur == path {
                self.path = Some(target.clone());
            } else if is_dir
                && cur.starts_with(&path)
                && let Ok(suffix) = cur.strip_prefix(&path)
            {
                self.path = Some(target.join(suffix));
            }
        }
        // 目录重命名后，把展开记录里的旧前缀迁移到新前缀，保持展开状态。
        if is_dir && let (Some(old_rel), Some(new_rel)) = (self.rel_of(&path), self.rel_of(&target))
        {
            remap_expanded(&mut self.expanded_dirs, &old_rel, &new_rel);
        }
        self.refresh_workspace(cx);
    }

    /// 移动当前文件到选定目录（命令面板入口）。
    ///
    /// 同 `open_file_dialog`：用异步 `AsyncFileDialog` + `cx.spawn`，避免同步对话框嵌套泵
    /// runloop 触发 gpui 键盘布局回调的 `RefCell` 争用（D41）。
    fn move_current_file_dialog(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let cur = self.path.clone();
        let Some(cur) = cur else { return };
        self.begin_dialog(cx);
        cx.spawn(async move |this, cx| {
            let picked = AsyncFileDialog::new()
                .set_title("移动到目录")
                .pick_folder()
                .await;
            let _ = this.update(cx, |view, cx| {
                view.dialog_open = false;
                if let Some(handle) = picked {
                    view.move_entry(cur, handle.path().to_path_buf(), cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 把文件或目录移动到目标目录（文件树拖拽 / 命令面板「移动当前文件…」）。
    ///
    /// 拒绝三种非法落点：目标就是条目自身、把目录移进自身或其后代、目标位置已有同名条目。
    /// 成功后跟随更新当前打开文档的绝对路径，并迁移 `expanded_dirs` 的旧前缀（目录）。
    fn move_entry(&mut self, from: PathBuf, target_dir: PathBuf, cx: &mut Context<Self>) {
        let Some(name) = from.file_name().map(|n| n.to_owned()) else {
            return;
        };
        let target = target_dir.join(&name);
        if target == from {
            return;
        }
        // rename 后原路径即消失，先记录是否为目录，供后续路径/展开态迁移判断。
        let from_is_dir = from.is_dir();
        if !target_dir.is_dir() {
            eprintln!("移动失败：目标不是目录");
            return;
        }
        // 目录不能移进自身或其子孙，否则会把自己搬进自己内部。
        if from_is_dir && (target_dir == from || target_dir.starts_with(&from)) {
            eprintln!("移动失败：不能把目录移动到自身或其子目录");
            return;
        }
        if target.exists() {
            eprintln!("移动失败：目标已存在");
            return;
        }
        if let Err(e) = std::fs::rename(&from, &target) {
            eprintln!("移动失败：{}", e);
            return;
        }
        // 当前文档正好是被移动条目，或落在被移动目录内：跟随更新绝对路径。
        if let Some(cur) = self.path.clone() {
            if cur == from {
                self.path = Some(target.clone());
            } else if from_is_dir
                && cur.starts_with(&from)
                && let Ok(suffix) = cur.strip_prefix(&from)
            {
                self.path = Some(target.join(suffix));
            }
        }
        // 目录移动后，把展开记录里的旧前缀迁移到新前缀，保持展开状态。
        if from_is_dir
            && let (Some(old_rel), Some(new_rel)) = (self.rel_of(&from), self.rel_of(&target))
        {
            remap_expanded(&mut self.expanded_dirs, &old_rel, &new_rel);
        }
        self.refresh_workspace(cx);
    }

    /// 把系统拖入的文件/目录导入工作区（D74：Finder → 码札）。
    ///
    /// `target_dir` 为落点目录（侧栏根目录/目录行）；`None` 时回退工作区根或当前文档父目录。
    /// `insert_links` 仅对编辑区落点为真：非 Markdown 条目导入后转成相对链接插入光标。
    /// 外部条目一律**复制**，不移动 Finder 原件；Markdown 导入完成后逐个打开为标签。
    fn import_external_paths(
        &mut self,
        paths: &[PathBuf],
        target_dir: Option<PathBuf>,
        insert_links: bool,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        // 链接的基准目录必须在打开新标签之前确定，否则会指向刚打开的文档目录。
        let link_base = self
            .path
            .as_ref()
            .and_then(|path| path.parent().map(Path::to_path_buf));
        let dir = target_dir
            .or_else(|| self.workspace_root.clone())
            .or_else(|| link_base.clone());
        let mut opened: Vec<PathBuf> = Vec::new();
        let mut linked: Vec<PathBuf> = Vec::new();
        let mut imported = false;
        if let Some(ref dir) = dir {
            if !dir.is_dir() {
                eprintln!("导入失败：目标不是目录 {}", dir.display());
                return;
            }
            for (src, dst) in plan_imports(paths, dir) {
                if let Err(e) = copy_entry(&src, &dst) {
                    eprintln!("导入失败：{} → {}：{e}", src.display(), dst.display());
                    continue;
                }
                imported = true;
                // 目录只在文件树里出现，不打开也不生成指向目录的链接；
                // 直接拖入的 Markdown 文件自动打开，其余文件生成链接。
                if src.is_file() {
                    if is_markdown_file(&dst) {
                        opened.push(dst);
                    } else {
                        linked.push(dst);
                    }
                }
            }
            if imported {
                self.refresh_workspace(cx);
            }
        } else {
            // 尚未保存任何文档：没有可复制的工作区，直接打开 Markdown / 插入绝对路径链接。
            for path in paths {
                if path.is_dir() {
                    continue;
                }
                if is_markdown_file(path) {
                    opened.push(path.clone());
                } else {
                    linked.push(path.clone());
                }
            }
        }
        if insert_links && !linked.is_empty() {
            let base = link_base.or_else(|| dir.clone());
            let text = linked
                .iter()
                .map(|path| dropped_link_markdown(path, base.as_deref()))
                .collect::<Vec<_>>()
                .join("\n");
            self.insert_text_at_cursors(&text, cx);
        }
        for path in opened {
            self.open_path(path, cx);
        }
        cx.notify();
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
        // CRUD 可能改动展开态（新建/重命名/移动/删除），一并落盘（D72）。
        self.persist_tree_expanded();
        cx.notify();
    }

    /// 当前工作区根目录的持久化键（绝对路径字符串）；无工作区时为 `None`。
    fn workspace_key(&self) -> Option<String> {
        self.workspace_root
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
    }

    /// 把当前文件树展开状态写入设置并落盘（D72）。
    fn persist_tree_expanded(&mut self) {
        let Some(key) = self.workspace_key() else {
            return;
        };
        let mut dirs: Vec<String> = self.expanded_dirs.iter().cloned().collect();
        dirs.sort();
        self.settings.set_tree_expanded_for(&key, dirs);
        self.persist_settings();
    }

    /// 收集文件树要渲染的行：`(缩进深度, 条目)`。
    ///
    /// 根目录的子项恒展开；子目录仅在其路径出现在 `expanded_dirs` 时递归展开。
    /// 只保留**目录**与 **Markdown 文档**（与 `list_markdown` 的过滤一致），
    /// 避免把图片/其它格式等无关文件混进树。
    fn file_tree_rows(&self) -> Vec<(usize, Entry)> {
        let Some(ws) = self.workspace.as_ref() else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        collect_tree_rows(ws, &DocPath::root(), 0, &self.expanded_dirs, &mut rows);
        rows
    }

    /// 切换某个目录的展开/折叠状态（文件树点击目录行）。
    fn toggle_dir(&mut self, rel: String, cx: &mut Context<Self>) {
        if !self.expanded_dirs.remove(&rel) {
            self.expanded_dirs.insert(rel);
        }
        self.persist_tree_expanded();
        cx.notify();
    }

    /// 另存为：弹原生保存对话框（经 `window.defer` 延迟，规避键盘布局回调的 `RefCell` 争用）。
    /// 另存为：异步保存对话框（`AsyncFileDialog` + `cx.spawn`，D41），避免同步对话框
    /// 嵌套泵 runloop 触发 gpui 键盘布局回调的 `RefCell` 争用。
    fn save_as(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.begin_dialog(cx);
        let text = self.doc.text().to_string();
        cx.spawn(async move |this, cx| {
            let picked = AsyncFileDialog::new()
                .set_title("另存为")
                .add_filter("Markdown", &["md", "markdown"])
                .set_file_name("document.md")
                .save_file()
                .await;
            let _ = this.update(cx, |view, cx| {
                view.dialog_open = false;
                cx.notify();
            });
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
                    // 与 `save_current` 同理：当前文档仍由 `self.doc` 持有，
                    // 标签快照等切走时再回写。
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
        // 全工作区搜索（`Cmd+Shift+F`）：悬浮面板 + 按文件分组的结果列表。
        let workspace_search_overlay = if self.workspace_search.open {
            let st = &self.workspace_search;
            let mut rows: Vec<gpui::AnyElement> = Vec::new();
            let mut last_rel: Option<&str> = None;
            for (i, hit) in st.hits.iter().enumerate() {
                if last_rel != Some(hit.rel.as_str()) {
                    rows.push(
                        div()
                            .px_3()
                            .pt_2()
                            .pb_1()
                            .text_xs()
                            .text_color(palette.muted)
                            .child(hit.rel.clone())
                            .into_any_element(),
                    );
                    last_rel = Some(hit.rel.as_str());
                }
                // 命中前后各截一段上下文，中间命中文本用浅色底强调（按字符截断）。
                let before = tail_chars(&hit.line[..hit.range.start], 36);
                let matched = hit.line[hit.range.clone()].to_string();
                let after = head_chars(&hit.line[hit.range.end..], 80);
                rows.push(
                    div()
                        .id(gpui::ElementId::Name(format!("ws-hit-{i}").into()))
                        .w_full()
                        .px_3()
                        .py_1()
                        .text_sm()
                        .flex()
                        .items_center()
                        .gap_2()
                        .when(i == st.selected, |d| d.bg(palette.selection))
                        .child(
                            div()
                                .w(px(40.))
                                .flex_none()
                                .text_xs()
                                .text_color(palette.muted)
                                .child(format!("{}", hit.line_no)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .flex()
                                .items_center()
                                .overflow_hidden()
                                .child(div().flex_none().text_color(palette.muted).child(before))
                                .child(
                                    div()
                                        .flex_none()
                                        .rounded(px(2.))
                                        .bg(palette.selection)
                                        .child(matched),
                                )
                                .child(div().flex_none().text_color(palette.muted).child(after)),
                        )
                        .on_click(cx.listener(move |view, _ev, _w, cx| {
                            view.open_workspace_hit(i, cx);
                        }))
                        .into_any_element(),
                );
            }
            let query_shown = if st.query.is_empty() {
                "输入要搜索的内容…".to_string()
            } else {
                st.query.clone()
            };
            let case_label = if st.case_sensitive {
                "Aa·开"
            } else {
                "aa·关"
            };
            let summary = if st.query.is_empty() {
                "输入查询后自动扫描工作区内的 Markdown 文件".to_string()
            } else {
                format!(
                    "{} 处 · {} 个文件{}{}",
                    st.hits.len(),
                    st.file_count,
                    if st.truncated { "（已截断）" } else { "" },
                    if st.skipped > 0 {
                        format!(" · 跳过 {} 个文件", st.skipped)
                    } else {
                        String::new()
                    }
                )
            };
            Some(
                div()
                    .id("ws-search")
                    .absolute()
                    .top(px(60.))
                    .left(px(0.))
                    .right(px(0.))
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .id("ws-search-box")
                            .w(px(620.))
                            .h(px(430.))
                            .flex()
                            .flex_col()
                            .border_1()
                            .border_color(palette.border)
                            .bg(palette.subtle)
                            .child(
                                h_flex()
                                    .id("ws-search-head")
                                    .items_center()
                                    .gap_2()
                                    .px_3()
                                    .py_2()
                                    .border_b_1()
                                    .border_color(palette.border)
                                    .child(
                                        div()
                                            .min_w(px(220.))
                                            .flex_1()
                                            .px_2()
                                            .h(px(24.))
                                            .rounded_md()
                                            .border_1()
                                            .border_color(palette.accent)
                                            .bg(palette.background)
                                            .text_sm()
                                            .child(query_shown),
                                    )
                                    .child(div().text_xs().child(case_label)),
                            )
                            .child(
                                div()
                                    .id("ws-search-results")
                                    .flex_1()
                                    .min_h(px(0.))
                                    .overflow_y_scroll()
                                    .children(rows),
                            )
                            .child(
                                div()
                                    .id("ws-search-foot")
                                    .px_3()
                                    .py_1()
                                    .border_t_1()
                                    .border_color(palette.border)
                                    .text_xs()
                                    .text_color(palette.muted)
                                    .child(format!(
                                        "{summary} · ↑↓ 选择 · Enter 打开 · Cmd+Alt+C 大小写 · Esc 关闭"
                                    )),
                            ),
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

        // 左侧文件树（只显示文件/目录，不再混入大纲）。
        // 标题栏 `sb-h` 单独作为 sidebar 的直接子节点固定在顶部（D75）：
        // 只有下面的条目列表进入滚动容器，标题与「＋文件/＋文件夹」不随列表卷走。
        let sb_header = div()
            .id("sb-h")
            .px_2()
            .h(px(28.))
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(palette.border)
            // 侧栏标题也是「拖到根目录」的落点（根目录恒展开，rel 为空串）。
            .on_drop(cx.listener(|view, drag: &TreeDrag, _w, cx| {
                if let Some(root) = view.workspace_root.clone() {
                    let from = root.join(&drag.rel);
                    view.move_entry(from, root, cx);
                }
            }))
            // 根目录落点不接受「本来就在根目录」的条目（移动无变化）；外部文件拖入恒接受。
            .can_drop(|drag, _w, _cx| {
                if let Some(d) = drag.downcast_ref::<TreeDrag>() {
                    return d.rel.contains('/');
                }
                drag.is::<ExternalPaths>()
            })
            // 拖到根目录标题时整行铺强调底色，明确当前落点（D73/D74）。
            .drag_over::<TreeDrag>(move |style, _drag, _w, _cx| style.bg(palette.selection))
            // 系统拖入的文件落到工作区根目录（D74）。
            .drag_over::<ExternalPaths>(move |style, _drag, _w, _cx| style.bg(palette.selection))
            .on_drop(cx.listener(|view, paths: &ExternalPaths, _w, cx| {
                let root = view.workspace_root.clone();
                view.import_external_paths(paths.paths(), root, false, cx);
            }))
            .child("资源管理器")
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id("sb-new-file")
                            .cursor_pointer()
                            .text_xs()
                            .hover(|d| d.text_color(palette.foreground))
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(|view, _ev, _w, cx| {
                                    view.new_markdown_in_root(cx);
                                }),
                            )
                            .child("＋文件"),
                    )
                    .child(
                        div()
                            .id("sb-new-folder")
                            .cursor_pointer()
                            .text_xs()
                            .hover(|d| d.text_color(palette.foreground))
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(|view, _ev, _w, cx| {
                                    view.new_folder_in_root(cx);
                                }),
                            )
                            .child("＋文件夹"),
                    ),
            );
        let mut sb: Vec<gpui::AnyElement> = Vec::new();
        if self.tree_open
            && let Some(ref root) = self.workspace_root
        {
            let rows = self.file_tree_rows();
            for (depth, entry) in rows {
                let name = entry.name.clone();
                let rel = entry.path.as_str().to_string();
                let full = root.join(&rel);
                // 缩进按层级：每层 +12px，基准 8px。
                let indent = (depth as f32) * 12.0 + 8.0;
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
                            .pl(px(indent))
                            .pr_3()
                            .py_1()
                            .text_sm()
                            .bg(palette.selection)
                            .child(edit_text)
                            .into_any_element(),
                    );
                    continue;
                }
                match entry.kind {
                    // 目录行：▸/▾ 切换展开；＋ 在该目录内新建 Markdown；✎ 重命名；✕ 删除。
                    EntryKind::Dir => {
                        let expanded = self.expanded_dirs.contains(&rel);
                        let arrow = if expanded { "▾" } else { "▸" };
                        let rel_toggle = rel.clone();
                        let rel_for_drop = rel.clone();
                        let full_ren = full.clone();
                        let full_del = full.clone();
                        let full_new = full.clone();
                        let full_drop = full.clone();
                        let full_external = full.clone();
                        sb.push(
                            div()
                                .id(gpui::ElementId::Name(format!("sb-dir-{rel}").into()))
                                .pl(px(indent))
                                .pr_2()
                                .py_1()
                                .text_sm()
                                .flex()
                                .justify_between()
                                .items_center()
                                // 目录行可作为拖拽源（拖动该目录）与落点（接收其它条目）。
                                .on_drag(
                                    TreeDrag {
                                        rel: rel.clone(),
                                        name: name.clone(),
                                        is_dir: true,
                                        bg: palette.subtle,
                                        fg: palette.foreground,
                                        border: palette.border,
                                    },
                                    |drag, _offset, _window, cx| {
                                        cx.new(|_| TreeDragPreview {
                                            label: drag.name.clone(),
                                            bg: drag.bg,
                                            fg: drag.fg,
                                            border: drag.border,
                                        })
                                    },
                                )
                                // 拖出窗口 → 交给系统原生文件拖拽（可跨应用/拖到 Finder/桌面）。
                                .external_drag_payload({
                                    let full = full.clone();
                                    move |_drag: &TreeDrag, _w, _cx| {
                                        Some(ExternalDragPayload::Files(FileDragPaths::new([(
                                            full.clone(),
                                            true,
                                        )])))
                                    }
                                })
                                // 拒绝自身与「把目录拖进自己的子孙」两种非法落点；外部文件恒接受。
                                .can_drop(move |drag, _w, _cx| {
                                    if let Some(d) = drag.downcast_ref::<TreeDrag>() {
                                        return drop_allowed(&d.rel, d.is_dir, &rel_for_drop);
                                    }
                                    drag.is::<ExternalPaths>()
                                })
                                // 合法落点上铺强调底色（`can_drop` 为 false 时不应用）。
                                .drag_over::<TreeDrag>(move |style, _drag, _w, _cx| {
                                    style.bg(palette.selection)
                                })
                                // 外部文件落到该目录（D74）。
                                .drag_over::<ExternalPaths>(move |style, _drag, _w, _cx| {
                                    style.bg(palette.selection)
                                })
                                .on_drop(cx.listener(move |view, drag: &TreeDrag, _w, cx| {
                                    if let Some(root) = view.workspace_root.clone() {
                                        let from = root.join(&drag.rel);
                                        view.move_entry(from, full_drop.clone(), cx);
                                    }
                                }))
                                .on_drop(cx.listener(move |view, paths: &ExternalPaths, _w, cx| {
                                    view.import_external_paths(
                                        paths.paths(),
                                        Some(full_external.clone()),
                                        false,
                                        cx,
                                    );
                                }))
                                .child(
                                    div()
                                        .id(gpui::ElementId::Name(format!("sb-dir-t-{rel}").into()))
                                        .flex_1()
                                        .min_w(px(0.))
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .cursor_pointer()
                                        .truncate()
                                        // 用 on_click（而非 on_mouse_down）：拖动该目录时不误触发折叠。
                                        .on_click(cx.listener(move |view, _ev, _w, cx| {
                                            view.toggle_dir(rel_toggle.clone(), cx);
                                        }))
                                        .child(arrow)
                                        .child(name),
                                )
                                .child(
                                    div()
                                        .cursor_pointer()
                                        .text_xs()
                                        .hover(|d| d.text_color(palette.foreground))
                                        .on_mouse_down(
                                            gpui::MouseButton::Left,
                                            cx.listener(move |view, _ev, _w, cx| {
                                                view.new_markdown_in(full_new.clone(), cx);
                                            }),
                                        )
                                        .child("＋"),
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
                                                view.delete_path(full_del.clone(), cx);
                                            }),
                                        )
                                        .child("✕"),
                                )
                                .into_any_element(),
                        );
                    }
                    // 文件行：点击打开；✎ 重命名；✕ 删除。
                    EntryKind::File => {
                        let cur = self.path.as_ref().is_some_and(|cp| cp.ends_with(&rel));
                        let full_open = full.clone();
                        let full_ren = full.clone();
                        let full_del = full.clone();
                        sb.push(
                            div()
                                .id(gpui::ElementId::Name(format!("sb-{rel}").into()))
                                .pl(px(indent))
                                .pr_2()
                                .py_1()
                                .text_sm()
                                .flex()
                                .justify_between()
                                .items_center()
                                .when(cur, |d| d.bg(palette.selected_bg))
                                // 文件行可拖拽到任意目录（含根目录标题）完成移动。
                                .on_drag(
                                    TreeDrag {
                                        rel: rel.clone(),
                                        name: name.clone(),
                                        is_dir: false,
                                        bg: palette.subtle,
                                        fg: palette.foreground,
                                        border: palette.border,
                                    },
                                    |drag, _offset, _window, cx| {
                                        cx.new(|_| TreeDragPreview {
                                            label: drag.name.clone(),
                                            bg: drag.bg,
                                            fg: drag.fg,
                                            border: drag.border,
                                        })
                                    },
                                )
                                // 拖出窗口 → 系统原生文件拖拽（跨应用/窗口外，如拖进 Finder）。
                                .external_drag_payload({
                                    let full = full.clone();
                                    move |_drag: &TreeDrag, _w, _cx| {
                                        Some(ExternalDragPayload::Files(FileDragPaths::new([(
                                            full.clone(),
                                            false,
                                        )])))
                                    }
                                })
                                .child(
                                    div()
                                        .id(gpui::ElementId::Name(format!("sb-open-{rel}").into()))
                                        .flex_1()
                                        .min_w(px(0.))
                                        // `truncate()` = nowrap + hidden + 末尾省略号（D58）。
                                        .truncate()
                                        .cursor_pointer()
                                        // 用 on_click：拖动该文件时不误触发打开。
                                        .on_click(cx.listener(move |view, _ev, _w, cx| {
                                            view.open_path(full_open.clone(), cx);
                                        }))
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
                                                view.delete_path(full_del.clone(), cx);
                                            }),
                                        )
                                        .child("✕"),
                                )
                                .into_any_element(),
                        );
                    }
                }
            }
        }

        // 右侧大纲面板（目录导航）
        let mut ol: Vec<gpui::AnyElement> = Vec::new();
        ol.push(
            div()
                .id("ol-h")
                .px_2()
                .h(px(28.))
                .items_center()
                .border_b_1()
                .border_color(palette.border)
                .child("大纲")
                .into_any_element(),
        );
        if self.outline_open {
            let items = self.outline_items();
            if items.is_empty() {
                ol.push(
                    div()
                        .id("ol-empty")
                        .px_3()
                        .py_2()
                        .text_sm()
                        .text_color(palette.muted)
                        .child("（无标题）")
                        .into_any_element(),
                );
            } else {
                for (i, (level, title, src)) in items.into_iter().enumerate() {
                    // 缩进按级别：level=1 无缩进，每级 +8px
                    let indent = ((level.saturating_sub(1)) as f32) * 8.0 + 8.0;
                    ol.push(
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
            }
        }
        let outline_panel = div()
            .id("outline")
            .w(self.outline_width)
            .flex_none()
            .h_full()
            .border_l_1()
            .border_color(palette.border)
            .bg(palette.subtle)
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .children(ol);
        let outline_resize = div()
            .id("outline-resize")
            .w(px(4.))
            .h_full()
            .flex_none()
            .cursor_col_resize()
            .hover(|d| d.bg(palette.border))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _ev, _w, cx| {
                    view.outline_resizing = true;
                    cx.notify();
                }),
            );
        let sidebar_el = div()
            .id("sidebar")
            .w(self.sidebar_width)
            .flex_none()
            .h_full()
            .border_r_1()
            .border_color(palette.border)
            .bg(palette.subtle)
            .flex()
            .flex_col()
            // 标题栏固定；列表单独滚动。`min_h(0)` 让 flex 子项可收缩到内容高以下，
            // 否则 flex 项默认按内容高度撑开，滚动容器永远不出现（D75）。
            .child(sb_header)
            .child(
                div()
                    .id("sidebar-list")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .children(sb),
            );
        // 侧栏分隔条：按下后由 `#app` 的 move/up 监听统一改宽度，避免鼠标快速移出
        // 4px 手柄后丢失拖拽（D58）。
        let sidebar_resize = div()
            .id("sidebar-resize")
            .w(px(4.))
            .h_full()
            .flex_none()
            .cursor_col_resize()
            .hover(|d| d.bg(palette.border))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _ev, _w, cx| {
                    view.sidebar_resizing = true;
                    cx.notify();
                }),
            );
        // 标签栏
        let mut tbc: Vec<gpui::AnyElement> = Vec::new();
        for (i, t) in self.tabs.iter().enumerate() {
            let act = i == self.active;
            // 当前标签用实时编辑态算标题（含脏标记）；其余标签用切走时回写的快照。
            let name = if act { self.header_text() } else { t.name() };
            tbc.push(
                div()
                    .id(gpui::ElementId::Name(format!("tab-{i}").into()))
                    .flex()
                    .items_center()
                    .text_sm()
                    .when(act, |d| d.bg(palette.selected_bg))
                    .child(
                        div()
                            .id(gpui::ElementId::Name(format!("tab-label-{i}").into()))
                            .px_2()
                            .py_0p5()
                            .max_w(px(190.))
                            .truncate()
                            .cursor_pointer()
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(move |v, _ev, _w, cx| {
                                    // 必须走 switch_tab：直接改 active 只会切换高亮，
                                    // 编辑器仍持有上一个文档的 doc/cursors。
                                    v.switch_tab(i, cx);
                                }),
                            )
                            .child(name),
                    )
                    // 单标签时不显示关闭按钮，避免把唯一的编辑区关空。
                    .when(self.tabs.len() > 1, |d| {
                        d.child(
                            div()
                                .id(gpui::ElementId::Name(format!("tab-close-{i}").into()))
                                .px_1()
                                .py_0p5()
                                .text_xs()
                                .text_color(palette.muted)
                                .cursor_pointer()
                                .hover(|d| d.text_color(palette.foreground))
                                .on_mouse_down(
                                    gpui::MouseButton::Left,
                                    cx.listener(move |v, _ev, _w, cx| {
                                        v.close_tab(i, cx);
                                    }),
                                )
                                .child("✕"),
                        )
                    })
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
        // 底部状态栏：文件信息 + 行数/光标 + 常用快捷键，横跨整个窗口。
        let status_bar = h_flex()
            .id("statusbar")
            .w_full()
            .h(px(24.))
            .flex_none()
            .items_center()
            .gap_3()
            .px_3()
            .border_t_1()
            .border_color(palette.border)
            .bg(palette.subtle)
            .text_xs()
            .text_color(palette.muted)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .flex_1()
                    .min_w(px(0.))
                    .child(div().flex_1().min_w(px(0.)).truncate().child(format!(
                        "{} · {}",
                        self.header_text(),
                        self.header_full_path()
                    )))
                    .child(
                        div()
                            .flex_none()
                            .child(format!("{line_count} 行 · 光标×{caret_count}")),
                    ),
            )
            .child(div().flex_none().child(format!(
                "字号 {:.0} · 主题 {}",
                f32::from(self.font_size),
                self.settings.theme.label()
            )))
            .child("Cmd+O 打开 · Cmd+S 保存 · Cmd+F 查找替换 · Cmd+Shift+F 全工作区搜索");
        div()
            .id("app")
            .size_full()
            .flex()
            .flex_col()
            .bg(palette.background)
            .text_color(palette.foreground)
            // 窗口级兜底落点（状态栏/大纲/标签栏等）：外部文件导入根目录，不改文档（D74）。
            .can_drop(|drag, _w, _cx| drag.is::<ExternalPaths>())
            .drag_over::<ExternalPaths>(move |style, _drag, _w, _cx| {
                style.bg(palette.code_block_bg)
            })
            .on_drop(cx.listener(|view, paths: &ExternalPaths, _w, cx| {
                let root = view.workspace_root.clone();
                view.import_external_paths(paths.paths(), root, false, cx);
            }))
            .on_action(cx.listener(|view, action: &MenuAction, window, cx| {
                view.run_menu_command(action.command, window, cx);
            }))
            .on_mouse_move(cx.listener(|view, event: &MouseMoveEvent, window, cx| {
                if view.update_table_resize(event.position, cx) {
                    return;
                }
                if view.sidebar_resizing {
                    // 左侧栏贴左缘，鼠标窗口 x 即目标宽度；限幅避免拖没或占满正文。
                    let width = event.position.x.clamp(px(160.), px(420.));
                    if width != view.sidebar_width {
                        view.sidebar_width = width;
                        cx.notify();
                    }
                    return;
                }
                if view.outline_resizing {
                    // 右侧大纲栏贴右缘，宽度 = 窗口宽度 - 鼠标 x。
                    let window_width = window.viewport_size().width;
                    let width = (window_width - event.position.x).clamp(px(160.), px(420.));
                    if width != view.outline_width {
                        view.outline_width = width;
                        cx.notify();
                    }
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|view, _event: &MouseUpEvent, _w, cx| {
                    let mut changed = false;
                    if view.table_resizing.take().is_some() {
                        changed = true;
                    }
                    if view.sidebar_resizing {
                        view.sidebar_resizing = false;
                        changed = true;
                    }
                    if view.outline_resizing {
                        view.outline_resizing = false;
                        changed = true;
                    }
                    if changed {
                        cx.notify();
                    }
                }),
            )
            .child(
                div()
                    .id("main")
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h(px(0.))
                    .child(sidebar_el)
                    .child(sidebar_resize)
                    // 中间主列：标签栏 + 查找/命令浮层 + 编辑区（纵向排列）
                    .child(
                        div()
                            .id("doc")
                            .h_full()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.))
                            // 编辑区拖入外部文件：导入后把非 Markdown 文件转成相对链接（D74）。
                            .can_drop(|drag, _w, _cx| drag.is::<ExternalPaths>())
                            .drag_over::<ExternalPaths>(move |style, _drag, _w, _cx| {
                                style.bg(palette.code_block_bg)
                            })
                            .on_drop(cx.listener(|view, paths: &ExternalPaths, _w, cx| {
                                view.import_external_paths(paths.paths(), None, true, cx);
                            }))
                            .child(tabbar_el)
                            .key_context("mazha-doc")
                            .on_action(cx.listener(|view, _: &InsertTab, _window, cx| {
                                view.handle_tab_action(false, cx);
                                cx.stop_propagation();
                            }))
                            .on_action(cx.listener(|view, _: &Outdent, _window, cx| {
                                view.handle_tab_action(true, cx);
                                cx.stop_propagation();
                            }))
                            .track_focus(&focus)
                            .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                                handle_key(view, event, window, cx);
                            }))
                            // 左键按下：先试任务复选框切换，再放置光标；支持双击选词、三击选行
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|view, event: &MouseDownEvent, window, cx| {
                                    if view.start_table_resize(event.position) {
                                        cx.notify();
                                        return;
                                    }
                                    // Cmd+点击（Windows/Linux 为 Ctrl+点击）：脚注标签先跳转（D69），
                                    // 其余位置再试链接（交给系统默认应用打开），都不移动光标。
                                    if event.modifiers.secondary() {
                                        if let Some(target) =
                                            view.footnote_target_at(event.position)
                                        {
                                            view.goto_source(target, cx);
                                            return;
                                        }
                                        if let Some(url) = view.link_at(event.position) {
                                            cx.open_url(&url);
                                            return;
                                        }
                                    }
                                    // 点音视频卡片：播放/暂停或点进度条 seek，不移动光标。
                                    if !event.modifiers.shift
                                        && !event.modifiers.alt
                                        && view.toggle_media_at(event.position, cx)
                                    {
                                        return;
                                    }
                                    // 点在任务复选框上：切换 `[ ]` ↔ `[x]`（Shift/Alt 点击不切换，
                                    // 让它们保持“扩选/多光标”语义；命中失败则继续放光标）
                                    if !event.modifiers.shift && !event.modifiers.alt {
                                        view.toggle_task_at(event.position, cx);
                                    }
                                    // 双击/三击判定（同一区域 400ms 内连续点击）
                                    let now = std::time::Instant::now();
                                    let mut clicks = 1u32;
                                    if let Some(last_t) = view.last_click_at
                                        && let Some(last_p) = view.last_click_pos
                                        && now.saturating_duration_since(last_t).as_millis() < 400
                                        && (event.position.x - last_p.x).abs() < px(4.)
                                        && (event.position.y - last_p.y).abs() < px(4.)
                                    {
                                        clicks = view.click_count.saturating_add(1).min(3);
                                    }
                                    view.last_click_at = Some(now);
                                    view.last_click_pos = Some(event.position);
                                    view.click_count = clicks;
                                    if let Some(src) = view.hit_source(event.position) {
                                        if event.modifiers.alt {
                                            // Alt+Click: 添加/移除光标
                                            view.add_or_remove_cursor(src);
                                            view.selecting = false;
                                        } else if event.modifiers.shift {
                                            // Shift+Click: 从当前锚点扩选到点击处；无选区时以原光标为锚点。
                                            // 保留 `selecting=true`，按住 Shift 继续拖拽也能继续扩选。
                                            let c = &mut view.cursors[view.primary];
                                            if c.anchor.is_none() {
                                                c.anchor = Some(c.caret);
                                            }
                                            c.caret = src;
                                            view.selecting = true;
                                        } else if clicks == 3 {
                                            view.select_line_at(src);
                                            view.selecting = false;
                                        } else if clicks == 2 {
                                            view.select_word_at(src);
                                            // 双击选词后允许拖拽扩展选区（像主流编辑器）
                                            if let Some(c) = view.cursors.get_mut(view.primary)
                                                && c.anchor.is_none()
                                            {
                                                c.anchor = Some(c.caret);
                                            }
                                            view.selecting = true;
                                        } else {
                                            // 普通点击: 重置为单光标，锚点=点击处，供按住左键拖选使用。
                                            let mut cursor = Cursor::new(src);
                                            cursor.anchor = Some(src);
                                            view.cursors = vec![cursor];
                                            view.primary = 0;
                                            view.selecting = true;
                                        }
                                        // 点击/拖选都让光标先显示，避免刚好落在隐藏相位。
                                        view.reset_blink(cx);
                                    }
                                    let focus = view.focus.clone();
                                    window.focus(&focus, cx);
                                    cx.notify();
                                }),
                            )
                            // 拖动中：持续更新主光标；到视口外时自动滚动以扩选
                            .on_mouse_move(cx.listener(
                                |view, event: &MouseMoveEvent, window, cx| {
                                    if !view.selecting {
                                        return;
                                    }
                                    // 若拖到视口外，按方向自动滚动（每帧按距离加速）
                                    let sc = view.scroll.clone();
                                    let vp = sc.bounds();
                                    if vp.size.height > Pixels::ZERO {
                                        let y = event.position.y;
                                        if y < vp.top() {
                                            let dist_f: f32 = (vp.top() - y).into();
                                            let delta_f = (dist_f / 12.0).max(2.0);
                                            let cur = sc.offset();
                                            let max_off = sc.max_offset().y;
                                            let want =
                                                (cur.y - px(delta_f)).clamp(-max_off, Pixels::ZERO);
                                            if want != cur.y {
                                                sc.set_offset(point(cur.x, want));
                                                cx.notify();
                                            }
                                        } else if y > vp.bottom() {
                                            let dist_f: f32 = (y - vp.bottom()).into();
                                            let delta_f = (dist_f / 12.0).max(2.0);
                                            let cur = sc.offset();
                                            let max_off = sc.max_offset().y;
                                            let want =
                                                (cur.y + px(delta_f)).clamp(-max_off, Pixels::ZERO);
                                            if want != cur.y {
                                                sc.set_offset(point(cur.x, want));
                                                cx.notify();
                                            }
                                        }
                                    }
                                    if let Some(src) = view.hit_source(event.position) {
                                        // 只移动活动端；锚点在按下时已固定，不能在这里用新 caret 覆盖，
                                        // 否则 anchor == caret，拖选永远为空。
                                        if let Some(c) = view.cursors.get_mut(view.primary) {
                                            c.caret = view.doc.snap_to_boundary(src);
                                        }
                                        view.reset_blink(cx);
                                        cx.notify();
                                        let _ = window;
                                    }
                                },
                            ))
                            // 左键松开：结束拖选
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|view, _event: &MouseUpEvent, _, cx| {
                                    view.selecting = false;
                                    cx.notify();
                                }),
                            )
                            // 查找/替换条（`Cmd+F` 展开时才渲染）
                            .children(find_bar)
                            .children(palette_overlay)
                            .children(workspace_search_overlay)
                            // 正文滚动区：滚动**只包正文**——标签栏不随之卷走。
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
                    .when(self.outline_open, |d| {
                        d.child(outline_resize).child(outline_panel)
                    }),
            )
            .child(status_bar)
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
            .position_of_col(b0.col_of_source(start))
            .unwrap_or_else(|| point(Pixels::ZERO, Pixels::ZERO));
        let x0 = layout.origin.x + b0.indent + pos0.x;
        let y0 = layout.origin.y + layout.row_top(layout.row_starts[p0.line]) + pos0.y;
        let (x1, y1) = match layout.blocks.get(p1.line) {
            Some(b1) => {
                let pos1 = b1
                    .position_of_col(b1.col_of_source(end))
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
    // 任意按键都把光标相位重置到“可见”，打字时不会出现光标刚好隐藏的情况。
    view.reset_blink(cx);
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

    // 全工作区搜索（Cmd+Shift+F）：在其它浮层之前拦截，保证任何状态下都能打开。
    // 已展开时重复按下不清空查询（只聚焦面板），避免误覆盖输入。
    if m.platform && shift && event.keystroke.key.as_str() == "f" {
        if !view.workspace_search.open {
            view.open_workspace_search(cx);
        }
        cx.stop_propagation();
        return;
    }

    // 全工作区搜索展开时：所有按键优先交给搜索面板。
    if view.workspace_search.open {
        handle_workspace_search_key(view, event, window, cx);
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
            // 外观：Cmd+=/- 正文字号，Cmd+Option+=/- 等宽字号，Cmd+Shift+L 循环主题。
            "=" | "+" if m.platform && m.alt => {
                view.adjust_mono_font_size(FONT_STEP, cx);
                cx.stop_propagation();
            }
            "-" if m.platform && m.alt => {
                view.adjust_mono_font_size(-FONT_STEP, cx);
                cx.stop_propagation();
            }
            "=" | "+" if m.platform => {
                view.adjust_font_size(FONT_STEP, cx);
                cx.stop_propagation();
            }
            "-" if m.platform && !shift => {
                view.adjust_font_size(-FONT_STEP, cx);
                cx.stop_propagation();
            }
            "l" if m.platform && shift => {
                view.cycle_theme(window, cx);
                cx.stop_propagation();
            }
            "0" if m.platform && m.alt => {
                view.reset_appearance(window, cx);
                cx.stop_propagation();
            }
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
                view.selecting = false;
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
            if !shift || !view.insert_table_break(cx) {
                view.insert_newline(cx);
            }
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

/// 全工作区搜索展开时的按键处理（`handle_key` 在 `workspace_search.open` 时转到这里）。
///
/// 语义：可打印字符追加查询并即时重扫；`Backspace` 删字符；`↑`/`↓` 选择命中；
/// `Enter` 打开并定位；`Esc` 关闭；`Cmd+Alt+C` 切换大小写；其余 `Cmd` 组合忽略。
fn handle_workspace_search_key(
    view: &mut DocView,
    event: &KeyDownEvent,
    _window: &mut Window,
    cx: &mut Context<DocView>,
) {
    let m = &event.keystroke.modifiers;
    let key = event.keystroke.key.as_str();

    if m.control || m.platform {
        match key {
            // Cmd+Shift+F 再按：关闭；其它 Cmd 组合不抢正文编辑。
            "f" if m.platform && m.shift => view.close_workspace_search(cx),
            "c" if m.platform && m.alt => {
                view.workspace_search.case_sensitive = !view.workspace_search.case_sensitive;
                view.recompute_workspace_hits();
                cx.notify();
            }
            _ => {}
        }
        return;
    }

    match key {
        "escape" => view.close_workspace_search(cx),
        "up" => {
            let len = view.workspace_search.hits.len();
            if len > 0 {
                view.workspace_search.selected = (view.workspace_search.selected + len - 1) % len;
            }
            cx.notify();
        }
        "down" => {
            let len = view.workspace_search.hits.len();
            if len > 0 {
                view.workspace_search.selected = (view.workspace_search.selected + 1) % len;
            }
            cx.notify();
        }
        "enter" | "return" => {
            view.open_workspace_hit(view.workspace_search.selected, cx);
        }
        "backspace" => {
            view.workspace_search.query.pop();
            view.recompute_workspace_hits();
            cx.notify();
        }
        "delete" => {
            view.workspace_search.query.clear();
            view.recompute_workspace_hits();
            cx.notify();
        }
        k if k.chars().count() == 1 && !k.chars().next().unwrap().is_control() => {
            // 可打印 ASCII 直键兜底（与命令面板/查找条一致，不走 IME）。
            view.workspace_search.query.push(k.chars().next().unwrap());
            view.recompute_workspace_hits();
            cx.notify();
        }
        _ => {}
    }
}

/// 查找条展开时的按键处理（`handle_key` 在 `find.open` 时全部转到这里）。
///
/// 语义：`Enter`/`Shift+Enter` 上/下一个匹配、`Tab` 切换查找/替换框、`Backspace`/`Delete`
/// 编辑聚焦框、`Esc` 关闭、`Cmd+F` 再按关闭、`Cmd+Enter` 替换当前、`Cmd+Shift+Enter`
/// 替换全部、`Cmd+Alt+C` 切换大小写敏感；其余 `Cmd` 组合一律忽略（不在查找时改正文）。
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

/// 行高要同时容下正文与行内等宽字号，避免等宽字号调大后行内代码压到相邻行。
fn block_line_px_with_mono(
    block: &BlockStyle,
    font_size: Pixels,
    mono_font_size: Pixels,
) -> Pixels {
    let line_font_size = if block.code_block {
        font_size
    } else {
        font_size.max(mono_font_size)
    };
    block_line_px(block, line_font_size)
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
    let mut color = run.color.map(rgb_to_hsla).unwrap_or(palette.foreground);
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
    // 表头整行加粗：与表体的表头底色/分隔线一起，让首行一眼可辨
    if line.block.table_line == Some(TableLine::Header) {
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

/// 代码块单行可排版宽度：扣掉块缩进、页面右内边距、文本右移量和面板右内边距。
///
/// 代码块面板左缘在 `indent - 8px`，文本左缘在 `indent + CODE_TEXT_INSET`；
/// 右缘固定到 `content_width - BASE`（与正文右边距一致），因此文本宽度必须
/// 同时扣除左移和 `CODE_RIGHT_INSET`，否则长行会越过面板右缘。
fn code_block_wrap_width(content_width_total: Pixels, indent: Pixels) -> Pixels {
    (content_width_total - indent - BASE - CODE_TEXT_INSET - CODE_RIGHT_INSET).max(px(48.))
}

/// 引用块单行可排版宽度：在正文宽度基础上再给右侧留出 `QUOTE_TEXT_RIGHT_INSET`。
///
/// 引用文字左缘 = 正文左缘 + 每层引用缩进，底色左缘只比正文左缘缩 4px，
/// 因此长行必须在右侧收窄，否则会顶到/越过底色右缘。
fn quote_block_wrap_width(content_width_total: Pixels, indent: Pixels) -> Pixels {
    (content_width_total - indent - BASE - QUOTE_TEXT_RIGHT_INSET).max(px(48.))
}

/// 独占行图片：命中唯一一个图片 Widget，且本行没有其它可见文本/表格/代码。
fn standalone_image_dest(line: &ProjectedLine) -> Option<&String> {
    if !line.runs.is_empty()
        || line.widgets.len() != 1
        || line.table_cells.is_some()
        || line.block.code_block
    {
        return None;
    }
    match &line.widgets[0].kind {
        WidgetKind::Image { dest, .. } => Some(dest),
        WidgetKind::Media { .. } => None,
    }
}

/// 独占行音视频：命中唯一一个 Media Widget，且本行没有其它可见文本/表格/代码。
///
/// 行内媒体（前面有文字）不生成卡片，只显示图片式的占位 chip；光标进入独占行时
/// 与图片一致切回源码编辑。
fn standalone_media(line: &ProjectedLine) -> Option<MediaElement> {
    if !line.runs.is_empty()
        || line.widgets.len() != 1
        || line.table_cells.is_some()
        || line.block.code_block
    {
        return None;
    }
    match &line.widgets[0].kind {
        WidgetKind::Media {
            kind,
            src,
            poster,
            title,
        } => Some(MediaElement {
            kind: *kind,
            src: src.clone(),
            poster: poster.clone(),
            title: title.clone(),
        }),
        _ => None,
    }
}

/// 媒体播放状态键：同类型同地址共享一个播放器（同一文档内重复引用时同步播放）。
fn media_key(kind: MediaKind, src: &str) -> String {
    format!("{}:{}", kind.tag(), src.trim())
}

/// 秒数格式化为 `m:ss` / `h:mm:ss`；非有限值显示 `--:--`。
fn fmt_time(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "--:--".to_string();
    }
    let total = seconds.round() as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// 视频尺寸适配：不超过正文可用宽度，也不放大小尺寸源。
fn fit_video_size(natural: Size<Pixels>, max_width: Pixels) -> Size<Pixels> {
    let width = natural.width.max(px(1.));
    let height = natural.height.max(px(1.));
    let scale = (max_width.max(px(1.)) / width).min(1.);
    size(width * scale, height * scale)
}

/// 把图片地址解析为 GPUI 资源：远程 URL 直接用 `Resource::Uri`，其余走本地路径。
///
/// `http(s)://` 原样使用；协议相对 `//host/path` 在本地编辑器没有页面协议可继承，
/// 统一按 `https://` 处理。`data:`/`mailto:` 返回 `None`（data URI 由
/// [`decode_data_image`] 单独解码），本地路径由 [`resolve_image_path`] 处理。
fn image_resource(dest: &str, doc_path: Option<&Path>) -> Option<Resource> {
    let dest = dest.trim();
    let lower = dest.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Some(Resource::Uri(dest.to_string().into()));
    }
    if dest.starts_with("//") {
        return Some(Resource::Uri(format!("https:{dest}").into()));
    }
    resolve_image_path(dest, doc_path).map(|path| Resource::Path(path.into()))
}

/// 把 GPUI 剪贴板条目转换为可直接插入文档的 Markdown。
///
/// 图片文件路径优先于内存图片和文本；这样从文件管理器复制图片时保留文件引用，
/// 截图等只有像素数据的内容才会内联为 base64 data URI。
fn clipboard_markdown(item: &ClipboardItem) -> Option<String> {
    let path_images = item
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            ClipboardEntry::ExternalPaths(paths) => Some(paths.0.iter()),
            _ => None,
        })
        .flatten()
        .filter(|path| is_image_path(path))
        .map(|path| markdown_image_path(path))
        .collect::<Vec<_>>();
    if !path_images.is_empty() {
        return Some(path_images.join("\n"));
    }

    item.entries()
        .iter()
        .find_map(|entry| match entry {
            ClipboardEntry::Image(image) if !image.bytes.is_empty() => {
                let payload = base64::engine::general_purpose::STANDARD.encode(&image.bytes);
                Some(format!(
                    "![图片](data:{};base64,{payload})",
                    image.format.mime_type()
                ))
            }
            _ => None,
        })
        .or_else(|| item.text().filter(|text| !text.is_empty()))
}

/// 图片文件路径生成 Markdown 图片语法；尖括号允许路径保留空格。
fn markdown_image_path(path: &Path) -> String {
    format_markdown_link(path, None, true)
}

/// 是否为桌面端可渲染的常见图片文件路径。
fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png"
                    | "jpg"
                    | "jpeg"
                    | "webp"
                    | "gif"
                    | "svg"
                    | "bmp"
                    | "tif"
                    | "tiff"
                    | "ico"
                    | "pbm"
                    | "pgm"
                    | "ppm"
                    | "pnm"
            )
        })
}

/// 解码 `data:` 图片 URI，返回 GPUI 图片格式与原始字节。
///
/// 支持 `data:image/png;base64,...` 与百分号编码的二进制形式；MIME 决定格式。
/// 未知 MIME、缺失逗号、base64/百分号解码失败或空载荷都返回 `None`，回退占位。
fn decode_data_image(uri: &str) -> Option<(ImageFormat, Vec<u8>)> {
    let rest = uri.strip_prefix("data:")?;
    let (meta, payload) = rest.split_once(',')?;
    let mut parts = meta.split(';');
    let format = image_format_from_mime(parts.next()?.trim())?;
    let is_base64 = parts.any(|part| part.trim().eq_ignore_ascii_case("base64"));
    let bytes = if is_base64 {
        // data URI 允许换行/空白；base64 解码器本身不容忍，先剔除。
        let compact: String = payload
            .chars()
            .filter(|c| !c.is_ascii_whitespace())
            .collect();
        base64::engine::general_purpose::STANDARD
            .decode(compact)
            .ok()?
    } else {
        percent_decode_bytes(payload)
    };
    (!bytes.is_empty()).then_some((format, bytes))
}

/// data URI MIME → GPUI 图片格式；未知类型返回 `None`。
fn image_format_from_mime(mime: &str) -> Option<ImageFormat> {
    Some(match mime.to_ascii_lowercase().as_str() {
        "image/png" => ImageFormat::Png,
        "image/jpeg" | "image/jpg" => ImageFormat::Jpeg,
        "image/webp" => ImageFormat::Webp,
        "image/gif" => ImageFormat::Gif,
        "image/svg+xml" => ImageFormat::Svg,
        "image/bmp" | "image/x-ms-bmp" => ImageFormat::Bmp,
        "image/tiff" => ImageFormat::Tiff,
        "image/x-icon" | "image/vnd.microsoft.icon" => ImageFormat::Ico,
        "image/x-portable-anymap"
        | "image/x-portable-bitmap"
        | "image/x-portable-pixmap"
        | "image/x-portable-graymap" => ImageFormat::Pnm,
        _ => return None,
    })
}

/// 加载图片：data URI 走 [`Image`] 的内联解码，其它地址走 GPUI 资源加载。
fn load_image(
    dest: &str,
    doc_path: Option<&Path>,
    window: &mut Window,
    cx: &mut App,
) -> Option<Arc<RenderImage>> {
    if let Some((format, bytes)) = decode_data_image(dest.trim()) {
        return Arc::new(Image::from_bytes(format, bytes)).use_render_image(window, cx);
    }
    let resource = image_resource(dest, doc_path)?;
    window
        .use_asset::<ImageAssetLoader>(&resource, cx)
        .and_then(Result::ok)
}

/// 把图片地址解析为本地路径：相对路径以当前文档目录为基准；URL/空地址返回 `None`。
///
/// 本地文件支持绝对路径、`file://` 前缀与 `%20` 等百分号转义，方便直接使用
/// 浏览器/其它编辑器生成的 Markdown。远程图片由 [`image_resource`] 单独处理。
fn resolve_image_path(dest: &str, doc_path: Option<&Path>) -> Option<PathBuf> {
    let dest = dest.trim();
    if dest.is_empty() {
        return None;
    }
    let lower = dest.to_ascii_lowercase();
    if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("data:")
        || lower.starts_with("mailto:")
        || dest.starts_with("//")
    {
        return None;
    }
    let raw = dest.strip_prefix("file://").unwrap_or(dest);
    let path = PathBuf::from(percent_decode(raw));
    if path.is_absolute() {
        Some(path)
    } else {
        doc_path.and_then(Path::parent).map(|dir| dir.join(path))
    }
}

/// 还原路径中的 `%XX` 转义（非法序列原样保留）。
fn percent_decode(text: &str) -> String {
    String::from_utf8_lossy(&percent_decode_bytes(text)).into_owned()
}

/// 还原 `%XX` 转义并保留任意二进制字节（data URI 非 base64 分支使用）。
fn percent_decode_bytes(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2]))
        {
            out.push(hi << 4 | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

/// 单个十六进制字符值。
fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// 图片适配：宽度不超过正文可用宽度，且**不放大**小图。
fn fit_image_size(natural: Size<Pixels>, max_width: Pixels) -> Size<Pixels> {
    let width = natural.width.max(px(1.));
    let height = natural.height.max(px(1.));
    let scale = (max_width.max(px(1.)) / width).min(1.);
    size(width * scale, height * scale)
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
    line_height: Pixels,
    window: &mut Window,
    cx: &mut App,
) {
    let lx = right - label.width() - px(6.);
    let ly = top + px(3.);
    let _ = label.paint(
        point(lx, ly),
        (line_height - px(6.)).max(px(1.)),
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

/// 排版基数（左右内边距 / 列表标记区）。
const BASE: Pixels = px(16.);

/// 代码块文本相对块缩进的右移量（同时作为面板左内边距的一部分）。
const CODE_TEXT_INSET: Pixels = px(12.);

/// 代码块文本距面板右缘的内边距。
const CODE_RIGHT_INSET: Pixels = px(20.);

/// 引用块底色相对正文文本列的左右内缩（避免贴住正文边界）。
const QUOTE_BAND_INSET: Pixels = px(4.);

/// 引用文字距正文右缘的留白：底色右缘再收 4px，文字再收 16px，形成 12px 内边距。
const QUOTE_TEXT_RIGHT_INSET: Pixels = px(16.);
/// 最小行高：正文/等宽字号较小时仍保持 24px，避免行距过密。
const LINE_HEIGHT: Pixels = px(24.);

/// 图片与上下相邻内容的呼吸间距。
const IMAGE_VERTICAL_PAD: Pixels = px(8.);
/// 媒体卡片与上下文字之间的呼吸间距。
const MEDIA_VERTICAL_PAD: Pixels = px(8.);
/// 媒体控制条高度（视频底栏/音频卡片底栏）。
const MEDIA_CONTROL_HEIGHT: Pixels = px(32.);
/// 音频播放按钮固定宽高。
const AUDIO_BUTTON_SIZE: Pixels = px(50.);
/// 视频未取帧时的占位宽高比。
const VIDEO_PLACEHOLDER_RATIO: f32 = 9.0 / 16.0;

/// 图片圆角（避免直角贴住引用底色/面板）。
const IMAGE_CORNER_RADIUS: Pixels = px(6.);

// 编辑器内的 Tab/Shift+Tab（D44）：Root 的全局 `tab` 绑定会先命中并吞掉按键，
// 必须在 `mazha-doc` 上下文里绑定更具体的动作，否则 `on_key_down` 收不到 Tab。
gpui_kit::actions!(mazha_doc, [InsertTab, Outdent]);

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
        let (
            palette,
            base_font,
            mono_font,
            base_size,
            mono_size,
            selection,
            focus_mode,
            source_mode,
        ) = {
            let snapshot = self.view.read(cx);
            (
                snapshot.palette,
                gpui::font(snapshot.ui_font.clone()),
                gpui::font(snapshot.mono_font.clone()),
                snapshot.font_size,
                snapshot.mono_font_size,
                snapshot.all_selections(),
                snapshot.settings.focus_mode,
                snapshot.settings.source_mode,
            )
        };
        let indent_base = BASE;

        // update 提供 &mut DocView（投影缓存需惰性建立），闭包返回几何数据避免借用越界
        let content_width_total = bounds.size.width;
        let (blocks, widgets, row_starts, row_tops, caret_row, task_cand) =
            self.view.update(cx, |view, cx| {
                // 需先取所有光标的源偏移，随后 projection() 会可变借用 view.doc（借用不相交）
                let caret_sources: Vec<usize> = view.cursors.iter().map(|c| c.caret).collect();
                let caret = view.cursors[view.primary].caret;
                // 光标所在整行源码：图片/代码围栏行进入编辑态时直接展示原文。
                // 必须在 projection() 借用 doc 之前取好，避免同时可变/不可变借用。
                let caret_line_text = view
                    .doc
                    .slice(view.doc.line_range_of(caret))
                    .trim_end_matches(['\n', '\r'])
                    .to_string();
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
                // 表格行不做行内展开：网格排版依赖投影的单元格字节范围，展开标记会让
                // 范围失效（编辑表格时保持稳定的网格视图，D47）。
                let caret_reveal = if view.doc.projection().lines[caret_line]
                    .block
                    .table_line
                    .is_some()
                {
                    None
                } else {
                    inline_reveals(&caret_line_src).into_iter().find(|r| {
                        caret >= caret_line_range.start + r.range.start
                            && caret <= caret_line_range.start + r.range.end
                    })
                };
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
                let doc_path = view.path.clone();
                // 源码模式（D68）：预取各行原始源码文本，避免在持有 projection 借用时再访问 doc。
                let source_line_texts: Option<Vec<String>> = if source_mode {
                    let ranges: Vec<std::ops::Range<usize>> = view
                        .doc
                        .projection()
                        .lines
                        .iter()
                        .map(|l| l.source_range.clone())
                        .collect();
                    Some(
                        ranges
                            .into_iter()
                            .map(|r| view.doc.slice(r).trim_end_matches(['\n', '\r']).to_string())
                            .collect(),
                    )
                } else {
                    None
                };
                let projection = view.doc.projection();
                // 删除/改写媒体标签后，旧播放器不能在后台继续播放：按当前投影引用
                // 的媒体键清理。切换标签时则整体清空（见 `activate_tab`）。
                let active_media: Vec<String> = projection
                    .lines
                    .iter()
                    .flat_map(|l| l.widgets.iter())
                    .filter_map(|w| match &w.kind {
                        WidgetKind::Media { kind, src, .. } => Some(media_key(*kind, src)),
                        _ => None,
                    })
                    .collect();
                view.media.retain(|key, _| active_media.contains(key));
                // 各光标所在逻辑行：用于判定块级 widget 是否处于编辑态（SPEC §11.5）
                let caret_lines: Vec<usize> = caret_sources
                    .iter()
                    .map(|&s| projection.line_of_source(s))
                    .collect();
                // 专注模式（D68）：仅当前主光标所在块正常显示，其余行淡化。
                let focus_range: Option<std::ops::Range<usize>> = if focus_mode {
                    let line = projection.line_of_source(caret);
                    if let Some(w) = projection.block_widget_on_line(line) {
                        Some(w.line_range.clone())
                    } else {
                        Some(line..line + 1)
                    }
                } else {
                    None
                };
                // 块级 widget 装饰：光标在其内=编辑态（不画装饰），否则=渲染态（画装饰）
                // 源码模式（D68）不绘制 WYSIWYG 块级装饰。
                let widgets: Vec<WidgetDecor> = if source_mode {
                    Vec::new()
                } else {
                    projection
                        .block_widgets()
                        .iter()
                        // 图片不走通用装饰面板：渲染态由逐行图片预览负责，编辑态显示整行源码。
                        .filter_map(|w| {
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
                                BlockWidgetKind::Image { .. } => return None,
                            };
                            Some(WidgetDecor {
                                kind,
                                line_range: w.line_range.clone(),
                                indent,
                                editing,
                            })
                        })
                        .collect()
                };
                // 选区高亮：WYSIWYG 按投影显示列；源码模式按原始源码字符列。
                let mut sel_by_line: Vec<Option<Range<usize>>> = vec![None; projection.lines.len()];
                if source_mode {
                    let srcs = source_line_texts.as_ref().unwrap();
                    for (idx, line) in projection.lines.iter().enumerate() {
                        let start = line.source_range.start;
                        let end = line.source_range.end;
                        let src = &srcs[idx];
                        let mut agg: Option<Range<usize>> = None;
                        for sel in &selection {
                            let s = sel.start.clamp(start, end);
                            let e = sel.end.clamp(start, end);
                            if s >= e {
                                continue;
                            }
                            let cs = src.get(..(s - start)).map_or(0, |t| t.chars().count());
                            let ce = src.get(..(e - start)).map_or(cs, |t| t.chars().count());
                            match &mut agg {
                                Some(a) => {
                                    a.start = a.start.min(cs);
                                    a.end = a.end.max(ce);
                                }
                                None => agg = Some(cs..ce),
                            }
                        }
                        sel_by_line[idx] = agg;
                    }
                } else {
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
                // 表格网格：先整表测量列宽并形状化单元格（放不下则返回 None，走流式排版）
                // 源码模式下禁用网格表格，按原始源码行显示。
                let table_rows = if source_mode {
                    vec![None; projection.lines.len()]
                } else {
                    build_table_grids(
                        window,
                        projection,
                        &TableGridCtx {
                            content_width_total,
                            indent_base,
                            base_font: &base_font,
                            mono_font: &mono_font,
                            palette: &palette,
                            base_size,
                            mono_size,
                        },
                        &view.table_column_widths,
                        &view.table_row_heights,
                    )
                };
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
                    // 围栏行/独占图片行：光标进入时显示整行源码，并做 1:1 字节映射。
                    // 源码模式（D68）：整行按原始 Markdown 源码显示与编辑。
                    let raw_src: Option<SharedString> = if source_mode {
                        source_line_texts
                            .as_ref()
                            .map(|srcs| srcs[idx].clone().into())
                    } else {
                        fence_texts
                            .iter()
                            .find(|(i, _)| *i == idx)
                            .map(|(_, src)| src.clone().into())
                            .or_else(|| {
                                (caret_pos.line == idx
                                    && (standalone_image_dest(line).is_some()
                                        || standalone_media(line).is_some()))
                                .then(|| caret_line_text.clone().into())
                            })
                    };
                    // 展开态显示文本 = 整行源码（含标记），光标列按行内字节直接换算
                    let caret_col = if let Some(src) = &raw_src {
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
                    // 本行若属于网格化表格，复用预排好的单元格几何
                    let table_row = table_rows.get(idx).cloned().flatten();
                    let dim = focus_range.as_ref().is_some_and(|r| !r.contains(&idx));
                    let (text, runs, oblique_spans) = if let Some(src) = &raw_src {
                        // 源码模式显示原文（等宽、正文色）；围栏编辑态沿用弱化色。
                        let mut color = if source_mode {
                            palette.foreground
                        } else {
                            palette.muted
                        };
                        if dim {
                            color.a *= 0.28;
                        }
                        let run = gpui::TextRun {
                            len: src.len(),
                            font: mono_font.clone(),
                            color,
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        };
                        (src.clone(), vec![run], Vec::new())
                    } else {
                        let mut built = build_line_runs(
                            line,
                            &base_font,
                            &mono_font,
                            &palette,
                            code_hl[idx].as_deref(),
                        );
                        if dim {
                            for run in &mut built.1 {
                                run.color.a *= 0.28;
                                if let Some(bg) = &mut run.background_color {
                                    bg.a *= 0.28;
                                }
                            }
                        }
                        built
                    };
                    // 前缀字节数 = 形状文本长度 - 投影显示文本长度（引用 "> " 等投影外文本）
                    // 围栏源码是整行原文，没有额外前缀
                    let prefix_bytes = if raw_src.is_some() {
                        0
                    } else {
                        text.len().saturating_sub(line.display_text().len())
                    };
                    // 逐块字号/行高：标题按级别放大，行高同步放大，避免字形压到相邻行
                    // 源码模式统一等宽字号与固定左边距，按原始源码行排版。
                    let (font_size, indent) = if source_mode && raw_src.is_some() {
                        (mono_size, indent_base)
                    } else {
                        (
                            block_font_size(&line.block, base_size, mono_size),
                            block_indent(line, indent_base),
                        )
                    };
                    // 图片只按正文可用宽度适配；不放大、不改宽高比（渲染态才加载）。
                    let image_max_width = (content_width_total - indent - BASE).max(px(48.));
                    let image_preview = if raw_src.is_none()
                        && caret_pos.line != idx
                        && let Some(dest) = standalone_image_dest(line)
                        && let Some(data) = load_image(dest, doc_path.as_deref(), window, cx)
                    {
                        let natural = data.size(0);
                        let natural = size(
                            px(natural.width.0.max(1) as f32),
                            px(natural.height.0.max(1) as f32),
                        );
                        Some(ImagePreview {
                            data,
                            size: fit_image_size(natural, image_max_width),
                        })
                    } else {
                        None
                    };
                    // 独占行音视频：渲染态显示播放卡片；光标进入该行时显示源码。
                    let media_preview = if raw_src.is_none()
                        && caret_pos.line != idx
                        && let Some(media) = standalone_media(line)
                    {
                        let key = media_key(media.kind, &media.src);
                        let playback = view.media.get(&key);
                        let player = playback.and_then(|p| p.player.as_ref());
                        let error = playback
                            .and_then(|p| p.error.as_deref())
                            .or_else(|| player.and_then(|p| p.error()));
                        let playing = player.is_some_and(|p| p.playing());
                        let loading = player.is_some_and(|p| p.loading());
                        let position = player.map_or(0.0, |p| p.position());
                        let duration = player.map_or(0.0, |p| p.duration());
                        // 视频优先用首帧自然尺寸；未取到帧时再尝试海报，最后回退 16:9。
                        let frame_size = player.and_then(|p| p.frame_size());
                        let poster = if media.kind == MediaKind::Video && frame_size.is_none() {
                            media
                                .poster
                                .as_deref()
                                .and_then(|p| load_image(p, doc_path.as_deref(), window, cx))
                        } else {
                            None
                        };
                        let card_size = match media.kind {
                            MediaKind::Audio => size(AUDIO_BUTTON_SIZE, AUDIO_BUTTON_SIZE),
                            MediaKind::Video => {
                                if let Some(natural) = frame_size {
                                    fit_video_size(natural, image_max_width)
                                } else if let Some(poster) = &poster {
                                    let natural = poster.size(0);
                                    fit_video_size(
                                        size(
                                            px(natural.width.0.max(1) as f32),
                                            px(natural.height.0.max(1) as f32),
                                        ),
                                        image_max_width,
                                    )
                                } else {
                                    size(image_max_width, image_max_width * VIDEO_PLACEHOLDER_RATIO)
                                }
                            }
                        };
                        let rect = Bounds::new(
                            point(
                                bounds.origin.x + indent,
                                bounds.origin.y + top_acc + MEDIA_VERTICAL_PAD,
                            ),
                            card_size,
                        );
                        let progress = Bounds::new(
                            point(rect.origin.x + px(8.), rect.bottom() - px(7.)),
                            size((rect.size.width - px(16.)).max(px(1.)), px(3.)),
                        );
                        let state = if error.is_some() {
                            "加载失败"
                        } else if playing {
                            "暂停"
                        } else {
                            "播放"
                        };
                        let label_text = match media.kind {
                            MediaKind::Audio if error.is_some() => "!".to_string(),
                            MediaKind::Audio if playing => "Ⅱ".to_string(),
                            MediaKind::Audio => "▶".to_string(),
                            MediaKind::Video if duration > 0.0 => format!(
                                "{}  ·  {state}  {} / {}",
                                media.title.as_deref().unwrap_or(media.kind.label()),
                                fmt_time(position),
                                fmt_time(duration)
                            ),
                            MediaKind::Video => format!(
                                "{}  ·  {state}",
                                media.title.as_deref().unwrap_or(media.kind.label())
                            ),
                        };
                        let run = gpui::TextRun {
                            len: label_text.len(),
                            font: base_font.clone(),
                            color: palette.foreground,
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        };
                        let label = window.text_system().shape_line(
                            label_text.into(),
                            if media.kind == MediaKind::Audio {
                                px(20.)
                            } else {
                                px(12.)
                            },
                            &[run],
                            None,
                        );
                        let duration_label = (media.kind == MediaKind::Audio).then(|| {
                            let text = if duration > 0.0 {
                                format!("{} / {}", fmt_time(position), fmt_time(duration))
                            } else {
                                "0:00 / --:--".to_string()
                            };
                            let run = gpui::TextRun {
                                len: text.len(),
                                font: base_font.clone(),
                                color: palette.muted,
                                background_color: None,
                                underline: None,
                                strikethrough: None,
                            };
                            Box::new(window.text_system().shape_line(
                                text.into(),
                                px(12.),
                                &[run],
                                None,
                            ))
                        });
                        Some(MediaPreview {
                            key,
                            kind: media.kind,
                            src: media.src,
                            rect,
                            progress,
                            label: Box::new(label),
                            duration_label,
                            loading,
                            poster,
                            position,
                            duration,
                        })
                    } else {
                        None
                    };
                    // 网格表格行高由单元格换行后的实际视觉行数决定。
                    let mut line_px = if let Some(row) = &table_row {
                        row.height
                    } else {
                        block_line_px_with_mono(&line.block, font_size, mono_size)
                    };
                    // 独占图片行：行高跟随图片（含上下呼吸间距），命中测试/滚动同步生效。
                    if let Some(image) = &image_preview {
                        line_px = image.size.height + IMAGE_VERTICAL_PAD * 2.;
                    }
                    if let Some(media) = &media_preview {
                        line_px = media.rect.size.height + MEDIA_VERTICAL_PAD * 2.;
                    }
                    // 表格分隔行（`| --- |`）投影为空行：正常布局下高度归零，改由表头行
                    // 自身加高来提供上下内边距，避免表头与表体之间横着一整行空白。
                    // 光标若落进分隔行则恢复正常高度，否则会出现"看不见的光标"。
                    let is_delim_hidden = line.block.table_line == Some(TableLine::Delimiter)
                        && caret_pos.line != idx;
                    if table_row.is_none() {
                        if line.block.table_line == Some(TableLine::Delimiter) {
                            line_px = if is_delim_hidden {
                                Pixels::ZERO
                            } else {
                                line_px
                            };
                        } else if line.block.table_line == Some(TableLine::Header) {
                            // 上下各多给约 6px，表头文字在行盒内垂直居中
                            line_px += px(12.);
                        }
                    }
                    // 可用宽度 = 元素宽度 - 左缩进（含 BASE 左内边距）- 右侧 BASE 内边距
                    let wrap_width = if line.block.code_block {
                        code_block_wrap_width(content_width_total, indent)
                    } else if line.block.quote_depth > 0 {
                        quote_block_wrap_width(content_width_total, indent)
                    } else {
                        (content_width_total - indent - BASE).max(px(48.))
                    };
                    // 逐 run 字体（粗/斜/等宽族）由 build_line_runs 决定，**不再统一覆盖**：
                    // 覆盖会把代码块的等宽族、标题的粗体都抹平，看起来就是"md 没渲染"。
                    let shaped = shape_wrapped_line(window, text, font_size, &runs, wrap_width);
                    // 网格表格每行固定单视觉行（单元格不换行；放不下的表已回退流式）
                    let rows = if table_row.is_some() {
                        1
                    } else {
                        shaped.wrap_boundaries().len() + 1
                    };
                    let rows_eff = if is_delim_hidden { 0usize } else { rows };

                    // 光标行（内容坐标）：表格按单元格几何取 x/y，普通行按换行后的视觉行取
                    if let Some(col) = caret_col {
                        if let Some(row) = &table_row {
                            if let Some(p) = table_point_of_col(row, line, col) {
                                caret_row = Some((top_acc + p.y, row.line_height));
                            }
                        } else {
                            let cbyte = if let Some(src) = &raw_src {
                                char_col_to_byte(src, col)
                            } else {
                                prefix_bytes + line.display_byte_of_col(col)
                            };
                            if let Some(p) = shaped.position_for_index(cbyte, line_px) {
                                caret_row = Some((top_acc + p.y, line_px));
                            }
                        }
                    }
                    // 列表符号：任务项改画复选框（见 paint），其余 shape 成文本右对齐进标记区
                    // 源码模式下显示原始 `-`/`1.`/`[ ]` 文本，不绘制渲染态标记与复选框。
                    let marker =
                        if !source_mode && line.block.bullet.is_some() && line.block.task.is_none()
                        {
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
                    let check = if !source_mode && line.block.task == Some(true) {
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
                    if !source_mode && line.block.task.is_some() {
                        task_cand.push((line.source_range.clone(), indent, top_acc, line_px));
                    }
                    // 行内图片占位：`![alt](url)` 整段被投影隐藏，画个小标签补上可见性。
                    // 独占行已真实渲染（或正在编辑原文）时不再叠占位标签。
                    let mut image_chips = Vec::new();
                    if raw_src.is_none() && image_preview.is_none() && media_preview.is_none() {
                        for w in &line.widgets {
                            // 展开态显示文本与源逐字节对齐；投影态用投影给的显示列
                            let byte = prefix_bytes
                                + if is_revealed {
                                    let rel = w
                                        .source_range
                                        .start
                                        .saturating_sub(line.source_range.start);
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
                    }

                    row_starts.push(row_acc);
                    // `row_tops` 按**全局视觉行**建表（软换行后一行会占多行）：
                    // 之前的实现每逻辑行只 push 一个顶边，后续块用 `row_starts` 里的
                    // 视觉行号索引时会取到别的行/越界为 0，导致块与块重叠、图片与
                    // 段落错位、内容下方出现大段空白。
                    for _ in 0..rows_eff {
                        row_tops.push(top_acc);
                        top_acc += line_px;
                    }
                    row_acc += rows_eff;
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
                        rows: rows_eff,
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
                        raw_src,
                        image: image_preview,
                        media: media_preview,
                        table: table_row,
                    });
                }
                row_starts.push(row_acc);
                row_tops.push(top_acc);
                (v, widgets, row_starts, row_tops, caret_row, task_cand)
            });

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

        let mut table_resize_handles = Vec::new();
        for (idx, block) in blocks.iter().enumerate() {
            let Some(row) = &block.table else {
                continue;
            };
            let top = bounds.origin.y + row_tops[row_starts[idx]];
            let left = bounds.origin.x + block.indent;
            let bottom = top + block.line_height;
            let max_width = (bounds.size.width - block.indent - BASE).max(TABLE_DRAG_MIN_WIDTH);
            let widths: Vec<Pixels> = row
                .grid
                .boundaries
                .windows(2)
                .map(|pair| pair[1] - pair[0])
                .collect();
            for column in 0..widths.len().saturating_sub(1) {
                let x = left + row.grid.boundaries[column + 1];
                table_resize_handles.push(TableResizeHandle {
                    rect: Bounds::new(
                        point(x - TABLE_RESIZE_HIT, top),
                        size(TABLE_RESIZE_HIT * 2., block.line_height),
                    ),
                    hitbox: None,
                    table: row.grid.source_start,
                    kind: TableResizeKind::Column(column),
                    widths: widths.clone(),
                    max_width,
                    row: row.source_start,
                    row_height: block.line_height,
                    row_min_height: row.natural_height,
                });
            }
            let right = left + row.grid.width;
            table_resize_handles.push(TableResizeHandle {
                rect: Bounds::new(
                    point(right - TABLE_RESIZE_HIT, top),
                    size(TABLE_RESIZE_HIT * 2., block.line_height),
                ),
                hitbox: None,
                table: row.grid.source_start,
                kind: TableResizeKind::RightEdge,
                widths: widths.clone(),
                max_width,
                row: row.source_start,
                row_height: block.line_height,
                row_min_height: row.natural_height,
            });
            table_resize_handles.push(TableResizeHandle {
                rect: Bounds::new(
                    point(left, bottom - TABLE_RESIZE_HIT),
                    size(row.grid.width, TABLE_RESIZE_HIT * 2.),
                ),
                hitbox: None,
                table: row.grid.source_start,
                kind: TableResizeKind::RowEdge,
                widths: widths.clone(),
                max_width,
                row: row.source_start,
                row_height: block.line_height,
                row_min_height: row.natural_height,
            });
            let last_table_row = blocks.get(idx + 1).is_none_or(|next| {
                next.table
                    .as_ref()
                    .is_none_or(|next_row| next_row.grid.source_start != row.grid.source_start)
            });
            if last_table_row {
                table_resize_handles.push(TableResizeHandle {
                    rect: Bounds::new(
                        point(right - px(5.), bottom - px(5.)),
                        size(px(10.), px(10.)),
                    ),
                    hitbox: None,
                    table: row.grid.source_start,
                    kind: TableResizeKind::Corner,
                    widths,
                    max_width,
                    row: row.source_start,
                    row_height: block.line_height,
                    row_min_height: row.natural_height,
                });
            }
        }
        for handle in &mut table_resize_handles {
            handle.hitbox = Some(window.insert_hitbox(handle.rect, HitboxBehavior::Normal));
        }

        DocLayout {
            blocks,
            widgets,
            origin: bounds.origin,
            line_height: (base_size.max(mono_size) * 1.5).max(LINE_HEIGHT),
            row_starts,
            row_tops,
            caret_row,
            task_boxes,
            table_resize_handles,
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

        // hitbox 已在 prepaint 注册；paint 阶段只设置水平/垂直/对角 resize 光标。
        for handle in &layout.table_resize_handles {
            if let Some(hitbox) = &handle.hitbox {
                window.set_cursor_style(handle.kind.cursor(), hitbox);
            }
        }

        // 回存几何供鼠标命中测试（`DocView::hit_source`），同时做两件事：
        // ① 内容高度回填（request_layout 用它上报可滚动高度，变化时补一帧收敛）；
        // ② 光标滚动跟随：仅当光标行几何变化时介入，避免与手动滚动手动打架。
        let layout_for_view = layout.clone();
        let total_height = layout.total_height();
        let caret_target = layout.caret_row;
        // 大纲跳转：把目标源偏移映射到视觉行，再取该行顶边/行高。
        // 必须在 `defer` 前算好，闭包拿走 layout 后就无法再查投影。
        let jump_target = self.view.update(cx, |v, _| {
            let src = v.scroll_to_source_top?;
            let projection = v.doc.projection();
            let pos = projection.source_to_doc_pos(src);
            let row = *layout.row_starts.get(pos.line)?;
            Some((layout.row_top(row), layout.block_line_height(pos.line)))
        });
        let view = self.view.clone();
        window.defer(cx, move |_, cx| {
            view.update(cx, |v, cx| {
                v.layout = Some(layout_for_view);
                if v.content_height != total_height {
                    v.content_height = total_height;
                    cx.notify(); // 高度变了：重排滚动区（内容超视口才会有滚动量）
                }
                if let Some((top, row_h)) = jump_target {
                    let sc = v.scroll.clone();
                    let viewport_h = sc.bounds().size.height;
                    if viewport_h > Pixels::ZERO {
                        // 用内容总高自己算上限，避免首次布局时 max_offset 尚未回填。
                        let max_scroll = (total_height - viewport_h).max(Pixels::ZERO);
                        let want = (-top).clamp(-max_scroll, Pixels::ZERO);
                        sc.set_offset(point(sc.offset().x, want));
                        v.scroll_to_source_top = None;
                        v.caret_scroll_key = Some((top, row_h));
                        cx.notify();
                    }
                } else if let Some((top, row_h)) = caret_target
                    && v.caret_scroll_key != Some((top, row_h))
                {
                    let sc = v.scroll.clone();
                    let viewport_h = sc.bounds().size.height;
                    if viewport_h > Pixels::ZERO {
                        let cur = sc.offset(); // y ∈ [-max_offset, 0]
                        let max_scroll = (total_height - viewport_h).max(Pixels::ZERO);
                        let want = if v.settings.typewriter_mode {
                            // 打字机模式（D68）：尽量将当前光标行居中。
                            let center = top + row_h / 2.;
                            -(center - viewport_h / 2.)
                        } else {
                            const MARGIN: Pixels = px(8.);
                            // 行完全落在上下留白内才不动；否则把它拉回留白内（最少滚动）
                            if top + cur.y > viewport_h - MARGIN - row_h {
                                -(top + row_h + MARGIN - viewport_h)
                            } else if top + cur.y < MARGIN {
                                -(top - MARGIN)
                            } else {
                                cur.y
                            }
                        };
                        let want = want.clamp(-max_scroll, Pixels::ZERO);
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
                // 面板右缘与正文右边距对齐；左侧只向外扩 8px 形成描边留白，
                // 不能再用 `+16px`，否则右侧会顶出编辑区边界。
                let right = content_right.max(left + px(24.));
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
                                let line_height = layout.block_line_height(w.line_range.start);
                                paint_corner_label(
                                    label,
                                    right - px(8.),
                                    top,
                                    line_height,
                                    window,
                                    cx,
                                );
                            }
                        }
                    }
                    // 图表块：弱底 + 外框 + 右上角语言标签（真渲染留渲染层，R4）
                    WidgetDecorKind::Chart { label } if !w.editing => {
                        window.paint_quad(fill(rect, palette.subtle));
                        paint_rect_border(window, rect, palette.border, px(1.));
                        if let Some(label) = label {
                            let line_height = layout.block_line_height(w.line_range.start);
                            paint_corner_label(label, right, top, line_height, window, cx);
                        }
                    }
                    // 显示公式：整体描边（公式体文字自身为斜体主题色）
                    WidgetDecorKind::Math if !w.editing => {
                        paint_rect_border(window, rect, palette.border, px(1.));
                    }
                    // 表格：弱底 + 表头带 + 外框 + 行列网格线。
                    // 网格模式下即使光标在表内也保留框线（编辑时仍要看清列边界）；
                    // 放不下而回退流式排版时，沿用旧的"编辑态隐藏装饰"行为。
                    WidgetDecorKind::Table => {
                        let grid = layout
                            .blocks
                            .get(w.line_range.start)
                            .and_then(|b| b.table.as_ref())
                            .map(|r| r.grid.clone());
                        if let Some(grid) = grid {
                            let left = layout.origin.x + w.indent;
                            let right = left + grid.width;
                            let rect = bounds(point(left, top), size(right - left, bottom - top));
                            window.paint_quad(fill(rect, palette.subtle));
                            let header_end = (w.line_range.start + 2).min(w.line_range.end);
                            if header_end > w.line_range.start {
                                let y0 = layout.origin.y
                                    + layout.row_top(layout.row_starts[w.line_range.start]);
                                let y1 =
                                    layout.origin.y + layout.row_top(layout.row_starts[header_end]);
                                window.paint_quad(fill(
                                    bounds(point(left, y0), size(right - left, y1 - y0)),
                                    Hsla {
                                        a: 0.35,
                                        ..palette.muted
                                    },
                                ));
                            }
                            paint_rect_border(window, rect, palette.border, px(1.));
                            // 行分隔线（表体每行顶边）
                            for line in (w.line_range.start + 2)..w.line_range.end {
                                let y = layout.origin.y + layout.row_top(layout.row_starts[line]);
                                window.paint_quad(fill(
                                    bounds(point(left, y), size(right - left, px(1.))),
                                    palette.border,
                                ));
                            }
                            // 竖向网格线：内部列边界（外框已画首尾）
                            for b in grid
                                .boundaries
                                .iter()
                                .skip(1)
                                .take(grid.boundaries.len() - 2)
                            {
                                let x = left + *b;
                                window.paint_quad(fill(
                                    bounds(point(x, top), size(px(1.), bottom - top)),
                                    palette.border,
                                ));
                            }
                        } else if !w.editing {
                            window.paint_quad(fill(rect, palette.subtle));
                            let header_end = (w.line_range.start + 2).min(w.line_range.end);
                            if header_end > w.line_range.start {
                                let y0 = layout.origin.y
                                    + layout.row_top(layout.row_starts[w.line_range.start]);
                                let y1 =
                                    layout.origin.y + layout.row_top(layout.row_starts[header_end]);
                                window.paint_quad(fill(
                                    bounds(point(left, y0), size(right - left, y1 - y0)),
                                    Hsla {
                                        a: 0.35,
                                        ..palette.muted
                                    },
                                ));
                            }
                            paint_rect_border(window, rect, palette.border, px(1.));
                            for line in (w.line_range.start + 2)..w.line_range.end {
                                let y = layout.origin.y + layout.row_top(layout.row_starts[line]);
                                window.paint_quad(fill(
                                    bounds(point(left, y), size(right - left, px(1.))),
                                    palette.border,
                                ));
                            }
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
            // 代码块文本整体右移 12px 让出面板内边距；光标/选区必须跟文本同一起点，
            // 否则光标会压在左侧字符上、行末光标也停在最后一个字之前（只差 12px）。
            let mut text_x = line_x;
            if block.proj.block.code_block {
                text_x += CODE_TEXT_INSET;
            }

            // 引用块：所有层级都画左竖线和背景；仅最外层加上下内边距，
            // 避免嵌套引用之间产生多余的间隔分隔。
            // 源码模式下显示原始 `>` 文本，不绘制引用装饰。
            if !self.view.read(cx).settings.source_mode && block.proj.block.quote_depth > 0 {
                let is_outer = block.proj.block.quote_depth == 1;
                let pad_y = if is_outer { px(2.) } else { px(0.) };
                // 底色只在正文文本列内留 4px 内缩：左缘 = 正文左缘 + 4px，
                // 右缘 = 正文右缘 - 4px。旧实现相对整个元素边界，左侧会伸到
                // 正文左缘之外、右侧顶出正文右边距。
                let left = layout.origin.x + BASE + QUOTE_BAND_INSET;
                let right =
                    element_bounds.origin.x + element_bounds.size.width - BASE - QUOTE_BAND_INSET;
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

            // 独占行图片：先铺弱底（透明 PNG 也能看清边界），再绘制真实图像。
            // 光标进入该行时 `image` 为 `None`，改为绘制整行源码（见 raw_src）。
            if let Some(image) = &block.image {
                let rect = bounds(point(text_x, row_top + IMAGE_VERTICAL_PAD), image.size);
                window.paint_quad(fill(rect, palette.subtle));
                let _ = window.paint_image(
                    rect,
                    rect,
                    Corners {
                        top_left: IMAGE_CORNER_RADIUS,
                        top_right: IMAGE_CORNER_RADIUS,
                        bottom_right: IMAGE_CORNER_RADIUS,
                        bottom_left: IMAGE_CORNER_RADIUS,
                    },
                    image.data.clone(),
                    0,
                    false,
                );
                paint_rect_border(window, rect, palette.border, px(1.));
            }

            // 独占行音视频：视频帧/海报/占位卡片 + 底部控制条与进度。
            if let Some(media) = &block.media {
                let rect = media.rect;
                let player = self
                    .view
                    .read(cx)
                    .media
                    .get(&media.key)
                    .and_then(|p| p.player.as_ref());
                let frame = player.and_then(|p| p.frame().cloned());
                let bar = bounds(
                    point(rect.origin.x, rect.bottom() - MEDIA_CONTROL_HEIGHT),
                    size(rect.size.width, MEDIA_CONTROL_HEIGHT),
                );
                match media.kind {
                    MediaKind::Video => {
                        window.paint_quad(fill(rect, palette.code_block_bg));
                        if let Some(frame) = frame {
                            let _ = window.paint_image(
                                rect,
                                rect,
                                Corners {
                                    top_left: IMAGE_CORNER_RADIUS,
                                    top_right: IMAGE_CORNER_RADIUS,
                                    bottom_right: IMAGE_CORNER_RADIUS,
                                    bottom_left: IMAGE_CORNER_RADIUS,
                                },
                                frame,
                                0,
                                false,
                            );
                        } else if let Some(poster) = &media.poster {
                            let _ = window.paint_image(
                                rect,
                                rect,
                                Corners {
                                    top_left: IMAGE_CORNER_RADIUS,
                                    top_right: IMAGE_CORNER_RADIUS,
                                    bottom_right: IMAGE_CORNER_RADIUS,
                                    bottom_left: IMAGE_CORNER_RADIUS,
                                },
                                poster.clone(),
                                0,
                                false,
                            );
                        }
                        // 控制条压在视频底部：半透明底保证文字在亮画面上可读。
                        window.paint_quad(fill(
                            bar,
                            Hsla {
                                a: 0.72,
                                ..palette.background
                            },
                        ));
                        let label_y = bar.origin.y + (bar.size.height - px(16.)) / 2.;
                        let _ = media.label.paint(
                            point(rect.origin.x + px(8.), label_y),
                            px(20.),
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        );
                    }
                    MediaKind::Audio => {
                        let playing = player.is_some_and(|p| p.playing());
                        let background = if playing {
                            Hsla {
                                a: 0.18,
                                ..palette.accent
                            }
                        } else {
                            palette.subtle
                        };
                        window.paint_quad(
                            fill(rect, background)
                                .corner_radii(AUDIO_BUTTON_SIZE / 2.)
                                .border_widths(px(1.))
                                .border_color(palette.border),
                        );
                        let label_x = rect.origin.x + (rect.size.width - media.label.width()) / 2.;
                        let _ = media.label.paint(
                            point(label_x, rect.origin.y + px(13.)),
                            px(24.),
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        );
                        if let Some(duration) = &media.duration_label {
                            let _ = duration.paint(
                                point(rect.right() + px(10.), rect.origin.y + px(13.)),
                                px(24.),
                                TextAlign::Left,
                                None,
                                window,
                                cx,
                            );
                        }
                        if media.loading {
                            // 四点循环表示 AVPlayer 首次加载/网络缓冲；100ms 轮询负责刷新。
                            let phase = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map_or(0, |d| (d.as_millis() / 180 % 4) as usize);
                            let center = point(
                                rect.origin.x + rect.size.width / 2.,
                                rect.origin.y + rect.size.height / 2.,
                            );
                            let dots = [
                                point(center.x, rect.origin.y - px(3.)),
                                point(rect.right() + px(3.), center.y),
                                point(center.x, rect.bottom() + px(3.)),
                                point(rect.origin.x - px(3.), center.y),
                            ];
                            for (index, dot) in dots.into_iter().enumerate() {
                                let diameter = if index == phase { px(6.) } else { px(4.) };
                                let dot_rect = bounds(
                                    point(dot.x - diameter / 2., dot.y - diameter / 2.),
                                    size(diameter, diameter),
                                );
                                let color = if index == phase {
                                    palette.accent
                                } else {
                                    palette.border
                                };
                                window
                                    .paint_quad(fill(dot_rect, color).corner_radii(diameter / 2.));
                            }
                        }
                    }
                }
                if media.kind == MediaKind::Video {
                    // 视频进度条：底轨 + 已播放比例；点击命中区在 `media.progress`。
                    window.paint_quad(fill(media.progress, palette.border));
                    let fraction = if media.duration > 0.0 {
                        (media.position / media.duration).clamp(0.0, 1.0) as f32
                    } else {
                        0.0
                    };
                    let filled = media.progress.size.width * fraction;
                    if filled > px(0.) {
                        window.paint_quad(fill(
                            bounds(
                                media.progress.origin,
                                size(filled, media.progress.size.height),
                            ),
                            palette.accent,
                        ));
                    }
                    paint_rect_border(window, rect, palette.border, px(1.));
                }
            }

            // 选区高亮：`selection` 是本块的显示列区间；按视觉行拆成多段矩形
            if let Some(cols) = &block.selection {
                let b0 = block.byte_of_col(cols.start);
                let b1 = block.byte_of_col(cols.end);
                if b1 > b0 {
                    if let Some(row) = &block.table {
                        // 表格单元格范围相对"显示文本"（不含引用前缀），故重算起止字节
                        let b0 = block.proj.display_byte_of_col(cols.start);
                        let b1 = block.proj.display_byte_of_col(cols.end);
                        // 表格：逐单元格、逐视觉行画高亮（竖线/内边距不参与选区）
                        for cell in &row.cells {
                            let s = b0.max(cell.display.start).min(cell.display.end);
                            let e = b1.max(cell.display.start).min(cell.display.end);
                            if e <= s {
                                continue;
                            }
                            for part in &cell.parts {
                                let part_s = s.max(part.display.start).min(part.display.end);
                                let part_e = e.max(part.display.start).min(part.display.end);
                                if part_e <= part_s {
                                    continue;
                                }
                                let rb = row_byte_bounds(&part.shaped);
                                for r in 0..rb.len().saturating_sub(1) {
                                    let rs = part.display.start + rb[r];
                                    let re = part.display.start + rb[r + 1];
                                    let ss = part_s.max(rs);
                                    let ee = part_e.min(re);
                                    if ee <= ss {
                                        continue;
                                    }
                                    let p0 = part
                                        .shaped
                                        .position_for_index(
                                            ss - part.display.start,
                                            row.line_height,
                                        )
                                        .unwrap_or_else(|| point(Pixels::ZERO, Pixels::ZERO));
                                    let p1 = part
                                        .shaped
                                        .position_for_index(
                                            ee - part.display.start,
                                            row.line_height,
                                        )
                                        .unwrap_or(p0);
                                    let width = (p1.x - p0.x).max(Pixels::ZERO);
                                    let align_x = wrapped_row_align_offset(
                                        &part.shaped,
                                        r,
                                        cell.inner_width,
                                        cell.align,
                                    );
                                    window.paint_quad(fill(
                                        bounds(
                                            point(
                                                text_x + cell.x + align_x + p0.x,
                                                row_top
                                                    + row.offset_y
                                                    + part.y
                                                    + row.line_height * r as f32,
                                            ),
                                            size(width, row.line_height),
                                        ),
                                        Hsla {
                                            h: 0.61,
                                            s: 0.9,
                                            l: 0.55,
                                            a: 0.32,
                                        },
                                    ));
                                }
                            }
                        }
                    } else {
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
                                bounds(point(text_x + x0, y), size(width, line_px)),
                                Hsla {
                                    h: 0.61,
                                    s: 0.9,
                                    l: 0.55,
                                    a: 0.32,
                                },
                            ));
                        }
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
            // 源码模式下显示原始 `[ ]`/`[x]` 文本，不绘制复选框 UI。
            if !self.view.read(cx).settings.source_mode && block.proj.block.task.is_some() {
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

            // 文本本体：网格表格逐单元格绘制，普通行由 `WrappedLine::paint` 按换行绘制
            if let Some(row) = &block.table {
                for cell in &row.cells {
                    for part in &cell.parts {
                        let origin = point(text_x + cell.x, row_top + row.offset_y + part.y);
                        let _ = part.shaped.paint(
                            origin,
                            row.line_height,
                            table_text_align(cell.align),
                            Some(Bounds::new(
                                origin,
                                size(cell.inner_width, row.line_height * part.rows as f32),
                            )),
                            window,
                            cx,
                        );
                    }
                }
            } else {
                let _ = block.shaped.paint(
                    point(text_x, row_top),
                    line_px,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
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

            // 光标竖线：仅在焦点在编辑区且处于闪烁可见相位时绘制。
            let (cursor_visible, focused) = {
                let view = self.view.read(cx);
                (view.cursor_visible, view.focus.is_focused(window))
            };
            if cursor_visible
                && focused
                && let Some(col) = block.caret_col
                && let Some(p) = block.position_of_col(col)
            {
                // p.y = 光标所在视觉行在块内的顶边偏移（软换行下不再画错行）
                // 表格单元格是单行 shape，光标高度按基准行高，避免表头行加高后光标过长
                let caret_h = if let Some(row) = &block.table {
                    row.line_height
                } else {
                    line_px
                };
                let caret_rect = bounds(
                    point(text_x + p.x, row_top + p.y + px(2.)),
                    size(px(2.), caret_h - px(4.)),
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

/// 把编译进二进制的应用图标设为 macOS Dock 图标。
///
/// `cargo run` 启动的是裸可执行文件，没有 `.app/Contents/Resources`，系统默认显示通用
/// 可执行图标；这里直接通过 AppKit 设置 `NSApplication.applicationIconImage`。
#[cfg(target_os = "macos")]
fn install_app_icon() {
    use objc2::{AnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let data = NSData::with_bytes(include_bytes!("../assets/appicon.png"));
    let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) else {
        return;
    };
    // 图标必须在主线程设置；调用点位于 GPUI 的主线程回调。
    unsafe {
        NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&image));
    }
}

#[cfg(not(target_os = "macos"))]
fn install_app_icon() {}

fn main() {
    // 可选参数：要打开的 .md 路径；缺省时渲染内置示例
    let path = std::env::args().nth(1).map(PathBuf::from);
    let settings = Settings::load();
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

    // GPUI `application()` 默认装的是 NullHttpClient；远程图片必须显式换成 reqwest。
    let http_client =
        reqwest_client::ReqwestClient::user_agent(concat!("Mazha/", env!("CARGO_PKG_VERSION")))
            .expect("初始化 HTTP 客户端失败");
    application()
        .with_http_client(Arc::new(http_client))
        .run(move |cx| {
            install_app_icon();
            gpui_kit::init(cx);
            // Tab/Shift+Tab 在编辑器内由本应用处理；Root 的全局 tab 绑定会吞掉按键
            // （命中绑定后 gpui 不再调用 on_key_down），故必须用更具体的 key context 覆盖。
            cx.bind_keys([
                KeyBinding::new("tab", InsertTab, Some("mazha-doc")),
                KeyBinding::new("shift-tab", Outdent, Some("mazha-doc")),
            ]);
            cx.bind_keys(menu_key_bindings());
            cx.set_menus(app_menus(&settings));
            let path = path.clone();
            let settings = settings.clone();
            gpui_kit::open_window(options, cx, move |window, cx| {
                apply_theme_preference(settings.theme, window, cx);
                let view = cx.new(|cx| DocView::new(path.as_ref(), settings.clone(), window, cx));
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
        BASE, BlockStyle, CODE_RIGHT_INSET, CODE_TEXT_INSET, DisplayPos, Document,
        QUOTE_BAND_INSET, QUOTE_TEXT_RIGHT_INSET, RunStyle, Settings, app_menus,
        block_line_px_with_mono, char_col_to_byte, clipboard_markdown, code_block_wrap_width,
        copy_entry, dropped_link_markdown, footnote_jump, horizontal_step, inline_links,
        inline_reveals, is_image_path, is_markdown_file, markdown_image_path, nearest_cell,
        oblique, plan_imports, quote_block_wrap_width, reveal_inline, table_arrow_step,
        table_entry_snap, unique_import_path,
    };
    use super::{decode_data_image, image_resource, px, size};
    use gpui_kit::{
        ClipboardEntry, ClipboardItem, ExternalPaths, Image, ImageFormat, MenuItem, Resource,
    };
    use smallvec::smallvec;
    use std::collections::HashSet;
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    #[test]
    fn app_menus_include_standard_groups_and_theme_checks() {
        let menus = app_menus(&Settings::default());
        let names: Vec<_> = menus.iter().map(|menu| menu.name.as_ref()).collect();
        assert_eq!(names, ["码札", "文件", "编辑", "视图", "窗口"]);

        let theme_menu = menus[3]
            .items
            .iter()
            .find_map(|item| match item {
                MenuItem::Submenu(menu) if menu.name == "主题" => Some(menu),
                _ => None,
            })
            .expect("视图菜单应包含主题子菜单");
        match &theme_menu.items[0] {
            MenuItem::Action {
                name,
                checked,
                disabled,
                ..
            } => {
                assert_eq!(name.as_ref(), "跟随系统");
                assert!(*checked);
                assert!(!*disabled);
            }
            _ => panic!("主题子菜单首项应为跟随系统"),
        }
    }

    #[test]
    fn clipboard_image_paths_are_inserted_as_markdown_paths() {
        let item = ClipboardItem {
            entries: vec![ClipboardEntry::ExternalPaths(ExternalPaths(smallvec![
                PathBuf::from("/tmp/a photo.PNG"),
                PathBuf::from("/tmp/readme.txt"),
                PathBuf::from("/tmp/b]c.svg"),
            ]))],
        };
        assert_eq!(
            clipboard_markdown(&item).as_deref(),
            Some("![a photo.PNG](</tmp/a photo.PNG>)\n![b\\]c.svg](</tmp/b]c.svg>)")
        );
        assert!(is_image_path(Path::new("cover.JpEg")));
        assert!(!is_image_path(Path::new("cover.md")));
    }

    #[test]
    fn clipboard_memory_image_becomes_base64_data_uri() {
        let item = ClipboardItem {
            entries: vec![ClipboardEntry::Image(Image {
                format: ImageFormat::Png,
                bytes: b"PNG".to_vec(),
                id: 1,
            })],
        };
        assert_eq!(
            clipboard_markdown(&item).as_deref(),
            Some("![图片](data:image/png;base64,UE5H)")
        );
    }

    #[test]
    fn clipboard_path_image_wins_and_non_image_falls_back_to_text() {
        let with_image = ClipboardItem {
            entries: vec![
                ClipboardEntry::Image(Image {
                    format: ImageFormat::Png,
                    bytes: b"PNG".to_vec(),
                    id: 1,
                }),
                ClipboardEntry::ExternalPaths(ExternalPaths(smallvec![PathBuf::from(
                    "/tmp/a.png",
                )])),
            ],
        };
        assert_eq!(
            clipboard_markdown(&with_image).as_deref(),
            Some("![a.png](</tmp/a.png>)")
        );

        let text = ClipboardItem::new_string("普通文本".to_string());
        assert_eq!(clipboard_markdown(&text).as_deref(), Some("普通文本"));
        assert_eq!(
            markdown_image_path(Path::new("/tmp/a>b%.png")),
            "![a>b%.png](</tmp/a%3Eb%25.png>)"
        );
    }

    /// 导入规划：同名追加序号且不互相覆盖，已在目标目录与复制到自身内部的条目跳过。
    #[test]
    fn import_plan_avoids_collisions_and_self_copy() {
        let target = tempfile::tempdir().expect("临时目录");
        let target_root = target.path();
        std::fs::write(target_root.join("note.md"), b"old").expect("写入已存在文件");
        let source = tempfile::tempdir().expect("临时目录");
        let source_root = source.path();
        std::fs::write(source_root.join("note.md"), b"new").expect("写入来源文件");
        std::fs::write(source_root.join("other.txt"), b"x").expect("写入来源文件");
        std::fs::create_dir(source_root.join("assets")).expect("创建来源目录");

        let plan = plan_imports(
            &[
                source_root.join("note.md"),
                source_root.join("other.txt"),
                source_root.join("note.md"),
                source_root.join("assets"),
                target_root.join("note.md"),
            ],
            target_root,
        );
        let destinations: Vec<_> = plan.iter().map(|(_, dst)| dst.as_path()).collect();
        assert_eq!(
            destinations,
            [
                target_root.join("note 2.md").as_path(),
                target_root.join("other.txt").as_path(),
                target_root.join("note 3.md").as_path(),
                target_root.join("assets").as_path(),
            ]
        );

        // 多扩展名文件只在最后一段扩展名前追加序号。
        let taken = HashSet::new();
        assert_eq!(
            unique_import_path(target_root, OsStr::new("draft.v1.md"), &taken),
            target_root.join("draft.v1.md")
        );
        std::fs::write(target_root.join("draft.v1.md"), b"old").expect("写入已存在文件");
        assert_eq!(
            unique_import_path(target_root, OsStr::new("draft.v1.md"), &taken),
            target_root.join("draft.v1 2.md")
        );

        // 目录不能复制进自身，避免无限递归。
        let nested = target_root.join("nested");
        std::fs::create_dir(&nested).expect("创建目录");
        assert!(plan_imports(&[nested.clone()], &nested).is_empty());
    }

    /// 递归复制保留目录结构与文件内容，且不动来源（Finder 原件必须保留）。
    #[test]
    fn copy_entry_preserves_source_and_nested_files() {
        let source = tempfile::tempdir().expect("临时目录");
        let target = tempfile::tempdir().expect("临时目录");
        let src_dir = source.path().join("assets");
        std::fs::create_dir(&src_dir).expect("创建来源目录");
        std::fs::write(src_dir.join("pic.txt"), b"image").expect("写入来源文件");
        std::fs::write(source.path().join("note.md"), b"# note").expect("写入来源文件");

        let dst_dir = target.path().join("assets");
        copy_entry(&src_dir, &dst_dir).expect("复制目录");
        assert_eq!(
            std::fs::read_to_string(dst_dir.join("pic.txt")).expect("读取复制文件"),
            "image"
        );
        assert_eq!(
            std::fs::read_to_string(src_dir.join("pic.txt")).expect("读取原文件"),
            "image"
        );
        assert!(is_markdown_file(Path::new("a.MARKDOWN")));
        assert!(!is_markdown_file(Path::new("a.txt")));
    }

    /// 拖入链接：工作区内写相对路径并区分图片/普通文件，工作区外保留绝对路径。
    #[test]
    fn dropped_links_use_relative_paths_when_possible() {
        let dir = tempfile::tempdir().expect("临时目录");
        let base = dir.path().join("notes");
        std::fs::create_dir(&base).expect("创建目录");
        let image = base.join("sub").join("a photo.PNG");
        let attachment = base.join("report.pdf");
        assert_eq!(
            dropped_link_markdown(&image, Some(&base)),
            "![a photo.PNG](<sub/a photo.PNG>)"
        );
        assert_eq!(
            dropped_link_markdown(&attachment, Some(&base)),
            "[report.pdf](<report.pdf>)"
        );

        let outside = tempfile::tempdir().expect("临时目录");
        let path = outside.path().join("other.pdf");
        assert_eq!(
            dropped_link_markdown(&path, Some(&base)),
            format!("[other.pdf](<{}>)", path.to_string_lossy())
        );
    }

    /// 行高取正文与等宽字号的较大者；代码块只按自身等宽字号计算。
    #[test]
    fn block_line_height_accounts_for_larger_mono_font() {
        let body = BlockStyle::default();
        assert_eq!(block_line_px_with_mono(&body, px(14.), px(20.)), px(30.));
        assert_eq!(block_line_px_with_mono(&body, px(16.), px(13.)), px(24.));

        let code = BlockStyle {
            code_block: true,
            ..BlockStyle::default()
        };
        assert_eq!(block_line_px_with_mono(&code, px(24.), px(10.)), px(36.));
    }

    /// 引用底色与文字都必须落在正文文本列内，左右不越界。
    #[test]
    fn quote_block_stays_inside_text_column() {
        let content_width = px(800.);
        let indent = px(32.);
        let wrap = quote_block_wrap_width(content_width, indent);
        let content_left = BASE;
        let content_right = content_width - BASE;
        let band_left = content_left + QUOTE_BAND_INSET;
        let band_right = content_right - QUOTE_BAND_INSET;
        let text_left = indent;
        let text_right = text_left + wrap;
        assert!(band_left > content_left && band_right < content_right);
        assert!(text_left >= band_left && text_right <= band_right);
        assert_eq!(text_right, content_right - QUOTE_TEXT_RIGHT_INSET);
    }

    /// 代码块长行右缘不能越过面板右缘（面板右缘 = 正文右边距）。
    #[test]
    fn code_block_text_stays_inside_panel() {
        let content_width = px(800.);
        let indent = px(24.);
        let wrap = code_block_wrap_width(content_width, indent);
        let panel_right = content_width - BASE;
        let text_right = indent + CODE_TEXT_INSET + wrap;
        assert_eq!(text_right, panel_right - CODE_RIGHT_INSET);
        assert!(text_right <= panel_right);
    }

    /// 表格内左右方向键要跳过竖线/填充空格/分隔行，不能出现"按了没反应"。
    #[test]
    fn table_arrows_skip_hidden_gaps_and_wrap_rows() {
        let text = "| a | b |\n| --- | --- |\n| c | d |\n\n尾";
        let mut doc = Document::new(text);
        doc.projection();
        let body = text.find("| c").expect("表体行");
        // 表头：格内逐字符，格尾一步跳到下一格首字符
        assert_eq!(table_arrow_step(&mut doc, 3, true), Some(6));
        assert_eq!(table_arrow_step(&mut doc, 6, false), Some(3));
        // 表头末格 → 跳过隐藏分隔行 → 表体首格
        assert_eq!(table_arrow_step(&mut doc, 7, true), Some(body + 2));
        assert_eq!(table_arrow_step(&mut doc, body + 2, false), Some(7));
        // 表体末格 → 表格外下一逻辑行行首
        assert_eq!(table_arrow_step(&mut doc, body + 7, true), Some(body + 10));
        // 表格外/普通行不接管
        assert_eq!(table_arrow_step(&mut doc, body + 10, true), None);

        // 只有表头+分隔行的表：末格右移应越过分隔行，落到表格后的空行
        let text = "| a | b |\n| --- | --- |\n\n尾";
        let mut doc = Document::new(text);
        doc.projection();
        let blank = text.find("\n\n").expect("空行") + 1;
        assert_eq!(table_arrow_step(&mut doc, 7, true), Some(blank));
    }

    /// 从表格外横移进入表格：第一下就应落在单元格内，不能停在首/末竖线外。
    #[test]
    fn arrow_enters_table_inside_cells_on_first_press() {
        let text = "para\n\n| a | b |\n| --- | --- |\n| c | d |\ntail";
        let mut doc = Document::new(text);
        doc.projection();
        let header = text.find("| a").expect("表头");
        let body = text.find("| c").expect("表体");
        let blank = text.find("\n\n").expect("空行") + 1;
        let tail = text.find("tail").expect("尾行");

        // 空行右移：普通移动先到表头行首，吸附到首格文本起点
        assert_eq!(table_entry_snap(&mut doc, header), Some(header + 2));
        assert_eq!(horizontal_step(&mut doc, blank, true), header + 2);
        // 尾行行首左移：普通移动先到表体行尾换行符，吸附到末格文本末尾
        assert_eq!(table_entry_snap(&mut doc, tail - 1), Some(body + 7));
        assert_eq!(horizontal_step(&mut doc, tail, false), body + 7);
    }

    /// 多字单元格末尾必须仍命中本格（不能因 `local` 大而误判为下一格）。
    #[test]
    fn table_hit_at_cell_end_stays_in_cell() {
        let cells = [2..8, 11..17, 20..26];
        assert_eq!(nearest_cell(cells.iter(), 8), Some((0, 6)));
        assert_eq!(nearest_cell(cells.iter(), 9), Some((0, 6)));
        assert_eq!(nearest_cell(cells.iter(), 10), Some((1, 0)));
        assert_eq!(nearest_cell(cells.iter(), 11), Some((1, 0)));
        // 单字单元格末尾同样属于本格
        let narrow = [1..2, 3..4];
        assert_eq!(nearest_cell(narrow.iter(), 2), Some((0, 1)));
    }

    #[test]
    fn table_breaks_split_cell_and_keep_empty_lines() {
        assert_eq!(
            super::table_cell_parts(2..8, &[4, 4, 6]),
            vec![2..4, 4..4, 4..6, 6..8]
        );
    }

    #[test]
    fn table_column_resize_preserves_pair_width() {
        let widths = [px(100.), px(120.), px(80.)];
        let resized = super::resized_table_columns(&widths, 0, px(30.)).unwrap();
        assert_eq!(resized, vec![px(130.), px(90.), px(80.)]);
        let clamped = super::resized_table_columns(&widths, 0, px(200.)).unwrap();
        assert_eq!(clamped, vec![px(172.), px(48.), px(80.)]);
    }

    #[test]
    fn table_outer_resize_respects_width_and_height_limits() {
        let widths = [px(100.), px(120.), px(80.)];
        let grown = super::resized_table_right_edge(&widths, px(50.), px(360.)).unwrap();
        assert_eq!(grown, vec![px(100.), px(120.), px(130.)]);
        let clamped = super::resized_table_right_edge(&widths, px(200.), px(320.)).unwrap();
        assert_eq!(clamped, vec![px(100.), px(120.), px(100.)]);
        assert_eq!(
            super::resized_table_row_height(px(80.), px(-60.), px(48.)),
            px(48.)
        );
        assert_eq!(
            super::resized_table_row_height(px(80.), px(20.), px(48.)),
            px(100.)
        );
    }

    #[test]
    fn table_alignment_offsets_left_center_and_right() {
        assert_eq!(
            super::table_alignment_offset(px(40.), px(100.), mazha_core::Align::Left),
            px(0.)
        );
        assert_eq!(
            super::table_alignment_offset(px(40.), px(100.), mazha_core::Align::Center),
            px(30.)
        );
        assert_eq!(
            super::table_alignment_offset(px(40.), px(100.), mazha_core::Align::Right),
            px(60.)
        );
    }

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

    /// 独占行图片才进入真实渲染；混排图片仍是行内占位。
    #[test]
    fn standalone_image_line_detection() {
        let p = mazha_core::build_projection("![图](assets/a.png)");
        assert_eq!(
            super::standalone_image_dest(&p.lines[0]).map(String::as_str),
            Some("assets/a.png")
        );

        let p = mazha_core::build_projection("前 ![图](assets/a.png) 后");
        assert!(super::standalone_image_dest(&p.lines[0]).is_none());

        let p = mazha_core::build_projection("![一](a.png) ![二](b.png)");
        assert!(super::standalone_image_dest(&p.lines[0]).is_none());
    }

    /// 独占行媒体生成播放卡片；与正文混排或同一行多个媒体时只显示行内占位。
    #[test]
    fn standalone_media_line_detection() {
        let p = mazha_core::build_projection("<video src=\"a.mp4\"></video>");
        let media = super::standalone_media(&p.lines[0]).expect("应识别独占视频");
        assert_eq!(media.kind, mazha_md::MediaKind::Video);
        assert_eq!(media.src, "a.mp4");

        let p = mazha_core::build_projection("前 <audio src=\"a.mp3\"></audio> 后");
        assert!(super::standalone_media(&p.lines[0]).is_none());

        let p = mazha_core::build_projection(
            "<audio src=\"a.mp3\"></audio> <audio src=\"b.mp3\"></audio>",
        );
        assert!(super::standalone_media(&p.lines[0]).is_none());
    }

    #[test]
    fn media_time_and_size_helpers_are_stable() {
        assert_eq!(super::AUDIO_BUTTON_SIZE, px(50.));
        assert_eq!(super::fmt_time(0.0), "0:00");
        assert_eq!(super::fmt_time(65.4), "1:05");
        assert_eq!(super::fmt_time(3_661.0), "1:01:01");
        assert_eq!(super::fmt_time(f64::NAN), "--:--");

        let natural = size(px(640.), px(360.));
        assert_eq!(super::fit_video_size(natural, px(800.)), natural);
        assert_eq!(
            super::fit_video_size(natural, px(320.)),
            size(px(320.), px(180.))
        );
    }

    /// 相对图片路径以文档目录为基准；远程/空地址不进入本地加载。
    #[test]
    fn image_path_resolution() {
        let doc = Path::new("/tmp/notes/post.md");
        assert_eq!(
            super::resolve_image_path("assets/a.png", Some(doc)),
            Some(PathBuf::from("/tmp/notes/assets/a.png"))
        );
        assert_eq!(
            super::resolve_image_path("assets/my photo.png", Some(doc)),
            Some(PathBuf::from("/tmp/notes/assets/my photo.png"))
        );
        assert_eq!(
            super::resolve_image_path("assets/my%20photo.png", Some(doc)),
            Some(PathBuf::from("/tmp/notes/assets/my photo.png"))
        );
        assert_eq!(
            super::resolve_image_path("file:///tmp/pic.png", Some(doc)),
            Some(PathBuf::from("/tmp/pic.png"))
        );
        assert_eq!(
            super::resolve_image_path("/tmp/pic.png", None),
            Some(PathBuf::from("/tmp/pic.png"))
        );
        assert!(super::resolve_image_path("https://example.com/a.png", Some(doc)).is_none());
        assert!(super::resolve_image_path("data:image/png;base64,xx", Some(doc)).is_none());
        assert!(super::resolve_image_path("", Some(doc)).is_none());
    }

    /// 远程图片走 HTTP 资源；协议相对地址默认补 `https:`，data URI 单独解码。
    #[test]
    fn remote_image_uses_uri_resource() {
        let doc = Path::new("/tmp/notes/post.md");
        assert_eq!(
            image_resource("https://example.com/a.png", Some(doc)),
            Some(Resource::Uri("https://example.com/a.png".into()))
        );
        assert_eq!(
            image_resource("//example.com/a.png", Some(doc)),
            Some(Resource::Uri("https://example.com/a.png".into()))
        );
        assert!(image_resource("data:image/png;base64,xx", Some(doc)).is_none());
    }

    /// data URI 支持 base64 与百分号编码，未知 MIME/坏 base64 回退占位。
    #[test]
    fn data_image_decodes_base64_and_percent() {
        let (format, bytes) = decode_data_image("data:image/png;base64,UE5H").unwrap();
        assert_eq!(format, ImageFormat::Png);
        assert_eq!(bytes, b"PNG");

        let (format, bytes) =
            decode_data_image("data:image/jpeg;charset=utf-8,%89PNG%0D%0A").unwrap();
        assert_eq!(format, ImageFormat::Jpeg);
        assert_eq!(bytes, [0x89, b'P', b'N', b'G', b'\r', b'\n']);

        assert!(decode_data_image("data:text/plain;base64,UE5H").is_none());
        assert!(decode_data_image("data:image/png;base64,***").is_none());
        assert!(decode_data_image("data:image/png,").is_none());
    }

    /// 图片适配只缩小、不放大，并保持宽高比。
    #[test]
    fn image_fit_size_never_upscales() {
        let natural = size(gpui_kit::px(400.), gpui_kit::px(200.));
        let fitted = super::fit_image_size(natural, gpui_kit::px(800.));
        assert_eq!(fitted, natural);

        let fitted = super::fit_image_size(natural, gpui_kit::px(200.));
        assert_eq!(fitted, size(gpui_kit::px(200.), gpui_kit::px(100.)));
    }

    /// 百分号转义只还原合法 `%XX`，非法序列保持原样。
    #[test]
    fn percent_decode_handles_invalid_sequences() {
        assert_eq!(super::percent_decode("a%20b"), "a b");
        assert_eq!(super::percent_decode("100%"), "100%");
        assert_eq!(super::percent_decode("%ZZ"), "%ZZ");
    }

    /// 脚注 Cmd+点击：引用 → 定义块起点，定义 → 首个引用；未知标签/无关落点不跳转。
    #[test]
    fn footnote_jump_resolves_both_directions() {
        let src = "见[^a]。\n\n[^a]: note\n";
        let index = mazha_md::FootnoteIndex::new(src, &mazha_md::parse(src));
        let entry = index.get("a").expect("应有脚注 a");
        let reference = entry.references[0].start;
        let definition = entry.definition.as_ref().expect("应有定义").start;

        assert_eq!(footnote_jump(&index, "a", reference), Some(definition));
        assert_eq!(
            footnote_jump(&index, "a", definition),
            Some(reference),
            "点击定义块应回跳到首个引用"
        );
        assert_eq!(footnote_jump(&index, "missing", reference), None);
        assert_eq!(
            footnote_jump(&index, "a", definition - 1),
            None,
            "定义块之外的落点不跳转"
        );
    }

    /// 全工作区搜索：逐行命中、行号 1 基、源偏移跨行累计、大小写与 CJK 均正确。
    #[test]
    fn workspace_search_text_lines() {
        use super::search_text_lines;

        let text = "第一行 foo\n第二行 Foo 与 foo\n末行";
        let hits = search_text_lines(text, "foo", false);
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].line_no, 1);
        assert_eq!(hits[0].line, "第一行 foo");
        // 第一行 "第一行 " 为 3 个三字节字符 + 1 空格 = 10 字节
        assert_eq!(hits[0].range, 10..13);
        assert_eq!(hits[0].source, 10..13);

        // 第二行两个命中，源偏移 = 第一行（含换行）长度 + 行内偏移
        let line2_start = "第一行 foo\n".len();
        assert_eq!(hits[1].line_no, 2);
        assert_eq!(
            hits[1].source,
            line2_start + hits[1].range.start..line2_start + hits[1].range.end
        );
        assert_eq!(hits[2].line_no, 2);

        // 大小写敏感只命中全小写（第一行与第二行第二个 foo）
        let strict = search_text_lines(text, "foo", true);
        assert_eq!(strict.len(), 2);
        assert_eq!(strict[0].line_no, 1);
        assert_eq!(strict[1].line_no, 2);

        // 空查询/无命中返回空
        assert!(search_text_lines(text, "", false).is_empty());
        assert!(search_text_lines(text, "不存在", false).is_empty());
    }

    /// 全工作区搜索递归收集所有 Markdown（与展开状态无关），忽略非 Markdown。
    #[test]
    fn workspace_search_collects_nested_markdown() {
        use super::collect_markdown_paths;
        use mazha_core::Workspace;
        use mazha_store::{DocPath, FileSystemProvider};

        let dir = tempfile::TempDir::new().unwrap();
        let store = FileSystemProvider::open(dir.path()).unwrap();
        std::fs::write(dir.path().join("a.md"), "a").unwrap();
        std::fs::write(dir.path().join("skip.txt"), "x").unwrap();
        std::fs::create_dir_all(dir.path().join("sub/deep")).unwrap();
        std::fs::write(dir.path().join("sub/b.markdown"), "b").unwrap();
        std::fs::write(dir.path().join("sub/deep/c.md"), "c").unwrap();

        let ws = Workspace::new(store);
        let mut rels = Vec::new();
        collect_markdown_paths(&ws, &DocPath::root(), &mut rels);
        // 目录在前、名称排序：sub 先于 a.md，sub/deep 先于 sub/b.markdown。
        assert_eq!(rels, ["sub/deep/c.md", "sub/b.markdown", "a.md"]);
    }

    /// 预览截断按字符边界进行，不切坏 UTF-8，并在截断侧补省略号。
    #[test]
    fn workspace_search_preview_truncates_by_chars() {
        use super::{head_chars, tail_chars};
        assert_eq!(tail_chars("中文测试", 2), "…测试");
        assert_eq!(head_chars("中文测试", 2), "中文…");
        assert_eq!(tail_chars("abc", 5), "abc");
        assert_eq!(head_chars("abc", 5), "abc");
    }

    /// 文件树按层级递归：未展开的目录不显示子项，展开后子项带正确深度，
    /// 非 Markdown 文件被过滤。
    #[test]
    fn file_tree_rows_respect_expansion_and_depth() {
        use super::collect_tree_rows;
        use mazha_core::Workspace;
        use mazha_store::{DocPath, FileSystemProvider};
        use std::collections::HashSet;

        let dir = tempfile::TempDir::new().unwrap();
        let store = FileSystemProvider::open(dir.path()).unwrap();
        // 结构：a.md、sub/(deep/(c.md)、b.md)、img.png
        std::fs::write(dir.path().join("a.md"), "a").unwrap();
        std::fs::create_dir_all(dir.path().join("sub/deep")).unwrap();
        std::fs::write(dir.path().join("sub/b.md"), "b").unwrap();
        std::fs::write(dir.path().join("sub/deep/c.md"), "c").unwrap();
        std::fs::write(dir.path().join("img.png"), "x").unwrap();

        let ws = Workspace::new(store);
        let mut expanded = HashSet::new();

        // 根层：目录 sub 在前、随后 a.md；img.png 被过滤。
        let mut rows = Vec::new();
        collect_tree_rows(&ws, &DocPath::root(), 0, &expanded, &mut rows);
        let names: Vec<_> = rows.iter().map(|(_, e)| e.name.as_str()).collect();
        assert_eq!(names, ["sub", "a.md"]);
        assert!(rows.iter().all(|(d, _)| *d == 0));

        // 展开 sub：出现 deep 目录与 b.md（深度 1），deep 未展开故 c.md 不出现。
        expanded.insert("sub".to_string());
        let mut rows = Vec::new();
        collect_tree_rows(&ws, &DocPath::root(), 0, &expanded, &mut rows);
        let names: Vec<_> = rows.iter().map(|(_, e)| e.name.as_str()).collect();
        assert_eq!(names, ["sub", "deep", "b.md", "a.md"]);
        assert_eq!(rows[1].0, 1);
        assert_eq!(rows[2].0, 1);

        // 再展开 sub/deep：c.md 以深度 2 出现。
        expanded.insert("sub/deep".to_string());
        let mut rows = Vec::new();
        collect_tree_rows(&ws, &DocPath::root(), 0, &expanded, &mut rows);
        let names: Vec<_> = rows.iter().map(|(_, e)| e.name.as_str()).collect();
        assert_eq!(names, ["sub", "deep", "c.md", "b.md", "a.md"]);
        assert_eq!(rows[2].0, 2);
    }

    /// 拖拽落点合法性：不能拖到自身所在目录；目录不能拖进自身或其后代；其余允许。
    #[test]
    fn drop_allowed_rejects_self_and_descendants() {
        use super::drop_allowed;
        // 文件拖到任意不同目录都允许（含根目录 ""）。
        assert!(drop_allowed("a.md", false, "sub"));
        assert!(drop_allowed("sub/a.md", false, ""));
        // 拖到自身所在目录（原地不动）被拒绝。
        assert!(!drop_allowed("sub", true, "sub"));
        assert!(!drop_allowed("a.md", false, "a.md"));
        // 目录不能拖进自身或其后代。
        assert!(!drop_allowed("sub", true, "sub/deep"));
        assert!(!drop_allowed("sub", true, "sub/deep/more"));
        // 目录拖到根目录或兄弟目录允许。
        assert!(drop_allowed("sub", true, ""));
        assert!(drop_allowed("sub", true, "other"));
        // 前缀相近但非子孙（sub2 不是 sub 的后代）允许。
        assert!(drop_allowed("sub", true, "sub2"));
    }

    /// 目录重命名/移动后，展开集合里的旧前缀整体迁移为新前缀，子路径后缀保留。
    #[test]
    fn remap_expanded_moves_prefix_and_keeps_suffix() {
        use super::remap_expanded;
        use std::collections::HashSet;
        let mut set: HashSet<String> = ["sub", "sub/deep", "sub/deep/more", "other", "subx"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        remap_expanded(&mut set, "sub", "moved");
        // 旧前缀全部消失，新前缀保留子路径后缀。
        assert!(set.contains("moved"));
        assert!(set.contains("moved/deep"));
        assert!(set.contains("moved/deep/more"));
        // 不相干的项（other）与前缀相近项（subx）不受影响。
        assert!(set.contains("other"));
        assert!(set.contains("subx"));
        assert!(!set.contains("sub"));
        assert!(!set.contains("sub/deep"));
    }
}
