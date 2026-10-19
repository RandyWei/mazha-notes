//! 码札桌面端（Phase 2：只读投影渲染 + 光标/标记显隐 + 跨行选区）。见 `SPEC.md` §7.1、§11。
//!
//! 管线：`.md` 源文本 → [`Document`]（源文本真相 + 投影缓存）→ `Projection`
//! （块样式 + 带样式可见文本片段 + 图片占位）→ 自写 [`DocElement`] 用 `shape_line`
//! 逐块连续绘制。
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
//! 设计取舍：**不做软换行**——每个投影块渲染成一条不换行的 `ShapedLine`。这样
//! "显示字节索引"与 `OffsetMap` 的"显示字符索引"天然等长（偏移映射要求字符边界），
//! 一次整篇命中即可换算源偏移；软换行留待后续用 `shape_text` 的 `wrap_boundaries`
//! 折算成"块内显示行"再映射。

use std::ops::Range;
use std::panic::Location;
use std::path::PathBuf;

use gpui_kit::base::h_flex;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme, Theme};
use gpui_kit::*;
use mazha_core::{
    BlockStyle, BlockTarget, BlockWidgetKind, ColoredRun, DisplayPos, DocPos, Document, Edit,
    Highlighter, InlineMark, OffsetMap, ProjectedLine, RunStyle, Selection, TableOp,
    TextRun as DocTextRun,
    commands::{set_block, toggle_inline},
    edit_table,
};
use mazha_export::to_pdf;
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
    /// 该块在源文本中的字节区间。
    source_range: Range<usize>,
    /// 显示位置 ↔ 源偏移映射（不换行时：显示索引 = 显示字符索引）。
    offset_map: OffsetMap,
    /// 已形状化的行（不含换行；`x_for_index`/`closest_index_for_x` 均为行内相对坐标）。
    line: ShapedLine,
    /// 左缩进（引用深度、列表缩进），行内 x 坐标需加上它才是块内绝对 x。
    indent: Pixels,
    /// 光标在本块时的行内**显示列**（`None` = 光标不在本块；仅一块为 `Some`）。
    ///
    /// 由 `Projection::source_to_doc_pos` 于 prepaint 阶段算好，paint 直接据此画竖线。
    caret_col: Option<usize>,
    /// 本块在当前选区内的**显示列**区间（`None` 表示本块不在选区内）。
    ///
    /// 由 `Projection::selection_display_ranges` 于 prepaint 阶段算好，paint 直接按
    /// `line.x_for_index(cols.*)` 换算屏幕 x，无需再经 `OffsetMap` 换算源偏移。
    selection: Option<Range<usize>>,
}

/// 整篇文档的布局几何：块序列 + 光标原点 + 统一行高。
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
}

impl DocLayout {
    /// 从窗口坐标命中一个**显示位置**（块序号 + 行内显示字节索引）。
    fn hit(&self, position: Point<Pixels>) -> Option<(usize, usize)> {
        if self.blocks.is_empty() {
            return None;
        }
        let rel_y = position.y - self.origin.y;
        let row = (rel_y / self.line_height).floor().max(0.);
        let row = (row as usize).min(self.blocks.len() - 1);
        let block = &self.blocks[row];
        // 行内 x 需减去该块的缩进才是 ShapedLine 内的相对坐标
        let rel_x = position.x - self.origin.x - block.indent;
        let local_ix = block.line.closest_index_for_x(rel_x);
        Some((row, local_ix))
    }
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

/// 主窗口内容：文档视图（投影渲染 + 多光标/选区）。
struct DocView {
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
}

impl DocView {
    /// 读入文件（或内置示例）并构建文档，光标置于文首。
    fn new(path: Option<&PathBuf>, cx: &mut App) -> Self {
        let text = match path {
            Some(p) => std::fs::read_to_string(p).unwrap_or_else(|e| format!("> 读取失败：{e}\n")),
            None => SAMPLE.to_string(),
        };
        let theme = cx.theme();
        let palette = Palette::from_theme(theme);
        Self {
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
        }
    }

    /// 窗口内标题栏文本。
    fn header_text(&self) -> String {
        match &self.path {
            Some(p) => p.display().to_string(),
            None => "内置示例（只读）".to_string(),
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

    /// 所有光标上移一行（保持行内相对字符位置，超出则钳到行尾）。
    fn move_up(&mut self, shift: bool) {
        self.move_vertical_all(-1, shift);
    }

    /// 所有光标下移一行。
    fn move_down(&mut self, shift: bool) {
        self.move_vertical_all(1, shift);
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
        let (row, local_ix) = layout.hit(position)?;
        let block = &layout.blocks[row];
        // 显示索引（不换行 = 显示字符索引）→ 源字节偏移
        let source_offset = block.offset_map.display_to_source(DisplayPos(local_ix));
        Some(
            self.doc
                .snap_to_boundary(source_offset.min(block.source_range.end)),
        )
    }

    /// 导出当前文档为 PDF 并保存到文件。
    fn export_pdf(&mut self, _cx: &mut Context<Self>) {
        let markdown = self.doc.text();
        let result = to_pdf(&markdown);
        match result {
            Ok(pdf_doc) => {
                // 使用 rfd 弹出保存文件对话框
                if let Some(path) = FileDialog::new()
                    .set_title("导出 PDF")
                    .add_filter("PDF 文件", &["pdf"])
                    .set_file_name("document.pdf")
                    .save_file()
                {
                    // 写入 PDF 字节
                    if let Err(e) = std::fs::write(&path, &pdf_doc.bytes) {
                        eprintln!("导出 PDF 失败：{}", e);
                    }
                }
            }
            Err(e) => {
                eprintln!("PDF 编译失败：{}", e);
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

        // 正文：可滚动 + 可聚焦（键盘事件入口）+ 鼠标选区
        div()
            .id("doc")
            .size_full()
            .flex()
            .flex_col()
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
                    .child(format!(
                        "· {line_count} 行 · 光标×{} · 点击定位 / Alt+点击增减光标 / ↑↓←→ 移动 / Shift 扩选",
                        caret_count
                    )),
            )
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
        let x0 = layout.origin.x + b0.indent + b0.line.x_for_index(p0.col);
        let (x1, rows) = match layout.blocks.get(p1.line) {
            Some(b1) => (
                layout.origin.x + b1.indent + b1.line.x_for_index(p1.col),
                p1.line - p0.line + 1,
            ),
            None => (x0, 1),
        };
        let row_top = layout.origin.y + layout.line_height * p0.line as f32;
        Some(bounds(
            point(x0, row_top),
            size(x1 - x0, layout.line_height * rows as f32),
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

    // Cmd/Ctrl 组合键：撤销/重做/全选/格式化/块类型；其余交给系统，避免误拦截
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

/// 文档正文的自写元素：逐块 `shape_line` 连续布局 + 绘制，并把几何回存给视图。
///
/// 之所以自写 [`Element`] 而不用 `div`+`StyledText`：需要在 `prepaint` 里拿到
/// 每块**不换行**的 `ShapedLine` 几何（`x_for_index`/`closest_index_for_x`），
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
        let (blocks, widgets) = self.view.update(cx, |view, _| {
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
                    let kind = match &w.kind {
                        BlockWidgetKind::Code { lang, .. } => {
                            // 语言标签：用等宽字体在 prepaint 里 shape 好，paint 直接画
                            let label = lang.as_ref().map(|l| {
                                let run = gpui::TextRun {
                                    len: l.len(),
                                    font: mono_font.clone(),
                                    color: palette.muted,
                                    background_color: None,
                                    underline: None,
                                    strikethrough: None,
                                };
                                Box::new(window.text_system().shape_line(
                                    l.clone().into(),
                                    mono_size,
                                    &[run],
                                    None,
                                ))
                            });
                            WidgetDecorKind::Code { label }
                        }
                        BlockWidgetKind::Table { .. } => WidgetDecorKind::Table,
                        BlockWidgetKind::Rule => WidgetDecorKind::Rule,
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
                // 用统一字体排版，保证行高一致（粗斜体/等宽在 runs 内体现）
                let line_font = base_font.clone();
                let uniform_runs: Vec<gpui::TextRun> = runs
                    .into_iter()
                    .map(|mut r| {
                        r.font = line_font.clone();
                        r
                    })
                    .collect();
                let shaped = window
                    .text_system()
                    .shape_line(text, base_size, &uniform_runs, None);
                let indent = block_indent(line, indent_base);
                v.push(BlockGeom {
                    source_range: line.source_range.clone(),
                    offset_map: line.offset_map.clone(),
                    line: shaped,
                    indent,
                    caret_col: (caret_pos.line == idx).then_some(caret_pos.col),
                    selection: sel_by_line[idx].clone(),
                });
            }
            (v, widgets)
        });

        DocLayout {
            blocks,
            widgets,
            origin: bounds.origin,
            line_height: LINE_HEIGHT,
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
        if !layout.widgets.is_empty() {
            let content_right = element_bounds.origin.x + element_bounds.size.width - BASE;
            for w in &layout.widgets {
                let top = layout.origin.y + LINE_HEIGHT * w.line_range.start as f32;
                let bottom = layout.origin.y + LINE_HEIGHT * w.line_range.end as f32;
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
                    }
                    // 表格：弱底填充 + 外框 + 每行之间的水平分隔线
                    WidgetDecorKind::Table if !w.editing => {
                        window.paint_quad(fill(rect, palette.subtle));
                        paint_rect_border(window, rect, palette.border, px(1.));
                        for row in (w.line_range.start + 1)..w.line_range.end {
                            let y = layout.origin.y + LINE_HEIGHT * row as f32;
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

        // 逐块绘制：选区高亮底 → 文本 → 光标竖线
        for (idx, block) in layout.blocks.iter().enumerate() {
            let row_top = layout.origin.y + LINE_HEIGHT * idx as f32;
            let line_x = layout.origin.x + block.indent;
            let baseline_origin = point(line_x, row_top);

            // 选区高亮：`selection` 已是本行的显示列区间，直接换算行内 x 填矩形
            if let Some(cols) = &block.selection {
                let x0 = block.line.x_for_index(cols.start);
                let x1 = block.line.x_for_index(cols.end);
                let hl = bounds(
                    point(baseline_origin.x + x0, row_top),
                    size(x1 - x0, LINE_HEIGHT),
                );
                window.paint_quad(fill(hl, palette.selection));
            }

            // 文本本体
            let _ = block.line.paint(
                baseline_origin,
                LINE_HEIGHT,
                TextAlign::Left,
                None,
                window,
                cx,
            );

            // 光标竖线：仅在光标所在块绘制（用 prepaint 预计算的 caret_col 显示列）
            if let Some(col) = block.caret_col {
                let caret_x = baseline_origin.x + block.line.x_for_index(col);
                let caret_rect = bounds(
                    point(caret_x, row_top + px(2.)),
                    size(px(1.5), LINE_HEIGHT - px(4.)),
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
