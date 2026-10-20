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
//! - **WYSIWYG 标记显隐**（SPEC §11.2）：光标所在行渲染**原始 Markdown**（标记可见），
//!   其余行渲染投影（标记隐藏）；
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

use std::cmp::Reverse;
use std::ops::Range;
use std::panic::Location;
use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::base::h_flex;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme, Theme};
use gpui_kit::prelude::*;
use gpui_kit::*;
use mazha_core::{
    BlockStyle, BlockTarget, BlockWidgetKind, ColoredRun, DisplayPos, DocPos, Document, Edit,
    Highlighter, InlineMark, ProjectedLine, Projection, RunStyle, Selection, TableOp,
    TextRun as DocTextRun, Workspace,
    commands::{set_block, toggle_inline},
    edit_table, find_all, replace_all_edit,
};
use mazha_export::to_pdf;
#[allow(unused_imports, dead_code)]
use mazha_store::{DocPath, FileSystemProvider};
use rfd::FileDialog;

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
    /// 弱底色（代码块、行内代码背景）。
    subtle: Hsla,
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
            // 选区底色：accent 的低透明度，跟随强调色随主题变化
            selection: Hsla {
                a: 0.25,
                ..theme.accent
            },
        }
    }
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
    /// 光标在本块时的行内**显示列**（`None` = 光标不在本块；仅一块为 `Some`）。
    ///
    /// 由 `Projection::source_to_doc_pos` 于 prepaint 阶段算好，paint 直接据此画竖线。
    caret_col: Option<usize>,
    /// 本块在当前选区内的**显示列**区间（`None` 表示本块不在选区内）。
    ///
    /// 由 `Projection::selection_display_ranges` 于 prepaint 阶段算好，paint 直接按
    /// 视觉行换算屏幕 x（经 `display_byte_of_col`）。
    selection: Option<Range<usize>>,
}

impl BlockGeom {
    /// 显示列 → 形状化文本内的字节偏移（含前缀）。
    fn byte_of_col(&self, col: usize) -> usize {
        self.prefix_bytes + self.proj.display_byte_of_col(col)
    }

    /// 形状化文本内的字节偏移 → 显示列（扣除前缀，越界钳到行末）。
    fn col_of_byte(&self, byte: usize) -> usize {
        self.proj
            .display_col_of_byte(byte.saturating_sub(self.prefix_bytes))
    }
}

/// 整篇文档的布局几何：块序列 + 光标原点 + 统一行高 + 视觉行累积。
///
/// 在 [`DocElement::prepaint`] 中构建，经 `cx.defer` 回存到 [`DocView`]，
/// 供鼠标命中测试在事件回调里读取。
#[derive(Clone)]
struct DocLayout {
    /// 块序列（与投影行一一对应，已跳过分割线等无文本块）。
    blocks: Vec<BlockGeom>,
    /// 块级 widget 的装饰（代码块/表格/分割线；SPEC §11.5、D30）。
    widgets: Vec<WidgetDecor>,
    /// 内容左上角（窗口坐标）。
    origin: Point<Pixels>,
    /// 统一行高。
    line_height: Pixels,
    /// 每个块**首视觉行**的全局行号；末位为总视觉行数（长度 = `blocks.len() + 1`）。
    ///
    /// 软换行下逻辑行 ≠ 视觉行：命中测试/绘制都以它把"全局视觉行"映射回块。
    row_starts: Vec<usize>,
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

impl DocLayout {
    /// 从窗口坐标命中一个**显示位置**（块序号 + 行内显示字符列）。
    ///
    /// 软换行下先由 `row_starts` 把全局视觉行二分定位到块，再取块内视觉行的相对坐标
    /// 交给 [`WrappedLine::closest_index_for_position`] 求字节偏移，最后换算成显示列。
    fn hit(&self, position: Point<Pixels>) -> Option<(usize, usize)> {
        if self.blocks.is_empty() {
            return None;
        }
        let total = *self.row_starts.last()?;
        if total == 0 {
            return None;
        }
        let rel_y = position.y - self.origin.y;
        let row = (rel_y / self.line_height).floor().max(0.) as usize;
        let row = row.min(total - 1);
        // row_starts[i] <= row < row_starts[i+1] → 块 i（首元素为 0，故减一后非负）
        let block_idx = self.row_starts.partition_point(|&s| s <= row) - 1;
        let block = &self.blocks[block_idx];
        let local_row = row - self.row_starts[block_idx];
        // 行内 x 需减去该块缩进；y 只要落在该视觉行内即可（取行中点）
        let rel_x = position.x - self.origin.x - block.indent;
        let local_y = self.line_height * local_row as f32 + self.line_height / 2.;
        let byte = block
            .shaped
            .closest_index_for_position(point(rel_x, local_y), self.line_height)
            .unwrap_or_else(|ix| ix);
        Some((block_idx, block.col_of_byte(byte)))
    }
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
    let byte = block.byte_of_col(pos.col);
    let p = block.shaped.position_for_index(byte, layout.line_height)?;
    let cur_row = (p.y / layout.line_height) as usize;
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
    let local_y = layout.line_height * local_row as f32 + layout.line_height / 2.;
    let tbyte = tblock
        .shaped
        .closest_index_for_position(point(local_x, local_y), layout.line_height)
        .unwrap_or_else(|ix| ix);
    let tcol = tblock.col_of_byte(tbyte);
    Some(tblock.proj.offset_map.display_to_source(DisplayPos(tcol)))
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
    /// 已打开文件的保存快照（脏状态用）
    saved_text: Option<String>,
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
            saved_text,
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
            let target_line = if step < 0 {
                pos.line.checked_sub(1)
            } else {
                (pos.line + 1 < projection.len()).then_some(pos.line + 1)
            };
            let new_caret = match target_line {
                Some(line) => projection.doc_pos_to_source(DocPos { line, col: pos.col }),
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

    /// 所有光标处插入普通字符（IME 提交/直接按键）。有选区则先删除选区。
    fn insert_char(&mut self, ch: char, cx: &mut Context<Self>) {
        for c in &mut self.cursors {
            let range = c.selection().unwrap_or(c.caret..c.caret);
            let at = range.start;
            let mut text = String::new();
            text.push(ch);
            if self.doc.apply(Edit::Replace { range, text }).is_ok() {
                c.caret = self.doc.snap_to_boundary(at + ch.len_utf8());
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

    /// 从窗口坐标命中一个**源字节偏移**（整篇命中：先算显示索引，再经 OffsetMap 映射）。
    fn hit_source(&self, position: Point<Pixels>) -> Option<usize> {
        let layout = self.layout.as_ref()?;
        let (row, col) = layout.hit(position)?;
        let block = &layout.blocks[row];
        // 显示字符列 → 源字节偏移
        let source_offset = block.proj.offset_map.display_to_source(DisplayPos(col));
        Some(
            self.doc
                .snap_to_boundary(source_offset.min(block.proj.source_range.end)),
        )
    }

    /// 导出当前文档为 PDF 并保存到文件。
    fn export_pdf(&mut self, _cx: &mut Context<Self>) {
        let markdown = self.doc.text();
        let result = to_pdf(&markdown);
        match result {
            Ok(pdf_doc) => {
                if let Some(path) = FileDialog::new()
                    .set_title("导出 PDF")
                    .add_filter("PDF 文件", &["pdf"])
                    .set_file_name("document.pdf")
                    .save_file()
                    && let Err(e) = std::fs::write(&path, &pdf_doc.bytes)
                {
                    eprintln!("导出 PDF 失败：{}", e);
                }
            }
            Err(e) => {
                eprintln!("PDF 编译失败：{}", e);
            }
        }
    }

    fn open_file_dialog(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = FileDialog::new()
            .set_title("打开 Markdown")
            .add_filter("Markdown", &["md", "markdown", "txt"])
            .pick_file()
        {
            self.open_path(path, cx);
        }
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

    fn save_current(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.path.clone() {
            if let Err(e) = std::fs::write(&path, self.doc.text().as_bytes()) {
                eprintln!("保存失败：{}", e);
            } else {
                self.saved_text = Some(self.doc.text().to_string());
                cx.notify();
            }
        } else {
            self.save_as(cx);
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

    fn save_as(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = FileDialog::new()
            .set_title("另存为")
            .add_filter("Markdown", &["md", "markdown"])
            .set_file_name("document.md")
            .save_file()
        {
            if let Err(e) = std::fs::write(&path, self.doc.text().as_bytes()) {
                eprintln!("保存失败：{}", e);
            } else {
                self.path = Some(path.clone());
                self.saved_text = Some(self.doc.text().to_string());
                if let Some(parent) = path.parent()
                    && let Ok(p) = FileSystemProvider::open(parent)
                {
                    self.workspace_root = Some(parent.to_path_buf());
                    self.workspace = Some(Workspace::new(p));
                    self.tree_open = true;
                }
                cx.notify();
            }
        }
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = self.palette;
        let header = self.header_text();
        let caret_count = self.cursors.len();
        let line_count = self.doc.projection().len();
        let focus = self.focus.clone();
        // 查找/替换条（`Cmd+F` 展开）：查询框 + 替换框 + 匹配计数 + 大小写开关 + 提示
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
                sb.push(
                    div()
                        .id(gpui::ElementId::Name(format!("sb-{rel}").into()))
                        .px_3()
                        .py_1()
                        .text_sm()
                        .cursor_pointer()
                        .when(cur, |d| d.bg(palette.selection))
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(move |view, _ev, _w, cx| {
                                view.open_path(full.clone(), cx);
                            }),
                        )
                        .child(name)
                        .into_any_element(),
                );
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
            .child(sidebar_el)
            .child(
                div()
                    .id("doc")
                    .size_full()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .child(tabbar_el)
            )
            .overflow_y_scrollbar()
            .bg(palette.background)
            .text_color(palette.foreground)
            .key_context("mazha-doc")
            .track_focus(&focus)
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                handle_key(view, event, window, cx);
            }))
            // 左键按下：放置光标
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, event: &MouseDownEvent, window, cx| {
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
                if !view.selecting { return; }
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
                    .child(header)
                    .child(format!("· {}", self.header_full_path()))
                    .child(format!(
                        "· {line_count} 行 · 光标×{} · Cmd+O 打开 · Cmd+S 保存 · Cmd+Shift+S 另存为 · Cmd+F 查找替换",
                        caret_count
                    )),
            )
            // 查找/替换条（`Cmd+F` 展开时才渲染）
            .children(find_bar)
            .child(DocElement { view: cx.entity() })
    }
}

/// IME / 文本输入契约：把 macOS/系统 IME 的组合、提交、选区查询桥接到源文本模型。
///
/// 偏移约定：GPUI 传入的 `Range<usize>` 是 **UTF-8 字节偏移**（相对整篇文档），
/// 与本视图 `caret`/`anchor`/`Document::apply` 的表示一致，无需转换。
/// 选区查询（`selected_text_range`/`text_for_range`）需转成 UTF-16 供 AppKit。
impl EntityInputHandler for DocView {
    /// 查询 `range`（UTF-8 字节）对应的文本，供 IME 剪贴/转换使用。
    ///
    /// `adjusted_range` 回填"实际取用"的区间（IME 据此调整 marked text）。
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let start = self.doc.snap_to_boundary(range.start);
        let end = self.doc.snap_to_boundary(range.end);
        let snapped = start.min(end)..start.max(end);
        let text = self.doc.try_slice(snapped.clone()).ok()?.into_owned();
        *adjusted_range = Some(snapped);
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

    /// IME 组合文本（marked text）对应的源字节范围；`None` = 无组合。
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.primary_cursor().marked.clone()
    }

    /// IME 取消组合：删除 marked 区间文本，光标回到组合起点。
    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let c = self.primary_cursor_mut();
        if let Some(marked) = c.marked.take() {
            let at = marked.start;
            self.apply_edit(Edit::Delete { range: marked }, at, cx);
        }
    }

    /// 用 `text` 替换 `range`（`None` = 当前选区）。IME 每次提交字符都走这里。
    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = range.unwrap_or_else(|| {
            self.primary_selection()
                .unwrap_or(self.cursors[self.primary].caret..self.cursors[self.primary].caret)
        });
        self.replace_selection_with_inner(target, text, cx);
    }

    /// 替换并进入组合状态：写入 `new_text`，把该区间标记为 marked，并设定组合内选区。
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = range.unwrap_or_else(|| {
            self.primary_selection()
                .unwrap_or(self.cursors[self.primary].caret..self.cursors[self.primary].caret)
        });
        let start = target.start;
        self.replace_selection_with_inner(target, new_text, cx);
        let marked_end = start + new_text.len();
        let c = self.primary_cursor_mut();
        c.marked = (marked_end > start).then_some(start..marked_end);
        // 组合内选区：相对 marked 起点的 UTF-8 偏移，转成绝对源偏移
        if let Some(rel) = new_selected_range {
            let abs_start = self.doc.snap_to_boundary(start + rel.start);
            let abs_end = self.doc.snap_to_boundary(start + rel.end);
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
        let pos0 = b0
            .shaped
            .position_for_index(b0.byte_of_col(p0.col), layout.line_height)
            .unwrap_or_else(|| point(Pixels::ZERO, Pixels::ZERO));
        let x0 = layout.origin.x + b0.indent + pos0.x;
        let y0 = layout.origin.y + layout.line_height * layout.row_starts[p0.line] as f32 + pos0.y;
        let (x1, y1) = match layout.blocks.get(p1.line) {
            Some(b1) => {
                let pos1 = b1
                    .shaped
                    .position_for_index(b1.byte_of_col(p1.col), layout.line_height)
                    .unwrap_or_else(|| point(Pixels::ZERO, Pixels::ZERO));
                (
                    layout.origin.x + b1.indent + pos1.x,
                    layout.origin.y
                        + layout.line_height * layout.row_starts[p1.line] as f32
                        + pos1.y,
                )
            }
            None => (x0, y0),
        };
        // 跨视觉行的候选框：取起止点包络（含起点所在视觉行的高度）
        let top = y0.min(y1);
        let height = (y0.max(y1) + layout.line_height) - top;
        let width = if x1 > x0 { x1 - x0 } else { x0 - x1 };
        let width = if width < px(2.) { px(2.) } else { width };
        Some(bounds(
            point(x0.min(x1), top),
            size(width, height.max(layout.line_height)),
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

/// 键盘事件 → 光标移动 / 编辑命令 / 撤销重做 / 全选。
///
/// 字符输入主要经 IME（`EntityInputHandler::replace_text_in_range`）送达；
/// 这里兜底处理无 IME 的 ASCII 直键，以及退格/删除/回车/Tab/撤销/重做/全选。
fn handle_key(view: &mut DocView, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<DocView>) {
    let m = &event.keystroke.modifiers;
    let shift = m.shift;

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
            "o" if m.platform => {
                view.open_file_dialog(cx);
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
                view.save_current(cx);
            }
            "s" if m.platform && shift => {
                view.save_as(cx);
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
                view.export_pdf(cx);
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
        // ASCII 直键兜底（IME 未接管时）：仅可打印单字符
        key if key.chars().count() == 1 && !key.chars().next().unwrap().is_control() => {
            let ch = key.chars().next().unwrap();
            view.insert_char(ch, cx);
        }
        _ => return,
    }
    cx.notify();
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

/// 把 core 的块样式映射为排版字号（标题放大、行内代码等距）。
fn block_font_size(block: &BlockStyle, base: Pixels, mono: Pixels) -> Pixels {
    match block.heading {
        Some(1) => base * 1.75,
        Some(2) => base * 1.5,
        Some(3) => base * 1.25,
        Some(4) => base * 1.1,
        Some(5) => base * 1.05,
        Some(6) => base * 1.0,
        _ => {
            let _ = mono; // 代码块行按等宽字号渲染（见 build_line_runs）
            base
        }
    }
}

/// 把一个 core [`TextRun`]（片段 + 样式）转成 gpui 的 [`gpui::TextRun`]（字形级样式）。
///
/// 返回 `(显示文本, gpui_text_run)`；调用方需保证 `run.text` 非空（空片段不参与排版）。
fn to_gpui_run(
    run: &DocTextRun,
    base_font: &Font,
    mono_font: &Font,
    _mono_size: Pixels,
    _base_size: Pixels,
    palette: &Palette,
) -> (String, gpui::TextRun) {
    // 默认按正文样式；代码块整体覆盖为等宽字体（字号统一在 shape_line 时按块应用）
    let mut font = base_font.clone();
    let mut color = palette.foreground;
    let mut background = None;
    let mut underline = None;
    let mut strikethrough = None;

    match run.style {
        RunStyle::Normal => {}
        RunStyle::Emphasis => font = font.italic(),
        RunStyle::Strong => font = font.bold(),
        RunStyle::Strikethrough => {
            strikethrough = Some(StrikethroughStyle {
                thickness: px(1.),
                color: Some(color),
            });
        }
        RunStyle::InlineCode => {
            font = mono_font.clone();
            color = palette.accent;
            background = Some(palette.subtle);
        }
        // 行内公式体（D34 ④）：斜体 + 主题色，与普通文本区分
        RunStyle::Math => {
            font = font.italic();
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

    let gpui_run = gpui::TextRun {
        len: run.text.len(),
        font,
        color,
        background_color: background,
        underline,
        strikethrough,
    };
    (run.text.clone(), gpui_run)
}

/// 把一整块投影行（引用前缀 + 可见片段）拼成一条不换行的可见文本 + 对应 gpui 样式段。
///
/// 返回 `(完整可见文本, gpui_runs)`。引用前缀 `> ` 用 muted 色，其余用片段自身样式。
/// `highlight` 为代码块行的语法高亮片段（相对该行显示文本；`None` = 不高亮）。
fn build_line_runs(
    line: &ProjectedLine,
    base_font: &Font,
    mono_font: &Font,
    mono_size: Pixels,
    base_size: Pixels,
    palette: &Palette,
    highlight: Option<&[ColoredRun]>,
) -> (SharedString, Vec<gpui::TextRun>) {
    let mut text = String::new();
    let mut runs: Vec<gpui::TextRun> = Vec::new();

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

    let size = block_font_size(&line.block, base_size, mono_size);

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
        return (text.into(), runs);
    }

    // 其余情况（含未高亮的代码块）：按投影片段拼接
    for run in &line.runs {
        let mut gp = to_gpui_run(run, base_font, mono_font, mono_size, size, palette);
        if line.block.code_block {
            // 代码块行：强制等宽字体 + 整行弱底色
            gp.1.font = mono_font.clone();
            gp.1.background_color = Some(palette.subtle);
        }
        text.push_str(&gp.0);
        runs.push(gp.1);
    }

    // 分割线：不在此处画字符，改由 `paint` 依块级 widget 画一条真实细线（SPEC §11.5、D30）。

    (text.into(), runs)
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
        x += base;
    }
    x
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
        let style = Style {
            size: Size {
                width: Length::Definite(relative(1.)),
                height: Length::Definite(relative(1.)),
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
        let (blocks, widgets, row_starts) = self.view.update(cx, |view, _| {
            // 需先取所有光标的源偏移，随后 projection() 会可变借用 view.doc（借用不相交）
            let caret_sources: Vec<usize> = view.cursors.iter().map(|c| c.caret).collect();
            let projection = view.doc.projection();
            let caret = view.cursors[view.primary].caret;
            let caret_pos = projection.source_to_doc_pos(caret);
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
                for ldr in projection.selection_display_ranges(Selection::new(sel.start, sel.end)) {
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
            let mut row_acc = 0usize;
            for (idx, line) in projection.lines.iter().enumerate() {
                let (text, runs) = build_line_runs(
                    line,
                    &base_font,
                    &mono_font,
                    mono_size,
                    base_size,
                    &palette,
                    code_hl[idx].as_deref(),
                );
                // 前缀字节数 = 形状文本长度 - 投影显示文本长度（引用 "> " 等投影外文本）
                let prefix_bytes = text.len().saturating_sub(line.display_text().len());
                // 用统一字体排版，保证行高一致（粗斜体/等宽在 runs 内体现）
                let line_font = base_font.clone();
                let uniform_runs: Vec<gpui::TextRun> = runs
                    .into_iter()
                    .map(|mut r| {
                        r.font = line_font.clone();
                        r
                    })
                    .collect();
                let indent = block_indent(line, indent_base);
                // 可用宽度 = 元素宽度 - 左缩进（含 BASE 左内边距）- 右侧 BASE 内边距
                let wrap_width = (content_width_total - indent - BASE).max(px(48.));
                let shaped = shape_wrapped_line(window, text, base_size, &uniform_runs, wrap_width);
                let rows = shaped.wrap_boundaries().len() + 1;
                row_starts.push(row_acc);
                row_acc += rows;
                v.push(BlockGeom {
                    proj: line.clone(),
                    shaped: Arc::new(shaped),
                    rows,
                    prefix_bytes,
                    indent,
                    caret_col: (caret_pos.line == idx).then_some(caret_pos.col),
                    selection: sel_by_line[idx].clone(),
                });
            }
            row_starts.push(row_acc);
            (v, widgets, row_starts)
        });

        DocLayout {
            blocks,
            widgets,
            origin: bounds.origin,
            line_height: LINE_HEIGHT,
            row_starts,
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

        // 回存几何，供鼠标命中测试（`DocView::hit_source`）读取
        let layout_for_view = layout.clone();
        let view = self.view.clone();
        window.defer(cx, move |_, cx| {
            view.update(cx, |v, _| {
                v.layout = Some(layout_for_view);
            });
        });

        // 先画块级 widget 装饰（面板/网格/分割线），再画文本，保证文字在上层。
        // 渲染态画装饰；编辑态（光标已在块内）不画，露出可编辑内容（SPEC §11.5）。
        // 装饰块的纵向范围按**视觉行**累计（`row_starts`），软换行下同样正确。
        if !layout.widgets.is_empty() {
            let content_right = element_bounds.origin.x + element_bounds.size.width - BASE;
            for w in &layout.widgets {
                let top =
                    layout.origin.y + LINE_HEIGHT * layout.row_starts[w.line_range.start] as f32;
                let bottom =
                    layout.origin.y + LINE_HEIGHT * layout.row_starts[w.line_range.end] as f32;
                let left = layout.origin.x + w.indent;
                let right = content_right.max(left + px(24.));
                let rect = bounds(point(left, top), size(right - left, bottom - top));
                match &w.kind {
                    // 分割线：在行垂直中线画一条细线（取代旧的 "─" 文本占位）
                    WidgetDecorKind::Rule => {
                        let y = top + LINE_HEIGHT / 2.;
                        window.paint_quad(fill(
                            bounds(point(left, y), size(right - left, px(1.))),
                            palette.border,
                        ));
                    }
                    // 代码块：整体描边（弱底色由每个代码行 run 自身提供，此处不叠加填充）
                    // + 右上角语言标签
                    WidgetDecorKind::Code { label } if !w.editing => {
                        paint_rect_border(window, rect, palette.border, px(1.));
                        if let Some(label) = label {
                            paint_corner_label(label, right, top, window, cx);
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
                            let y = layout.origin.y + LINE_HEIGHT * layout.row_starts[line] as f32;
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

        // 逐块绘制：选区高亮底 → 文本 → 光标竖线（均按视觉行定位）
        for (idx, block) in layout.blocks.iter().enumerate() {
            let first_row = layout.row_starts[idx];
            let row_top = layout.origin.y + LINE_HEIGHT * first_row as f32;
            let line_x = layout.origin.x + block.indent;

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
                                .position_for_index(s, layout.line_height)
                                .map_or(Pixels::ZERO, |p| p.x)
                        };
                        let x1 = block
                            .shaped
                            .position_for_index(e, layout.line_height)
                            .map_or(Pixels::ZERO, |p| p.x);
                        let width = if x1 > x0 { x1 - x0 } else { Pixels::ZERO };
                        let y = row_top + layout.line_height * r as f32;
                        window.paint_quad(fill(
                            bounds(point(line_x + x0, y), size(width, layout.line_height)),
                            palette.selection,
                        ));
                    }
                }
            }

            // 文本本体（`WrappedLine::paint` 内部按换行边界逐视觉行绘制）
            let _ = block.shaped.paint(
                point(line_x, row_top),
                layout.line_height,
                TextAlign::Left,
                None,
                window,
                cx,
            );

            // 光标竖线：仅在光标所在块绘制（用 prepaint 预计算的 caret_col 显示列）
            if let Some(col) = block.caret_col
                && let Some(p) = block
                    .shaped
                    .position_for_index(block.byte_of_col(col), layout.line_height)
            {
                let caret_rect = bounds(
                    point(line_x + p.x, row_top + p.y + px(2.)),
                    size(px(1.5), layout.line_height - px(4.)),
                );
                window.paint_quad(fill(caret_rect, palette.accent));
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
