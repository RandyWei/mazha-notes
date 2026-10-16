//! Markdown → Typst 标记转换（PDF 导出管线的第一步）。
//!
//! 把 [`mazha-md`] 的块/片段模型转换为 Typst 标记文本；编译为 PDF 字节由
//! 同 crate 的 `pdf` 模块（Typst `World` 实现）完成。见 `SPEC.md` §8、D21。
//!
//! 关键约束：输出必须是**可编译的 Typst 标记**——可见文本里的标记特殊字符
//! 必须转义（见 [`escape_typst`]），块与块之间必须留空行（Typst 用空行分段）。

use mazha_md::{Block, BlockKind, SpanKind, SpanRole, SyntaxSpan, parse};
use std::fmt::Write as _;

/// Typst 标记模式中具有结构含义、必须转义的字符。
///
/// 覆盖：`\`（转义符自身）、`#`（代码表达式）、`[`/`]`（内容块）、
/// `` ` ``（行内原始文本）、`$`（数学）、`*`/`_`（强调）、`<`/`>`（标签与行首引用）、
/// `@`（引用，如邮箱会被误判）、`/`（`//`、`/*` 注释）、
/// `~`（不间断空格简写）、`=`（行首会被当成标题）。
const MARKUP_SPECIAL: &[char] = &[
    '\\', '#', '[', ']', '`', '$', '*', '_', '<', '>', '@', '/', '~', '=',
];

/// 把可见文本转义成 Typst 标记安全的文本。
///
/// Typst 中「`\` + 任意非空白字符」的求值结果就是该字符本身，因此逐字符
/// 转义即可无损还原；空白字符绝不转义（行尾单独一个 `\` 表示换行符）。
fn escape_typst(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if MARKUP_SPECIAL.contains(&ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// 转义 Typst **代码模式字符串**内部的文本（如 `#link("…")` 的 URL）。
///
/// 字符串字面量里只有 `"` 与 `\` 有特殊含义，其余字符原样保留。
fn escape_typst_string(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

/// 把 Markdown 源文本转换为 Typst 标记。
pub fn to_typst(source: &str) -> String {
    let blocks = parse(source);
    let mut out = String::new();
    blocks_to_typst(source, &blocks, &mut out);
    out
}

/// 用 [`crate::Template`] 版式前置包裹转换结果（PDF 导出用）。
///
/// 模板前置为空时等价 [`to_typst`]；非空时置于正文之前并以空行分隔（Typst 用空行分段）。
pub fn to_typst_with_template(source: &str, template: &crate::Template) -> String {
    let body = to_typst(source);
    let preamble = template.preamble.trim_end();
    if preamble.is_empty() {
        return body;
    }
    let mut out = String::with_capacity(preamble.len() + body.len() + 2);
    out.push_str(preamble);
    out.push_str("\n\n");
    out.push_str(&body);
    out
}

/// 递归转换一组块。
///
/// 块之间用**空行**分隔：Typst 以空行分段，单换行只是段内空格，否则
/// 两个段落会被合并成一段。
fn blocks_to_typst(source: &str, blocks: &[Block], out: &mut String) {
    for b in blocks {
        block_to_typst(source, b, out);
        out.push_str("\n\n");
    }
}

/// 转换单个块。
fn block_to_typst(source: &str, b: &Block, out: &mut String) {
    match &b.kind {
        BlockKind::FrontMatter { .. } => {
            // front-matter 是元数据，Typst 里用 #metadata 嵌入（不渲染）
            let _ = write!(out, "#metadata(none)");
        }
        BlockKind::Heading { level } => {
            // ATX 标题 → Typst `=` 系列
            for _ in 0..*level {
                out.push('=');
            }
            out.push(' ');
            inline_to_typst(source, &b.spans, out);
        }
        BlockKind::Paragraph => {
            inline_to_typst(source, &b.spans, out);
        }
        BlockKind::ThematicBreak => {
            // `---` 在 Typst 标记里是破折号简写（em dash），不是分隔线，
            // 因此显式画一条 100% 宽的横线
            out.push_str("#line(length: 100%)");
        }
        BlockKind::BlockQuote => {
            out.push_str("#quote[");
            blocks_to_typst(source, &b.children, out);
            out.push(']');
        }
        BlockKind::Callout { kind, title } => {
            // 提示块 → Typst block + 醒目标题（标题是可见文本，需转义）
            let heading = escape_typst(title.as_deref().unwrap_or_else(|| kind.as_str()));
            let _ = write!(out, "#block(fill: luma(240), inset: 10pt, radius: 4pt)[");
            let _ = write!(out, "*{heading}*\n\n");
            blocks_to_typst(source, &b.children, out);
            out.push(']');
        }
        BlockKind::List { ordered, start } => {
            for (i, item) in b.children.iter().enumerate() {
                let marker = if *ordered {
                    "+ ".to_string()
                } else {
                    "- ".to_string()
                };
                let _ = write!(out, "{marker}");
                let _ = start; // 起始序号：Typst 用 enum 计数器，MVP 忽略自定义起始
                let _ = i;
                list_item_to_typst(source, item, out);
                out.push('\n');
            }
        }
        BlockKind::ListItem { .. } => {
            // 单独的列表项（非列表容器直接子项）：按段落处理
            blocks_to_typst(source, &b.children, out);
        }
        BlockKind::CodeBlock { lang, .. } => {
            let lang_tag = lang.as_deref().unwrap_or("");
            let mut content = String::new();
            for sp in &b.spans {
                if sp.role == SpanRole::CodeContent {
                    content.push_str(&source[sp.range.clone()]);
                }
            }
            // 围栏取「内容中最长反引号串 + 1（至少 3）」，避免内容自带 ``` 时提前闭合
            let fence = "`".repeat(longest_backtick_run(&content).max(2) + 1);
            let _ = writeln!(out, "{fence}{lang_tag}");
            out.push_str(&content);
            let _ = write!(out, "\n{fence}");
        }
        BlockKind::Table { align } => {
            table_to_typst(source, b, align, out);
        }
    }
}

/// 列表项：任务标记 + 内容。
fn list_item_to_typst(source: &str, item: &Block, out: &mut String) {
    if let BlockKind::ListItem { task } = &item.kind {
        match task {
            Some(true) => out.push_str("✔ "),
            Some(false) => out.push_str("☐ "),
            None => {}
        }
    }
    // 列表项内容：直接子块（通常是段落）
    for (ci, child) in item.children.iter().enumerate() {
        if ci > 0 {
            out.push('\n');
        }
        match &child.kind {
            BlockKind::Paragraph => inline_to_typst(source, &child.spans, out),
            // 嵌套列表：Typst 以缩进表达层级，统一缩进两空格
            BlockKind::List { .. } => {
                let mut nested = String::new();
                block_to_typst(source, child, &mut nested);
                for line in nested.lines() {
                    let _ = writeln!(out, "  {line}");
                }
            }
            _ => block_to_typst(source, child, out),
        }
    }
}

/// 计算文本中最长的连续反引号长度（用于决定围栏长度）。
fn longest_backtick_run(text: &str) -> usize {
    let mut best = 0usize;
    let mut cur = 0usize;
    for ch in text.chars() {
        if ch == '`' {
            cur += 1;
            best = best.max(cur);
        } else {
            cur = 0;
        }
    }
    best
}

/// 表格 → Typst `#table`。
///
/// Typst 的函数实参必须用逗号分隔，且对齐走 `align:` 参数（可传对齐数组），
/// 单元格直接写成内容块 `[..]`——比逐格 `table.cell(...)` 更简洁也更符合习惯。
fn table_to_typst(source: &str, b: &Block, align: &[mazha_md::Align], out: &mut String) {
    let rows = table_rows_typst(source, b);
    if rows.is_empty() {
        return;
    }
    let cols = align.len().max(rows.first().map_or(0, Vec::len));
    out.push_str("#table(\n");
    let _ = writeln!(out, "  columns: {cols},");
    // 逐列对齐：align 接受与列等长的数组，缺省按左对齐处理
    let alignments: Vec<&str> = (0..cols)
        .map(|ci| match align.get(ci) {
            Some(mazha_md::Align::Center) => "center",
            Some(mazha_md::Align::Right) => "right",
            _ => "left",
        })
        .collect();
    let _ = writeln!(out, "  align: ({}),", alignments.join(", "));
    for row in &rows {
        for cell in row {
            // 单元格是标记内容块，可见文本必须转义
            let cell = escape_typst(cell);
            let _ = write!(out, "  [{cell}],");
        }
        out.push('\n');
    }
    out.push(')');
}

/// 把表格块按行切成"单元格可见文本"（跳过分隔行）。
///
/// 每行按竖线切成单元格，单元格文本为可见片段拼接（trim 后）。
fn table_rows_typst(source: &str, b: &Block) -> Vec<Vec<String>> {
    let bytes = source.as_bytes();
    let end = b.range.end.min(bytes.len());
    let mut bounds: Vec<std::ops::Range<usize>> = Vec::new();
    let mut start = b.range.start;
    while start < end {
        let e = bytes[start..end]
            .iter()
            .position(|&c| c == b'\n')
            .map_or(end, |i| start + i);
        bounds.push(start..e);
        start = e + 1;
    }
    let mut rows = Vec::new();
    for line in &bounds {
        // 本行内的片段按竖线切成单元格（分隔行整行跳过）
        let mut cells: Vec<String> = vec![String::new()];
        for sp in &b.spans {
            if sp.role == SpanRole::TableDelim {
                continue;
            }
            if sp.is_empty() || sp.range.start < line.start || sp.range.start >= line.end {
                continue;
            }
            if sp.role == SpanRole::TablePipe {
                cells.push(String::new());
            } else if sp.kind == SpanKind::Text {
                // 只取可见文本片段，跳过 `**`、链接括号等行内标记
                cells
                    .last_mut()
                    .expect("至少一个单元格")
                    .push_str(&source[sp.range.clone()]);
            }
        }
        // 去首尾空单元格（行首/行尾竖线产生）
        if cells.first().is_some_and(String::is_empty) {
            cells.remove(0);
        }
        if cells.last().is_some_and(String::is_empty) {
            cells.pop();
        }
        // 单元格文本 trim
        for c in &mut cells {
            *c = c.trim().to_string();
        }
        if !cells.is_empty() {
            rows.push(cells);
        }
    }
    rows
}

/// 把一组行内片段转换为 Typst 标记。
fn inline_to_typst(source: &str, spans: &[SyntaxSpan], out: &mut String) {
    let mut i = 0usize;
    let mut in_strike = false;
    while i < spans.len() {
        let sp = &spans[i];
        match (sp.kind, sp.role) {
            // 标签：单独样式（须在通用 Text 之前）
            (SpanKind::Text, SpanRole::Tag) => {
                let text = escape_typst(&source[sp.range.clone()]);
                let _ = write!(out, "#text(fill: blue)[{text}]");
                i += 1;
            }
            // 可见文本一律输出内容（样式由成对 Marker 开合），特殊字符转义
            (SpanKind::Text, _) => {
                out.push_str(&escape_typst(&source[sp.range.clone()]));
                i += 1;
            }
            (SpanKind::Marker, SpanRole::Strong) => {
                out.push('*');
                i += 1;
            }
            (SpanKind::Marker, SpanRole::Emphasis) => {
                out.push('_');
                i += 1;
            }
            (SpanKind::Marker, SpanRole::Strikethrough) => {
                if in_strike {
                    out.push(']');
                } else {
                    out.push_str("#strike[");
                }
                in_strike = !in_strike;
                i += 1;
            }
            (SpanKind::Marker, SpanRole::InlineCode) => {
                // 行内代码：内容按 **原始文本** 输出（Typst raw 不解析标记，也不吃
                // 反斜杠转义，故绝不可套 escape_typst）。内容不含反引号时用单
                // 反引号围栏；含反引号时改用 #raw("…")（字符串可转义），因为
                // 围栏与内容会粘连成更长反引号串 → 触发 unclosed raw text。
                let start = i + 1;
                let end = spans[start..]
                    .iter()
                    .position(|s| s.kind == SpanKind::Marker && s.role == SpanRole::InlineCode)
                    .map_or(spans.len(), |p| start + p);
                let mut content = String::new();
                for s in &spans[start..end] {
                    content.push_str(&source[s.range.clone()]);
                }
                if content.contains('`') {
                    let raw = escape_typst_string(&content);
                    let _ = write!(out, "#raw(\"{raw}\")");
                } else {
                    out.push('`');
                    out.push_str(&content);
                    out.push('`');
                }
                i = end + 1;
            }
            (SpanKind::Marker, SpanRole::LinkMarker) => {
                let ch = source.as_bytes().get(sp.range.start).copied();
                if ch == Some(b'[') {
                    // 找目标 URL（写进代码模式字符串，按字符串规则转义）
                    let dest = spans[i..]
                        .iter()
                        .find(|s| s.role == SpanRole::LinkDest)
                        .map(|s| escape_typst_string(&source[s.range.clone()]))
                        .unwrap_or_default();
                    let _ = write!(out, "#link(\"{dest}\")[");
                } else if ch == Some(b')') {
                    out.push(']');
                }
                i += 1;
            }
            (SpanKind::Widget, SpanRole::ImageMarker) => {
                // 图片：#image("src", alt: "…") —— Typst 的 alt 是字符串参数，
                // 不是尾随内容块（传内容块会报 unexpected argument）
                if let Some((alt, dest)) = parse_image_typst(&source[sp.range.clone()]) {
                    let dest = escape_typst_string(&dest);
                    if alt.is_empty() {
                        let _ = write!(out, "#image(\"{dest}\")");
                    } else {
                        let alt = escape_typst_string(&alt);
                        let _ = write!(out, "#image(\"{dest}\", alt: \"{alt}\")");
                    }
                }
                i += 1;
            }
            (SpanKind::Marker, SpanRole::SoftBreak) => {
                out.push('\n');
                i += 1;
            }
            (SpanKind::Marker, SpanRole::HardBreak) => {
                out.push_str("\\\n");
                i += 1;
            }
            (SpanKind::Marker, SpanRole::EscapeMarker) => {
                // Markdown 的反斜杠只说明"这是字面量"，落到 Typst 还得重新转义，
                // 否则 `#`、`[`、`*` 等会被 Typst 当语法再次解析
                if let Some(next) = spans.get(i + 1) {
                    out.push_str(&escape_typst(&source[next.range.clone()]));
                    i += 2;
                } else {
                    i += 1;
                }
            }
            // 其余标记（标题/引用/列表/围栏/链接目标等）导出时隐藏
            _ => i += 1,
        }
    }
}

/// 从 `![alt](dest)` 源文本解析 alt 与 dest。
fn parse_image_typst(raw: &str) -> Option<(String, String)> {
    let inner = raw.strip_prefix("![")?.strip_suffix(')')?;
    let (alt, dest) = inner.split_once("](")?;
    Some((alt.to_string(), dest.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typst_heading() {
        let t = to_typst("# 标题\n");
        assert_eq!(t.trim(), "= 标题");
    }

    #[test]
    fn typst_heading_levels() {
        assert!(to_typst("## 二\n").trim().starts_with("== 二"));
        assert!(to_typst("### 三\n").trim().starts_with("=== 三"));
    }

    #[test]
    fn typst_paragraph_bold_italic() {
        let t = to_typst("**粗** *斜*\n");
        assert!(t.contains("*粗*"));
        assert!(t.contains("_斜_"));
    }

    #[test]
    fn typst_inline_code() {
        let t = to_typst("`code`\n");
        assert!(t.contains("`code`"));
    }

    #[test]
    fn typst_inline_code_content_is_raw() {
        // 行内代码内容原样输出：Typst raw 不解析标记，故内容里的特殊字符
        // 不应被加上反斜杠转义（否则会原样显示出多余的反斜杠）。
        let t = to_typst("`a_b #c [d]`\n");
        assert!(t.contains("`a_b #c [d]`"), "{t}");
        assert!(!t.contains("\\_"), "{t}");
    }

    #[test]
    fn typst_inline_code_with_backtick_uses_raw_fn() {
        // 内容含反引号时不能用反引号围栏（会与围栏粘连成更长串 →
        // unclosed raw text），改用 #raw("…")。
        let t = to_typst("`` ` ``\n");
        assert!(t.contains("#raw(\"`\")"), "{t}");
    }

    #[test]
    fn typst_link() {
        let t = to_typst("[文本](https://x.com)\n");
        assert!(t.contains("#link(\"https://x.com\")["));
    }

    #[test]
    fn typst_list_unordered() {
        let t = to_typst("- 甲\n- 乙\n");
        assert!(t.contains("- 甲"));
        assert!(t.contains("- 乙"));
    }

    #[test]
    fn typst_list_ordered() {
        let t = to_typst("1. 甲\n2. 乙\n");
        assert!(t.contains("+ 甲"));
    }

    #[test]
    fn typst_code_block() {
        let t = to_typst("```rust\nfn main() {}\n```\n");
        assert!(t.contains("```rust"));
        assert!(t.contains("fn main() {}"));
        assert!(t.contains("```"));
    }

    #[test]
    fn typst_blockquote() {
        let t = to_typst("> 引用\n");
        assert!(t.contains("#quote["));
    }

    #[test]
    fn typst_table() {
        let t = to_typst("| a | b |\n| --- | --- |\n| 1 | 2 |\n");
        assert!(t.contains("#table("));
        assert!(t.contains("columns: 2,"));
        assert!(t.contains("align: ("), "{t}");
        assert!(t.contains("[a]"));
        assert!(t.contains("[b]"));
        assert!(t.contains("[1]"));
        assert!(t.contains("[2]"));
    }

    #[test]
    fn typst_thematic_break() {
        // `---` 在 Typst 是破折号简写，必须显式画横线
        let t = to_typst("---\n");
        assert!(t.contains("#line(length: 100%)"));
        assert!(!t.contains("---"));
    }

    #[test]
    fn typst_plain_template_is_identity() {
        let src = "# 标题\n\n正文\n";
        assert_eq!(
            to_typst_with_template(src, &crate::Template::plain()),
            to_typst(src)
        );
    }

    #[test]
    fn typst_template_preamble_prepended() {
        let t = to_typst_with_template("# 标题\n", &crate::Template::article());
        assert!(t.starts_with("#set page("), "{t}");
        assert!(t.contains("#set text(size: 11pt)"));
        // 正文仍在
        assert!(t.contains("= 标题"));
    }

    #[test]
    fn typst_escapes_markup_specials() {
        // Markdown 里用反斜杠拿到"字面量特殊字符"，输出时必须再转义回 Typst 安全文本
        let t =
            to_typst("价 \\#100 \\[方括号\\] a\\_b \\*星\\* <标签> x@y.z 斜杠//注释 \\~波浪 =号\n");
        assert!(t.contains("\\#100"), "{t}");
        assert!(t.contains("\\[方括号\\]"), "{t}");
        assert!(t.contains("a\\_b"), "{t}");
        assert!(t.contains("\\*星\\*"), "{t}");
        assert!(t.contains("\\<标签\\>"), "{t}");
        assert!(t.contains("x\\@y.z"), "{t}");
        assert!(t.contains("\\/\\/"), "{t}");
        assert!(t.contains("\\~波浪"), "{t}");
        assert!(t.contains("\\=号"), "{t}");
    }

    #[test]
    fn typst_escapes_string_literals() {
        let t = to_typst("[点这里](https://x.com/a\"b)\n");
        assert!(t.contains("#link(\"https://x.com/a\\\"b\")"), "{t}");
    }

    #[test]
    fn typst_blocks_separated_by_blank_line() {
        let t = to_typst("第一段\n\n第二段\n");
        assert!(t.contains("第一段\n\n第二段"), "{t}");
    }

    #[test]
    fn typst_nested_list_indented() {
        let t = to_typst("- 甲\n  - 乙\n");
        assert!(t.contains("\n  - 乙"), "{t}");
    }

    #[test]
    fn typst_code_fence_longer_than_content() {
        // 内容里含 ``` 时围栏要加长，否则提前闭合
        let src = "````\n```rust\nfn main() {}\n```\n````\n";
        let t = to_typst(src);
        assert!(t.contains("````\n"), "{t}");
        assert!(t.contains("```rust"), "{t}");
    }

    #[test]
    fn typst_callout() {
        let t = to_typst("> [!NOTE]\n> 内容\n");
        assert!(t.contains("#block("));
        assert!(t.contains("*NOTE*"));
    }

    #[test]
    fn typst_wikilink_visible() {
        let t = to_typst("[[Page]]\n");
        assert!(t.contains("Page"));
    }

    #[test]
    fn typst_tag_styled() {
        let t = to_typst("#标签\n");
        assert!(t.contains("#text(fill: blue)"));
        assert!(t.contains("#标签"));
    }

    #[test]
    fn typst_image() {
        let t = to_typst("![图](img.png)\n");
        assert!(t.contains("#image(\"img.png\", alt: \"图\")"), "{t}");
    }

    #[test]
    fn typst_strikethrough() {
        let t = to_typst("~~删~~\n");
        assert!(t.contains("#strike["));
    }
}
