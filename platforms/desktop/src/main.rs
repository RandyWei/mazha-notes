//! 码札桌面端（Phase 2：只读投影渲染 + 光标/标记显隐）。见 `SPEC.md` §7.1、§11。
//!
//! 管线：`.md` 源文本 → [`Document`]（源文本真相 + 投影缓存）→ `Projection`
//! （块样式 + 带样式可见文本片段 + 图片占位）→ GPUI 逐行绘制。
//!
//! 本里程碑已具备：
//! - **只读投影渲染**：语法标记默认隐藏（由 core 投影决定，UI 不解析 Markdown）；
//! - **光标**：点击文本放置光标（显示位置 → [`OffsetMap`] → 源字节偏移），
//!   方向键/Home/End 移动；
//! - **WYSIWYG 标记显隐**（SPEC §11.2）：光标所在行渲染**原始 Markdown**（标记可见），
//!   其余行渲染投影（标记隐藏）。
//!
//! 尚未实现：选区/多光标、IME（`EntityInputHandler`）、自写 `Element` + `shape_line`
//! 的连续布局（当前每行一个 `StyledText`，长段落会自动折行）。

use std::path::PathBuf;

use gpui_kit::base::{StyledExt, h_flex, v_flex};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme, Theme};
use gpui_kit::*;
use mazha_core::{DisplayPos, Document, ProjectedLine, RunStyle, TextRun as DocTextRun};

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
    /// 强调色（引用竖线、行内代码文字）。
    accent: Hsla,
    /// 边框/分割线。
    border: Hsla,
    /// 背景。
    background: Hsla,
    /// 弱底色（代码块、行内代码背景）。
    subtle: Hsla,
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
        }
    }
}

/// 主窗口内容：文档视图（投影渲染 + 光标）。
struct DocView {
    /// 打开的文件路径（`None` = 使用内置示例）。
    path: Option<PathBuf>,
    /// 文档（源文本真相 + 投影缓存）。
    doc: Document,
    /// 光标：**源文本字节偏移**（真相在源文本，显示位置只是一层映射）。
    caret: usize,
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
        Self {
            path: path.cloned(),
            doc: Document::new(&text),
            caret: 0,
            focus: cx.focus_handle(),
            ui_font: theme.font_family.clone(),
            mono_font: theme.mono_font_family.clone(),
            mono_font_size: theme.mono_font_size,
            palette: Palette::from_theme(theme),
        }
    }

    /// 窗口内标题栏文本。
    fn header_text(&self) -> String {
        match &self.path {
            Some(p) => p.display().to_string(),
            None => "内置示例（只读）".to_string(),
        }
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
    fn move_left(&mut self) {
        self.caret = self.doc.prev_char_boundary(self.caret);
    }

    /// 光标右移一个字符边界。
    fn move_right(&mut self) {
        self.caret = self.doc.next_char_boundary(self.caret);
    }

    /// 光标到当前行首。
    fn move_home(&mut self) {
        if let Some(i) = self.caret_line_index(self.caret) {
            let start = self.doc.projection().lines[i].source_range.start;
            self.caret = start;
        }
    }

    /// 光标到当前行尾。
    fn move_end(&mut self) {
        if let Some(i) = self.caret_line_index(self.caret) {
            let end = self.doc.projection().lines[i].source_range.end;
            self.caret = end;
        }
    }

    /// 光标上移一行（保持行内相对字符位置，超出则钳到行尾）。
    fn move_up(&mut self) {
        self.move_vertical(-1);
    }

    /// 光标下移一行。
    fn move_down(&mut self) {
        self.move_vertical(1);
    }

    /// 上/下移动公共实现：`step` 为 -1（上）或 +1（下）。
    fn move_vertical(&mut self, step: isize) {
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
                self.caret = if step < 0 { 0 } else { self.caret_line_end() };
                return;
            };
            (cur, projection.lines[target_i].source_range.clone())
        };
        let rel = self.caret.saturating_sub(cur.start);
        let target_caret = (target.start + rel).min(target.end);
        self.caret = self.doc.snap_to_boundary(target_caret);
    }

    /// 文档末尾偏移（光标下移到底时用）。
    fn caret_line_end(&mut self) -> usize {
        let projection = self.doc.projection();
        projection.lines.last().map_or(0, |l| l.source_range.end)
    }
}

impl Render for DocView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = self.palette;
        let ui_font = self.ui_font.clone();
        let mono_font = self.mono_font.clone();
        let mono_size = self.mono_font_size;
        // 本帧源文本快照：光标行要渲染"原始 Markdown"（含标记）
        let source = self.doc.text();
        let caret = self.caret;
        // 光标所在行（无光标行则 None，全部按投影渲染）
        let caret_line = self.caret_line_index(caret);
        let header = self.header_text();
        // 先把本帧需要的所有只读数据取出（借用投影期间不能再碰 self，故一次取全）
        let line_count = {
            let projection = self.doc.projection();
            projection.lines.len()
        };
        // 逐行元素：`render_line` 需 `&mut Context`，而 `self.focus` 是不可变借用，
        // 故先算完所有元素（借用在此结束），再把结果 move 进元素树。
        let line_els: Vec<AnyElement> = {
            let projection = self.doc.projection();
            projection
                .lines
                .iter()
                .enumerate()
                .map(|(i, line)| {
                    render_line(
                        i,
                        line,
                        &source,
                        caret,
                        caret_line == Some(i),
                        palette,
                        &ui_font,
                        &mono_font,
                        mono_size,
                        cx,
                    )
                })
                .collect()
        };
        let focus = self.focus.clone();

        v_flex()
            .size_full()
            .bg(palette.background)
            // 顶部信息条：文件路径 + 行数 + 光标位置（只读态标识）
            .child(
                h_flex()
                    .h(px(28.))
                    .w_full()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .border_b_1()
                    .border_color(palette.border)
                    .text_sm()
                    .text_color(palette.muted)
                    .child(header)
                    .child(format!(
                        "· {line_count} 行 · 光标 {} · 点击定位 / ↑↓←→ 移动",
                        caret + 1
                    )),
            )
            // 正文：可滚动 + 可聚焦（键盘事件入口）
            .child(
                div()
                    .id("doc")
                    .flex_1()
                    .w_full()
                    .flex()
                    .flex_col()
                    .overflow_y_scrollbar()
                    .text_color(palette.foreground)
                    .key_context("mazha-doc")
                    .track_focus(&focus)
                    .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                        handle_key(view, event, window, cx);
                    }))
                    .children(line_els),
            )
    }
}

/// 键盘事件 → 光标移动（Phase 2：仅方向键/Home/End；IME 与编辑在后续迭代）。
fn handle_key(view: &mut DocView, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<DocView>) {
    // 组合键（Cmd/Ctrl）交给系统，避免误拦截快捷键
    if event.keystroke.modifiers.control || event.keystroke.modifiers.platform {
        return;
    }
    match event.keystroke.key.as_str() {
        "left" => view.move_left(),
        "right" => view.move_right(),
        "up" => view.move_up(),
        "down" => view.move_down(),
        "home" => view.move_home(),
        "end" => view.move_end(),
        _ => return,
    }
    cx.notify();
}

/// 一行投影 → GPUI 元素。
///
/// - 普通行：列表/任务/引用前缀 + 投影 runs（标记隐藏）+ 图片占位；
/// - **光标行**：渲染 `source[source_range]` 原文（标记可见），并画光标竖线。
#[allow(clippy::too_many_arguments)]
fn render_line(
    index: usize,
    line: &ProjectedLine,
    source: &str,
    caret: usize,
    is_caret_line: bool,
    palette: Palette,
    ui_font: &SharedString,
    mono_font: &SharedString,
    mono_size: Pixels,
    cx: &mut Context<DocView>,
) -> AnyElement {
    let block = &line.block;

    // 分割线：无可见文本，画一条横向细线（分割线行不接受光标）
    if block.thematic_break {
        return div()
            .w_full()
            .my_1()
            .h(px(1.))
            .bg(palette.border)
            .into_any_element();
    }

    // 本行的源文本切片（光标行直接用它渲染）
    let raw = source
        .get(line.source_range.clone())
        .unwrap_or_default()
        .to_string();

    let mut row = h_flex()
        .id(format!("line-{index}"))
        .w_full()
        .items_start()
        .gap_1()
        .px_4()
        .py(px(1.))
        .min_h(px(22.));

    // 引用：左侧竖线 + 随深度加宽的缩进（`>` 标记本身已被投影隐藏）
    if block.quote_depth > 0 {
        row = row
            .border_l_2()
            .border_color(palette.accent)
            .pl(px(8. + 12. * f32::from(block.quote_depth)));
    }

    // 列表符号与任务复选框：光标行的原文里已含 `- ` / `* `，不再重复画前缀
    if !is_caret_line {
        if let Some(bullet) = &block.bullet {
            let marker = if bullet.ordered {
                format!("{}.", bullet.number)
            } else {
                "•".to_string()
            };
            row = row.child(div().text_color(palette.muted).child(marker));
        }
        if let Some(done) = block.task {
            let (mark, color) = if done {
                ("☑", palette.accent)
            } else {
                ("☐", palette.muted)
            };
            row = row.child(div().text_color(color).child(mark));
        }
    }

    // 文本主体：光标行用原文（默认样式），其余用投影 runs
    let mut content = h_flex().items_start().flex_1();
    if block.code_block && !is_caret_line {
        content = content
            .font_family(mono_font.clone())
            .text_size(mono_size)
            .bg(palette.subtle)
            .rounded_md()
            .px_1();
    }

    // 原文/投影文本 + runs
    let (text, runs) = if is_caret_line {
        (raw.clone(), Vec::new())
    } else {
        line_plain_text_and_runs(line, palette, ui_font, mono_font)
    };
    let styled = StyledText::new(text.clone()).with_runs(runs);

    // 点击定位：命中"显示字符索引" → `OffsetMap` → 源字节偏移 → `snap_to_boundary`。
    // 光标行的 `text` 就是原文（源偏移 = 行首 + 索引），非光标行经 `display_to_source`
    // 映射（投影隐藏了标记），二者合一，于是**每行**都能点击跳转，而非只有非光标行。
    let layout = styled.layout().clone();
    let click_line = line.clone();
    let line_id = ElementId::Name(format!("text-{index}").into());
    let hit_layout = layout.clone();
    let text_el = div().id(line_id).child(styled).on_click(cx.listener(
        move |view, event: &ClickEvent, window, cx| {
            // `ClickEvent` 是枚举（Mouse/Keyboard/Touch）；仅处理鼠标点击
            let ClickEvent::Mouse(mouse) = event else {
                return;
            };
            // 命中测试：与渲染一致的字体/字号下量一次，得到"显示字符索引"（字节）
            let display_char_ix = char_index_at(&hit_layout, mouse.down.position);
            // 显示字符索引 → 源字节偏移（投影行经 OffsetMap；交叉点取更小的源偏移）
            let source_offset = if is_caret_line {
                click_line.source_range.start + display_char_ix
            } else {
                click_line
                    .offset_map
                    .display_to_source(DisplayPos(display_char_ix))
            };
            view.caret = view
                .doc
                .snap_to_boundary(source_offset.min(click_line.source_range.end));
            let focus = view.focus.clone();
            window.focus(&focus, cx);
            cx.notify();
        },
    ));
    content = content.child(text_el);

    // 光标行：在文本**之前**插一个零宽画布，用 `position_for_index` 拿到光标的窗口坐标，
    // 画一条竖线。画布是行内的定位锚点，不参与命中测试，故不影响点击。
    if is_caret_line {
        // 行内源偏移（字节）= 光标绝对偏移 − 行首；钳到行内
        let local = caret
            .saturating_sub(line.source_range.start)
            .min(line.source_range.end - line.source_range.start);
        let line_end = line.source_range.end - line.source_range.start;
        let caret_layout = layout.clone();
        let caret_color = palette.accent;
        content = content.child(
            canvas(
                move |_, _, _| (),
                move |box_bounds, _, window, _| {
                    // `position_for_index` 返回的是**绝对窗口坐标**（内部已含 TextLayout 的
                    // bounds.origin），故直接取 x，不再叠加本画布的 origin。
                    let caret_x = caret_layout
                        .position_for_index(local)
                        .or_else(|| caret_layout.position_for_index(line_end))
                        .map(|p| p.x)
                        .unwrap_or(box_bounds.origin.x);
                    let top = box_bounds.origin.y;
                    let bottom = top + caret_layout.line_height();
                    let caret_rect = bounds(point(caret_x, top), size(px(1.5), bottom - top));
                    window.paint_quad(fill(caret_rect, caret_color));
                },
            )
            .h_full()
            .w(px(0.)),
        );
    }

    row.child(apply_heading_style(content, block.heading))
        .into_any_element()
}

/// 把点击的窗口坐标换算成"显示字符索引"（字节）。
///
/// `TextLayout` 内部已记录每个 run 的实际字体、字号与行高，`index_for_position`
/// 直接复用形状化结果，无需再压 text style；返回 Ok（精确命中）或 Err（最近边界），
/// 两者都是可用的字节索引（Err 为最近的字符边界）。
fn char_index_at(layout: &TextLayout, position: Point<Pixels>) -> usize {
    layout
        .index_for_position(position)
        .unwrap_or_else(|nearest| nearest)
}

/// 构造一行的「可见文本 + gpui runs」（不含光标行原文路径）。
///
/// 文本 = 投影可见片段拼接 + 图片占位文本；runs 与文本字节一一对应。
fn line_plain_text_and_runs(
    line: &ProjectedLine,
    palette: Palette,
    ui_font: &SharedString,
    mono_font: &SharedString,
) -> (String, Vec<TextRun>) {
    let mut text = String::new();
    let mut runs = Vec::new();
    for run in &line.runs {
        text.push_str(&run.text);
        runs.push(to_gpui_run(run, palette, ui_font, mono_font));
    }
    // 图片等内联占位：投影把 `![alt](dest)` 整段隐藏，这里给出可见占位
    for widget in &line.widgets {
        let placeholder = widget.kind.placeholder();
        text.push_str(&placeholder);
        runs.push(TextRun {
            len: placeholder.len(),
            font: font_with(ui_font, FontWeight::NORMAL, FontStyle::Italic),
            color: palette.muted,
            background_color: None,
            underline: None,
            strikethrough: None,
        });
    }
    // 空行补一个空格，保住行高
    if text.is_empty() {
        text.push(' ');
        runs.push(default_run(1, palette, ui_font));
    }
    (text, runs)
}

/// 我们的一段样式 → gpui `TextRun`（字体族/字重/字形 + 颜色/底色/下划线/删除线）。
fn to_gpui_run(
    run: &DocTextRun,
    palette: Palette,
    ui_font: &SharedString,
    mono_font: &SharedString,
) -> TextRun {
    let mut font = font_with(ui_font, FontWeight::NORMAL, FontStyle::Normal);
    let mut color = palette.foreground;
    let mut background_color = None;
    let mut underline = None;
    let mut strikethrough = None;
    match run.style {
        RunStyle::Normal => {}
        RunStyle::Emphasis => font.style = FontStyle::Italic,
        RunStyle::Strong => font.weight = FontWeight::BOLD,
        RunStyle::Strikethrough => strikethrough = Some(StrikethroughStyle::default()),
        RunStyle::InlineCode => {
            font = font_with(mono_font, FontWeight::NORMAL, FontStyle::Normal);
            color = palette.accent;
            background_color = Some(palette.subtle);
        }
        RunStyle::LinkText => {
            color = palette.link;
            underline = Some(UnderlineStyle::default());
        }
    }
    TextRun {
        len: run.text.len(),
        font,
        color,
        background_color,
        underline,
        strikethrough,
    }
}

/// 默认样式 run（覆盖空行等未被样式片段覆盖的尾巴）。
fn default_run(len: usize, palette: Palette, ui_font: &SharedString) -> TextRun {
    TextRun {
        len,
        font: font_with(ui_font, FontWeight::NORMAL, FontStyle::Normal),
        color: palette.foreground,
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

/// 构造一个 `Font`（gpui 的 Font 是纯数据，逐字段填）。
fn font_with(family: &SharedString, weight: FontWeight, style: FontStyle) -> Font {
    Font {
        family: family.clone(),
        features: FontFeatures::default(),
        fallbacks: None,
        weight,
        style,
    }
}

/// 标题级别 → 字号/字重（作用于整行内容，随子元素级联）。
fn apply_heading_style(element: Div, heading: Option<u8>) -> Div {
    match heading {
        Some(1) => element.text_3xl().font_bold(),
        Some(2) => element.text_2xl().font_bold(),
        Some(3) => element.text_xl().font_bold(),
        Some(4) => element.text_lg().font_bold(),
        Some(5) => element.text_base().font_bold(),
        Some(6) => element.text_sm().font_bold(),
        _ => element,
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
