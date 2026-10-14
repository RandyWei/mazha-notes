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
//!   在每个块内按重叠区画高亮矩形。
//!
//! 设计取舍：**不做软换行**——每个投影块渲染成一条不换行的 `ShapedLine`。这样
//! "显示字节索引"与 `OffsetMap` 的"显示字符索引"天然等长（偏移映射要求字符边界），
//! 一次整篇命中即可换算源偏移；软换行留待后续用 `shape_text` 的 `wrap_boundaries`
//! 折算成"块内显示行"再映射。
//!
//! 尚未实现：多光标。

use std::ops::Range;
use std::panic::Location;
use std::path::PathBuf;

use gpui_kit::base::h_flex;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme, Theme};
use gpui_kit::*;
use mazha_core::{
    BlockStyle, DisplayPos, Document, Edit, OffsetMap, ProjectedLine, RunStyle,
    TextRun as DocTextRun,
};

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
    /// 本块是否为光标所在块（仅一块为真；用于绘制光标竖线）。
    caret_line: bool,
    /// 本块与当前选区的重叠部分（`None` 表示本块不在选区内）。
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
    /// 内容左上角（窗口坐标）。
    origin: Point<Pixels>,
    /// 统一行高。
    line_height: Pixels,
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

/// 主窗口内容：文档视图（投影渲染 + 光标 + 选区）。
struct DocView {
    /// 打开的文件路径（`None` = 使用内置示例）。
    path: Option<PathBuf>,
    /// 文档（源文本真相 + 投影缓存）。
    doc: Document,
    /// 光标：**源文本字节偏移**（真相在源文本，显示位置只是一层映射）。
    caret: usize,
    /// 选区锚点：**源文本字节偏移**；`None` 表示当前无选区（仅光标）。
    ///
    /// 选区 = 锚点到光标的有序区间；锚点等于光标即塌缩为光标。
    anchor: Option<usize>,
    /// IME 组合文本（未提交的 marked text）对应的源字节范围；`None` = 无组合输入。
    ///
    /// 组合期间该区间视为"将被替换"，提交时整体替换、取消时还原。
    marked: Option<Range<usize>>,
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
            caret: 0,
            anchor: None,
            marked: None,
            selecting: false,
            layout: None,
            focus: cx.focus_handle(),
            ui_font: theme.font_family.clone(),
            mono_font: theme.mono_font_family.clone(),
            mono_font_size: theme.mono_font_size,
            palette,
        }
    }

    /// 窗口内标题栏文本。
    fn header_text(&self) -> String {
        match &self.path {
            Some(p) => p.display().to_string(),
            None => "内置示例（只读）".to_string(),
        }
    }

    /// 当前选区（源字节 `Range`），把锚点/光标归一成有序区间；无选区时返回 `None`。
    fn selection(&self) -> Option<Range<usize>> {
        let anchor = self.anchor?;
        if anchor == self.caret {
            return None; // 锚点与光标重合 → 塌缩，视为无选区
        }
        Some(anchor.min(self.caret)..anchor.max(self.caret))
    }

    /// 光标落在哪一行（`[start, end]` 闭右端，便于 `caret == 行尾` 时仍算本行）。
    fn caret_line_index(&mut self, caret: usize) -> Option<usize> {
        let projection = self.doc.projection();
        projection
            .lines
            .iter()
            .position(|l| l.source_range.contains(&caret) || l.source_range.end == caret)
    }

    /// 光标左移一个字符边界。
    fn move_left(&mut self, shift: bool) {
        let c = self.doc.prev_char_boundary(self.caret);
        self.update_caret(c, shift);
    }

    /// 光标右移一个字符边界。
    fn move_right(&mut self, shift: bool) {
        let c = self.doc.next_char_boundary(self.caret);
        self.update_caret(c, shift);
    }

    /// 光标到当前行首。
    fn move_home(&mut self, shift: bool) {
        if let Some(i) = self.caret_line_index(self.caret) {
            let start = self.doc.projection().lines[i].source_range.start;
            self.update_caret(start, shift);
        }
    }

    /// 光标到当前行尾。
    fn move_end(&mut self, shift: bool) {
        if let Some(i) = self.caret_line_index(self.caret) {
            let end = self.doc.projection().lines[i].source_range.end;
            self.update_caret(end, shift);
        }
    }

    /// 光标上移一行（保持行内相对字符位置，超出则钳到行尾）。
    fn move_up(&mut self, shift: bool) {
        self.move_vertical(-1, shift);
    }

    /// 光标下移一行。
    fn move_down(&mut self, shift: bool) {
        self.move_vertical(1, shift);
    }

    /// 更新光标：`shift` 为真时扩展选区（首次移动把锚点定在旧光标），否则塌缩选区。
    fn update_caret(&mut self, new_caret: usize, shift: bool) {
        if shift {
            // 首次按住 Shift 移动：把锚点钉在移动前的光标
            if self.anchor.is_none() {
                self.anchor = Some(self.caret);
            }
        } else {
            self.anchor = None; // 不按 Shift → 选区塌缩为光标
        }
        self.caret = self.doc.snap_to_boundary(new_caret);
    }

    /// 应用一次编辑到源文本（唯一修改入口），并把光标移到编辑后的新位置。
    ///
    /// `new_caret` 是编辑**完成后**希望光标落在的源字节偏移（会被 `snap_to_boundary` 校正）。
    /// 编辑失败（非法边界等）时保持文档与光标不变。成功后塌缩选区并清空 IME 组合区。
    fn apply_edit(&mut self, edit: Edit, new_caret: usize, cx: &mut Context<Self>) {
        if self.doc.apply(edit).is_ok() {
            self.anchor = None;
            self.marked = None;
            self.caret = self.doc.snap_to_boundary(new_caret);
            cx.notify();
        }
    }

    /// 撤销/重做等"绕过 `apply_edit`"的变更之后，统一收敛光标/选区状态并重绘。
    ///
    /// undo/redo 会改变文档长度，光标可能越界，必须重新 `snap_to_boundary`；
    /// 选区锚点与 IME 组合区在文档结构变化后不再可信，一律清空。
    fn after_doc_mutation(&mut self, cx: &mut Context<Self>) {
        self.caret = self.doc.snap_to_boundary(self.caret);
        self.anchor = None;
        self.marked = None;
        cx.notify();
    }

    /// 删除选区（或光标左侧一个字符，即退格）。返回是否发生了删除。
    fn delete_backward(&mut self, cx: &mut Context<Self>) -> bool {
        if let Some(sel) = self.selection() {
            let caret = sel.start;
            self.apply_edit(Edit::Delete { range: sel }, caret, cx);
            return true;
        }
        let prev = self.doc.prev_char_boundary(self.caret);
        if prev == self.caret {
            return false; // 已在文档开头
        }
        self.apply_edit(Edit::Delete { range: prev..self.caret }, prev, cx);
        true
    }

    /// 删除光标右侧一个字符（即 Delete 键）。返回是否发生了删除。
    fn delete_forward(&mut self, cx: &mut Context<Self>) -> bool {
        let range = self.selection().unwrap_or_else(|| {
            let next = self.doc.next_char_boundary(self.caret);
            self.caret..next
        });
        if range.is_empty() {
            return false; // 已在文档末尾
        }
        let caret = range.start;
        self.apply_edit(Edit::Delete { range }, caret, cx);
        true
    }

    /// Enter：插入换行。有选区则先删除选区。
    fn insert_newline(&mut self, cx: &mut Context<Self>) {
        let range = self.selection().unwrap_or(self.caret..self.caret);
        let at = range.start;
        self.apply_edit(
            Edit::Replace {
                range,
                text: "\n".into(),
            },
            at + 1,
            cx,
        );
    }

    /// 插入一个制表符（Tab）。有选区则先删除选区。
    fn insert_tab(&mut self, cx: &mut Context<Self>) {
        let range = self.selection().unwrap_or(self.caret..self.caret);
        let at = range.start;
        self.apply_edit(
            Edit::Replace {
                range,
                text: "    ".into(),
            },
            at + 4,
            cx,
        );
    }

    /// 插入一个普通字符（IME 提交/直接按键）。有选区则先删除选区。
    fn insert_char(&mut self, ch: char, cx: &mut Context<Self>) {
        let range = self.selection().unwrap_or(self.caret..self.caret);
        let at = range.start;
        let mut text = String::new();
        text.push(ch);
        let new_caret = at + text.len();
        self.apply_edit(Edit::Replace { range, text }, new_caret, cx);
    }

    /// 用 `text` 替换指定区间 `target`（不先取当前选区）；IME 的内部替换原语。
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
        self.apply_edit(
            Edit::Replace {
                range,
                text: text.to_string(),
            },
            new_caret,
            cx,
        );
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
        let (u_lo, u_hi) = if u_lo <= u_hi { (u_lo, u_hi) } else { (u_hi, u_lo) };

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

    /// 上/下移动公共实现：`step` 为 -1（上）或 +1（下）。
    fn move_vertical(&mut self, step: isize, shift: bool) {
        let Some(i) = self.caret_line_index(self.caret) else {
            return;
        };
        // 先取出当前行与目标行的源区间（避免与 self.caret 写入冲突的借用）
        let (cur, target) = {
            let projection = self.doc.projection();
            let cur = projection.lines[i].source_range.clone();
            let target_i = if step < 0 {
                i.checked_sub(1)
            } else {
                (i + 1 < projection.lines.len()).then_some(i + 1)
            };
            let Some(target_i) = target_i else {
                // 已到首/末行：钳到文档两端
                let end = if step < 0 { 0 } else { self.caret_line_end() };
                self.update_caret(end, shift);
                return;
            };
            (cur, projection.lines[target_i].source_range.clone())
        };
        let rel = self.caret.saturating_sub(cur.start);
        let target_caret = (target.start + rel).min(target.end);
        let target_caret = self.doc.snap_to_boundary(target_caret);
        self.update_caret(target_caret, shift);
    }

    /// 文档末尾偏移（光标下移到底时用）。
    fn caret_line_end(&mut self) -> usize {
        let projection = self.doc.projection();
        projection.lines.last().map_or(0, |l| l.source_range.end)
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
}

impl Render for DocView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = self.palette;
        let header = self.header_text();
        let caret = self.caret;
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
            // 左键按下：放置光标（或 Shift 扩展），开始拖选
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, event: &MouseDownEvent, window, cx| {
                    if let Some(src) = view.hit_source(event.position) {
                        view.update_caret(src, event.modifiers.shift);
                        view.selecting = true;
                    }
                    let focus = view.focus.clone();
                    window.focus(&focus, cx);
                    cx.notify();
                }),
            )
            // 拖动中：持续更新光标（锚点固定）→ 选区随之扩展
            .on_mouse_move(cx.listener(|view, event: &MouseMoveEvent, _, cx| {
                if !view.selecting {
                    return;
                }
                if let Some(src) = view.hit_source(event.position) {
                    // 拖动一律扩展（锚点已在 mousedown 固定），不再看 shift
                    if view.anchor.is_none() {
                        return;
                    }
                    view.caret = view.doc.snap_to_boundary(src);
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
            // 顶部信息条（sticky 之上用 header 太复杂，这里放在滚动内容前作首块）
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
                        "· {line_count} 行 · 光标 {} · 点击定位 / ↑↓←→ 移动 / Shift 扩选",
                        caret + 1
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
        let (start, end, reversed) = match self.anchor {
            Some(anchor) if anchor != self.caret => {
                let (lo, hi) = if anchor < self.caret {
                    (anchor, self.caret)
                } else {
                    (self.caret, anchor)
                };
                (lo, hi, anchor > self.caret)
            }
            _ => (self.caret, self.caret, false),
        };
        let (u16_start, u16_end) = self.utf16_range(start, end);
        Some(UTF16Selection {
            range: u16_start..u16_end,
            reversed,
        })
    }

    /// IME 组合文本（marked text）对应的源字节范围；`None` = 无组合。
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.clone()
    }

    /// IME 取消组合：删除 marked 区间文本，光标回到组合起点。
    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(marked) = self.marked.take() {
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
        let target = range.unwrap_or_else(|| self.selection().unwrap_or(self.caret..self.caret));
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
        let target = range.unwrap_or_else(|| self.selection().unwrap_or(self.caret..self.caret));
        let start = target.start;
        self.replace_selection_with_inner(target, new_text, cx);
        let marked_end = start + new_text.len();
        self.marked = (marked_end > start).then_some(start..marked_end);
        // 组合内选区：相对 marked 起点的 UTF-8 偏移，转成绝对源偏移
        if let Some(rel) = new_selected_range {
            let abs_start = self.doc.snap_to_boundary(start + rel.start);
            let abs_end = self.doc.snap_to_boundary(start + rel.end);
            self.anchor = Some(abs_start);
            self.caret = abs_end;
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
        let layout = self.layout.as_ref()?;
        let (start, end) = self.source_range_from_utf16(range_utf16.clone())?;
        // 找到与 [start, end) 相交的第一个块，用它的几何估算矩形
        for (idx, block) in layout.blocks.iter().enumerate() {
            let bs = block.source_range.start;
            let be = block.source_range.end;
            if be <= start || end <= bs {
                continue;
            }
            let line_start = start.max(bs);
            let line_end = end.min(be);
            let x0 = block.offset_map.source_to_display(line_start).get();
            let x1 = block.offset_map.source_to_display(line_end).get();
            let px0 = layout.origin.x + block.indent + block.line.x_for_index(x0);
            let px1 = layout.origin.x + block.indent + block.line.x_for_index(x1);
            let row_top = layout.origin.y + layout.line_height * idx as f32;
            return Some(bounds(
                point(px0, row_top),
                size(px1 - px0, layout.line_height),
            ));
        }
        None
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

    /// IME 主动设置选区（UTF-16 区间）；映射回源字节并更新 caret/anchor。
    fn set_selected_text_range(
        &mut self,
        range_utf16: Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((start, end)) = self.source_range_from_utf16(range_utf16) {
            if start == end {
                self.anchor = None;
                self.caret = start;
            } else {
                self.anchor = Some(start);
                self.caret = end;
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

    // Cmd/Ctrl 组合键：撤销/重做/全选；其余交给系统，避免误拦截
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
                // 全选：锚点钉在 0，光标到文档末
                view.anchor = Some(0);
                view.caret = view.doc.len();
                cx.notify();
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
fn build_line_runs(
    line: &ProjectedLine,
    base_font: &Font,
    mono_font: &Font,
    mono_size: Pixels,
    base_size: Pixels,
    palette: &Palette,
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

    // 代码块内容行整体等宽 + 弱底色
    let size = block_font_size(&line.block, base_size, mono_size);
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

    // 分割线：用一串水平线字符占位（视觉近似）
    if line.block.thematic_break {
        let hr = "─".repeat(40);
        runs.push(gpui::TextRun {
            len: hr.len(),
            font: base_font.clone(),
            color: palette.border,
            background_color: None,
            underline: None,
            strikethrough: None,
        });
        text.push_str(&hr);
    }

    (text.into(), runs)
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
                snapshot.selection(),
            )
        };
        let indent_base = BASE;

        // update 提供 &mut DocView（投影缓存需惰性建立），闭包返回几何数据避免借用越界
        let blocks = self.view.update(cx, |view, _| {
            let caret = view.caret;
            let caret_line = view.caret_line_index(caret);
            let projection = view.doc.projection();
            let mut v: Vec<BlockGeom> = Vec::with_capacity(projection.lines.len());
            for (idx, line) in projection.lines.iter().enumerate() {
                let (text, runs) =
                    build_line_runs(line, &base_font, &mono_font, mono_size, base_size, &palette);
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
                let sel_overlap = selection
                    .clone()
                    .filter(|sel| ranges_overlap(sel, &line.source_range));
                v.push(BlockGeom {
                    source_range: line.source_range.clone(),
                    offset_map: line.offset_map.clone(),
                    line: shaped,
                    indent,
                    caret_line: caret_line == Some(idx),
                    selection: sel_overlap,
                });
            }
            v
        });

        DocLayout {
            blocks,
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
        let caret = self.view.read(cx).caret;

        // 回存几何，供鼠标命中测试（`DocView::hit_source`）读取
        let layout_for_view = layout.clone();
        let view = self.view.clone();
        window.defer(cx, move |_, cx| {
            view.update(cx, |v, _| {
                v.layout = Some(layout_for_view);
            });
        });

        // 逐块绘制：选区高亮底 → 文本 → 光标竖线
        for (idx, block) in layout.blocks.iter().enumerate() {
            let row_top = layout.origin.y + LINE_HEIGHT * idx as f32;
            let line_x = layout.origin.x + block.indent;
            let baseline_origin = point(line_x, row_top);

            // 选区高亮：用该块与选区的重叠区间，换算成行内显示 x 区间再填矩形
            if let Some(sel) = &block.selection {
                let start_ix = block.offset_map.source_to_display(sel.start).get();
                let end_ix = block.offset_map.source_to_display(sel.end).get();
                let x0 = block.line.x_for_index(start_ix);
                let x1 = block.line.x_for_index(end_ix);
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

            // 光标竖线：仅在光标所在块绘制（用 prepaint 预计算的 caret_line 标记）
            if block.caret_line {
                let local = block.offset_map.source_to_display(caret).get();
                let caret_x = baseline_origin.x + block.line.x_for_index(local);
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

/// 两个源字节区间是否有重叠（用于选区高亮判断）。
fn ranges_overlap(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start < b.end && b.start < a.end
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
