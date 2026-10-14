//! 码札桌面端（Phase 2 起步：只读投影渲染）。见 `SPEC.md` §7.1、§11。
//!
//! 管线：`.md` 源文本 → [`Document`]（源文本真相 + 投影缓存）→ `Projection`
//! （块样式 + 带样式可见文本片段 + 图片占位）→ GPUI 只读绘制。
//!
//! 本里程碑**只读**：不做光标/IME/编辑（后续迭代引入自写 Element + `shape_line`，
//! 见 SPEC §7.1）。语法标记的显隐完全由 core 的投影决定，UI 不再解析 Markdown。

use std::path::PathBuf;

use gpui_kit::base::{StyledExt, h_flex, v_flex};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme, Theme};
use gpui_kit::*;
use mazha_core::{Document, ProjectedLine, RunStyle, TextRun as DocTextRun};

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

/// 主窗口内容：文档的只读投影视图。
struct DocView {
    /// 打开的文件路径（`None` = 使用内置示例）。
    path: Option<PathBuf>,
    /// 文档（源文本真相 + 投影缓存）。
    doc: Document,
    /// 等宽字体族（来自主题，代码块/行内代码使用）。
    mono_font: SharedString,
    /// 等宽字号（来自主题）。
    mono_font_size: Pixels,
    /// 主题色快照。
    palette: Palette,
}

impl DocView {
    /// 读入文件（或内置示例）并构建文档。
    fn new(path: Option<&PathBuf>, cx: &mut App) -> Self {
        let text = match path {
            Some(p) => std::fs::read_to_string(p).unwrap_or_else(|e| format!("> 读取失败：{e}\n")),
            None => SAMPLE.to_string(),
        };
        let theme = cx.theme();
        Self {
            path: path.cloned(),
            doc: Document::new(&text),
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
}

impl Render for DocView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let palette = self.palette;
        let mono_font = self.mono_font.clone();
        let mono_size = self.mono_font_size;
        // 先取只读字段，再借投影缓存（projection() 需 &mut self.doc）
        let header = self.header_text();
        // 投影缓存：&mut self.doc 借用，渲染期间不再触碰 self 其余字段
        let projection = self.doc.projection();
        let line_count = projection.len();

        v_flex()
            .size_full()
            .bg(palette.background)
            // 顶部信息条：文件路径 + 行数（只读态标识）
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
                    .child(format!("· {line_count} 行 · 只读")),
            )
            // 正文：逐行绘制投影（可滚动）
            .child(
                div()
                    .id("doc")
                    .flex_1()
                    .w_full()
                    .flex()
                    .flex_col()
                    .overflow_y_scrollbar()
                    .text_color(palette.foreground)
                    .children(
                        projection
                            .lines
                            .iter()
                            .map(|line| render_line(line, palette, &mono_font, mono_size)),
                    ),
            )
    }
}

/// 一行投影 → GPUI 元素（块样式前缀 + 带样式文本片段 + 图片占位）。
fn render_line(
    line: &ProjectedLine,
    palette: Palette,
    mono_font: &SharedString,
    mono_size: Pixels,
) -> Div {
    let block = &line.block;

    // 分割线：无可见文本，画一条横向细线
    if block.thematic_break {
        return div().w_full().my_1().h(px(1.)).bg(palette.border);
    }

    let mut row = h_flex()
        .w_full()
        .items_start()
        .gap_1()
        .px_4()
        .py(px(1.))
        // 等高底线：空行/标题行也能对齐
        .min_h(px(22.));

    // 引用：左侧竖线 + 随深度加宽的缩进（`>` 标记本身已被投影隐藏）
    if block.quote_depth > 0 {
        row = row
            .border_l_2()
            .border_color(palette.accent)
            .pl(px(8. + 12. * f32::from(block.quote_depth)));
    }

    // 列表符号（有序 `1.` / 无序 `•`）：由 BlockStyle 提供，不在 runs 里
    if let Some(bullet) = &block.bullet {
        let marker = if bullet.ordered {
            format!("{}.", bullet.number)
        } else {
            "•".to_string()
        };
        row = row.child(div().text_color(palette.muted).child(marker));
    }

    // 任务复选框
    if let Some(done) = block.task {
        let (mark, color) = if done {
            ("☑", palette.accent)
        } else {
            ("☐", palette.muted)
        };
        row = row.child(div().text_color(color).child(mark));
    }

    // 内容区：标题层级作用于整段，runs 各自带行内样式
    let mut content = h_flex().items_start().gap_1().flex_1();

    // 代码块内容行：整体等宽 + 弱底色
    if block.code_block {
        content = content
            .font_family(mono_font.clone())
            .text_size(mono_size)
            .bg(palette.subtle)
            .rounded_md()
            .px_1();
    }

    for run in &line.runs {
        content = content.child(render_run(run, palette, mono_font));
    }
    // 图片等内联占位：投影把 `![alt](dest)` 整段隐藏，这里给出可见占位
    for widget in &line.widgets {
        content = content.child(
            div()
                .text_color(palette.muted)
                .italic()
                .child(widget.kind.placeholder()),
        );
    }
    // 空行也保留行高（占一个空格，避免折叠成 0 高）
    if line.runs.is_empty() && line.widgets.is_empty() {
        content = content.child(" ");
    }

    row = row.child(apply_heading_style(content, block.heading));
    row
}

/// 一个带样式文本片段 → 元素（`RunStyle` → GPUI 文本样式）。
fn render_run(run: &DocTextRun, palette: Palette, mono_font: &SharedString) -> Div {
    let mut element = div().child(run.text.clone());
    match run.style {
        RunStyle::Normal => {}
        RunStyle::Emphasis => element = element.italic(),
        RunStyle::Strong => element = element.font_bold(),
        RunStyle::Strikethrough => element = element.line_through(),
        RunStyle::InlineCode => {
            element = element
                .font_family(mono_font.clone())
                .text_color(palette.accent)
                .bg(palette.subtle)
                .rounded_md()
                .px_1();
        }
        RunStyle::LinkText => element = element.text_color(palette.link).underline(),
    }
    element
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
        gpui_kit::open_window(options, cx, move |_, cx| {
            cx.new(|cx| DocView::new(path.as_ref(), cx))
        })
        .expect("打开窗口失败");
    });
}
