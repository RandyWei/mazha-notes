//! 行内语法解析（SPEC §5，决策 D23）。
//!
//! 对段落/标题等内容区间做一次左到右扫描，把 `**粗体**`、`*斜体*`、`~~删除~~`、
//! 行内代码、链接、图片、转义等切成"可见文本 + 语法标记"的 [`SyntaxSpan`]。
//! 强调/粗体等可见文本会**递归**解析内部，从而支持 `**粗 *斜* 体**` 这类嵌套。
//!
//! MVP 的取舍（已在 SPEC 记录）：
//! - `_` 不做词内强调（`snake_case` 不会被误判）；
//! - 引用式链接 `[a][b]`、实体 `&amp;`、跨行强调等暂不支持（按普通文本处理）；
//! - 图片整体作为 [`SpanKind::Widget`]（渲染占位，Phase 3 由 UI 绘制）。

use crate::span::{SpanKind, SpanRole, SyntaxSpan};
use std::ops::Range;

/// 是否可能触发行内语法的特殊字符。
fn is_special(b: u8) -> bool {
    matches!(
        b,
        b'\\' | b'*' | b'_' | b'~' | b'`' | b'[' | b'!' | b'<' | b'#' | b'$'
    )
}

/// 解析 `region` 区间内的行内语法，返回**完整覆盖该区间**的片段序列。
pub fn parse_inline(source: &str, region: Range<usize>) -> Vec<SyntaxSpan> {
    let mut out = Vec::new();
    scan(source, region, &mut out);
    out
}

/// 对 `region` 做扫描；`region` 使用源文本字节坐标。
fn scan(source: &str, region: Range<usize>, out: &mut Vec<SyntaxSpan>) {
    let base = region.start;
    let s = &source[region.clone()];
    let bytes = s.as_bytes();
    let mut i = 0usize;
    let mut text_start = 0usize;
    while i < bytes.len() {
        if !is_special(bytes[i]) {
            i += 1;
            continue;
        }
        if let Some((consumed, spans)) = try_construct(source, s, base, i) {
            flush_text(out, base, text_start, i);
            out.extend(spans);
            i += consumed;
            text_start = i;
        } else {
            i += 1; // 不构成语法：按普通文本继续
        }
    }
    flush_text(out, base, text_start, bytes.len());
}

/// 把 `[start, end)` 追加为普通文本片段（空区间忽略）。
fn flush_text(out: &mut Vec<SyntaxSpan>, base: usize, start: usize, end: usize) {
    if start < end {
        out.push(SyntaxSpan::text(base + start..base + end, SpanRole::Normal));
    }
}

/// 尝试在 `s[i]` 处解析一个行内构造；返回 `(消耗字节数, 片段)`。
fn try_construct(source: &str, s: &str, base: usize, i: usize) -> Option<(usize, Vec<SyntaxSpan>)> {
    let bytes = s.as_bytes();
    match bytes[i] {
        b'\\' => escape(base, bytes, i),
        b'`' => code_span(base, bytes, i),
        b'$' => inline_math(source, s, base, i),
        b'!' if bytes.get(i + 1) == Some(&b'[') => image(source, base, bytes, i),
        b'[' if bytes.get(i + 1) == Some(&b'[') => wiki_link(source, base, bytes, i),
        b'[' if bytes.get(i + 1) == Some(&b'^') => footnote_ref(base, bytes, i),
        b'[' => link(source, base, bytes, i),
        b'<' => html_tag_inline(source, s, base, i).or_else(|| autolink(base, s, i)),
        b'#' => tag(base, bytes, i),
        b'~' if bytes.get(i + 1) == Some(&b'~') => strike(source, base, bytes, i),
        b'*' | b'_' => emphasis(source, base, bytes, i),
        _ => None,
    }
}

/// 标签：`#tag` / `#nested/tag` / `#标签`（支持 CJK）。
///
/// `#` 后必须紧跟字母或数字（Unicode）；标签字符集为字母/数字/`-`/`_`/`/`。
/// 前一字符为字母数字时不算标签（避免 `a#b` 误判）。
fn tag(base: usize, bytes: &[u8], i: usize) -> Option<(usize, Vec<SyntaxSpan>)> {
    let s = std::str::from_utf8(bytes).ok()?;
    // 前一字符不能是字母数字（标签需在词首/空白后）
    if i > 0 {
        let prev_char = s[..i].chars().last()?;
        if prev_char.is_alphanumeric() || prev_char == '-' || prev_char == '_' {
            return None;
        }
    }
    // `#` 后必须是字母或数字（Unicode，支持 CJK）
    let after_hash = &s[i + 1..];
    let first = after_hash.chars().next()?;
    if !first.is_alphanumeric() {
        return None;
    }
    // 扫描标签字符集（按字符而非字节）
    let mut end = i + 1 + first.len_utf8();
    for c in after_hash.chars().skip(1) {
        if c.is_alphanumeric() || c == '-' || c == '_' || c == '/' {
            end += c.len_utf8();
        } else {
            break;
        }
    }
    // 末尾不能是 `/`（不允许以斜杠结尾）
    if s[..end].ends_with('/') {
        return None;
    }
    let spans = vec![SyntaxSpan::text(base + i..base + end, SpanRole::Tag)];
    Some((end - i, spans))
}

/// 转义：`\` + ASCII 标点 → 反斜杠为标记，标点为普通文本。
fn escape(base: usize, bytes: &[u8], i: usize) -> Option<(usize, Vec<SyntaxSpan>)> {
    let next = *bytes.get(i + 1)?;
    if !next.is_ascii_punctuation() {
        return None;
    }
    let spans = vec![
        SyntaxSpan::marker(base + i..base + i + 1, SpanRole::EscapeMarker),
        SyntaxSpan::text(base + i + 1..base + i + 2, SpanRole::Normal),
    ];
    Some((2, spans))
}

/// 行内代码：等长反引号包裹，内部为字面量（不再解析）。
fn code_span(base: usize, bytes: &[u8], i: usize) -> Option<(usize, Vec<SyntaxSpan>)> {
    let n = bytes[i..].iter().take_while(|&&b| b == b'`').count();
    let content_start = i + n;
    // 寻找等长反引号闭合串
    let mut j = content_start;
    while j < bytes.len() {
        if bytes[j] == b'`' {
            let run = bytes[j..].iter().take_while(|&&b| b == b'`').count();
            if run == n {
                if j <= content_start {
                    return None; // 空内容
                }
                // CommonMark：内容两端各有一个空格（且非全空格）时去掉一个空格
                let (cs, ce) = trim_code_edges(bytes, content_start, j);
                let mut spans = vec![SyntaxSpan::marker(
                    base + i..base + content_start,
                    SpanRole::InlineCode,
                )];
                if cs < ce {
                    spans.push(SyntaxSpan::text(base + cs..base + ce, SpanRole::InlineCode));
                }
                spans.push(SyntaxSpan::marker(
                    base + j..base + j + n,
                    SpanRole::InlineCode,
                ));
                return Some(((j + n) - i, spans));
            }
            j += run;
        } else {
            j += 1;
        }
    }
    None
}

/// 行内代码内容两端的空格修剪（CommonMark 规则）。
fn trim_code_edges(bytes: &[u8], start: usize, end: usize) -> (usize, usize) {
    if end > start + 1
        && bytes[start] == b' '
        && bytes[end - 1] == b' '
        && bytes[start..end].iter().any(|&b| b != b' ')
    {
        (start + 1, end - 1)
    } else {
        (start, end)
    }
}

/// 行内公式 `$…$`（D34）：两个 `$` 为**隐藏标记**，公式体是可见文本。
///
/// 判定规则（SPEC D34）：
/// - 开 `$`：左侧非字母数字（或行首）、右侧非空白；`$$` 不构成行内公式
///   （开位右侧是 `$`、或左侧紧贴 `$` 都拒绝，`$$x$$` 因此整段按普通文本处理）；
/// - 闭 `$`：左侧非空白、右侧非数字（避免 `$5` / `$10` 这类金额被误判成公式）；
/// - 公式体非空且不跨行；任一条件不成立则返回 `None`，整段按普通文本继续。
///
/// 转义 `\$` 由 [`escape`] 分支先命中，不会走到这里。
fn inline_math(source: &str, s: &str, base: usize, i: usize) -> Option<(usize, Vec<SyntaxSpan>)> {
    let bytes = s.as_bytes();
    if bytes.get(i) != Some(&b'$') || bytes.get(i + 1) == Some(&b'$') {
        return None;
    }
    let abs = base + i;
    // 开 `$` 的左侧字符可能在 region 之外（标题内容区从 `# ` 之后开始），故用源文本判断
    if abs > 0 {
        let prev = source[..abs].chars().next_back()?;
        if prev.is_alphanumeric() || prev == '$' {
            return None;
        }
    }
    // 开 `$` 右侧必须存在且非空白
    if s.get(i + 1..)
        .and_then(|t| t.chars().next())
        .is_none_or(char::is_whitespace)
    {
        return None;
    }
    // 逐个候选闭 `$`：内容非空、左邻非空白、右邻非数字
    let mut k = i + 1;
    while k < bytes.len() {
        if bytes[k] == b'$' {
            let content = &s[i + 1..k];
            let left = content.chars().next_back();
            let right = source.get(base + k + 1..).and_then(|t| t.chars().next());
            if !content.is_empty()
                && !content.contains('\n')
                && left.is_some_and(|c| !c.is_whitespace())
                && !right.is_some_and(char::is_numeric)
            {
                let spans = vec![
                    SyntaxSpan::marker(abs..abs + 1, SpanRole::MathMarker),
                    SyntaxSpan::text(abs + 1..base + k, SpanRole::Math),
                    SyntaxSpan::marker(base + k..base + k + 1, SpanRole::MathMarker),
                ];
                return Some((k + 1 - i, spans));
            }
            // 内容跨行：行内公式到此为止（region 为单行，正常不会命中）
            if content.contains('\n') {
                return None;
            }
        }
        k += 1;
    }
    None
}

/// 图片：`![alt](dest)` 整体作为 Widget（源文本隐藏）。
fn image(source: &str, base: usize, bytes: &[u8], i: usize) -> Option<(usize, Vec<SyntaxSpan>)> {
    let (consumed, _) = link_like(bytes, i + 1, source, base)?;
    // i 是 '!'，link_like 从 '[' 起算，所以整段 = 1 + consumed
    let total = consumed + 1;
    let spans = vec![SyntaxSpan {
        range: base + i..base + i + total,
        kind: SpanKind::Widget,
        role: SpanRole::ImageMarker,
        color: None,
    }];
    Some((total, spans))
}

/// Wiki-link：`[[Target]]` 或 `[[Target|Alias]]`。
///
/// 无别名时目标页名可见（`WikiLinkTarget` 为 Text）；有别名时别名可见
/// （`WikiLinkText` 为 Text）、目标隐藏（`WikiLinkTarget` 为 Marker）。
fn wiki_link(
    source: &str,
    base: usize,
    bytes: &[u8],
    i: usize,
) -> Option<(usize, Vec<SyntaxSpan>)> {
    // 跳过 `[[`
    let content_start = i + 2;
    // 找 `]]`
    let close = find_close(bytes, content_start)?;
    let content = &source[base + content_start..base + close];
    if content.is_empty() {
        return None;
    }
    let mut spans = vec![SyntaxSpan::marker(
        base + i..base + i + 2,
        SpanRole::WikiLinkMarker,
    )];
    // 按第一个 `|` 分目标与别名
    match content.find('|') {
        Some(p) => {
            let target = &content[..p];
            let alias = &content[p + 1..];
            if target.is_empty() || alias.is_empty() {
                return None;
            }
            // 目标页名（隐藏）
            spans.push(SyntaxSpan::marker(
                base + content_start..base + content_start + target.len(),
                SpanRole::WikiLinkTarget,
            ));
            // `|`（隐藏）
            spans.push(SyntaxSpan::marker(
                base + content_start + target.len()..base + content_start + target.len() + 1,
                SpanRole::WikiLinkMarker,
            ));
            // 别名（可见）
            spans.push(SyntaxSpan::text(
                base + content_start + target.len() + 1..base + close,
                SpanRole::WikiLinkText,
            ));
        }
        None => {
            // 无别名：目标页名可见
            spans.push(SyntaxSpan::text(
                base + content_start..base + close,
                SpanRole::WikiLinkTarget,
            ));
        }
    }
    // `]]`（隐藏）
    spans.push(SyntaxSpan::marker(
        base + close..base + close + 2,
        SpanRole::WikiLinkMarker,
    ));
    let total = close + 2 - i;
    Some((total, spans))
}

/// 从 `from` 起找 `]]` 的起始位置（字节偏移，相对 `bytes`）。
fn find_close(bytes: &[u8], from: usize) -> Option<usize> {
    let mut j = from;
    while j + 1 < bytes.len() {
        if bytes[j] == b']' && bytes[j + 1] == b']' {
            return Some(j);
        }
        j += 1;
    }
    None
}

/// 脚注引用：`[^label]`（SPEC D35 ①）。
///
/// `[^` 与 `]` 是隐藏标记（`FootnoteMarker`），label 是**可见文本**（`FootnoteRef`，
/// UI 用 `RunStyle::Footnote` 标注）。label 非空且不含空白与 `]`；标签紧跟 `(` 时
/// 让位给普通链接（`[^1](x)` 是"文字为 `^1` 的链接"，与 GFM 一致）。
fn footnote_ref(base: usize, bytes: &[u8], i: usize) -> Option<(usize, Vec<SyntaxSpan>)> {
    let label_start = i + 2;
    let mut j = label_start;
    while j < bytes.len() && bytes[j] != b']' {
        if bytes[j] == b'[' || bytes[j].is_ascii_whitespace() {
            return None;
        }
        j += 1;
    }
    if j >= bytes.len() || j == label_start {
        return None;
    }
    // `[^1](…)`：让给链接分支
    if bytes.get(j + 1) == Some(&b'(') {
        return None;
    }
    let spans = vec![
        SyntaxSpan::marker(base + i..base + label_start, SpanRole::FootnoteMarker),
        SyntaxSpan::text(base + label_start..base + j, SpanRole::FootnoteRef),
        SyntaxSpan::marker(base + j..base + j + 1, SpanRole::FootnoteMarker),
    ];
    Some((j + 1 - i, spans))
}

/// 链接：`[text](dest)`；text 递归解析，URL 隐藏。
fn link(source: &str, base: usize, bytes: &[u8], i: usize) -> Option<(usize, Vec<SyntaxSpan>)> {
    let (consumed, parts) = link_like(bytes, i, source, base)?;
    let LinkParts {
        text,
        bracket,
        paren_open,
        paren_close,
    } = parts;
    let mut spans = vec![SyntaxSpan::marker(
        base + i..base + i + 1,
        SpanRole::LinkMarker,
    )];
    // 链接文字递归解析，普通文本继承 LinkText 角色
    let mark = spans.len();
    scan(source, base + text.start..base + text.end, &mut spans);
    retag_normal(&mut spans[mark..], SpanRole::LinkText);
    // `](`
    spans.push(SyntaxSpan::marker(
        base + text.end..base + paren_open + 1,
        SpanRole::LinkMarker,
    ));
    // 目标 URL（隐藏，保留角色信息）
    spans.push(SyntaxSpan::marker(
        base + paren_open + 1..base + paren_close,
        SpanRole::LinkDest,
    ));
    // `)`
    spans.push(SyntaxSpan::marker(
        base + paren_close..base + paren_close + 1,
        SpanRole::LinkMarker,
    ));
    let _ = bracket;
    Some((consumed, spans))
}

/// 链接/图片的括号结构解析结果。
struct LinkParts {
    /// 显示文字（`[` 与 `]` 之间）。
    text: Range<usize>,
    /// `]` 的位置。
    bracket: usize,
    /// `(` 的位置。
    paren_open: usize,
    /// `)` 的位置。
    paren_close: usize,
}

/// 从 `open`（指向 `[`）解析 `[text](dest)`，返回 `(消耗字节, 结构)`。
fn link_like(bytes: &[u8], open: usize, _source: &str, _base: usize) -> Option<(usize, LinkParts)> {
    debug_assert_eq!(bytes.get(open), Some(&b'['));
    // 找匹配的 `]`（不处理嵌套括号；遇到换行放弃）
    let mut j = open + 1;
    while j < bytes.len() && bytes[j] != b']' {
        if bytes[j] == b'\\' {
            j += 2; // 跳过转义
            continue;
        }
        if bytes[j] == b'\n' {
            return None;
        }
        j += 1;
    }
    if j >= bytes.len() {
        return None;
    }
    let bracket = j;
    // 必须紧跟 `(`
    if bytes.get(bracket + 1) != Some(&b'(') {
        return None;
    }
    let paren_open = bracket + 1;
    let mut k = paren_open + 1;
    let mut depth = 1i32;
    while k < bytes.len() {
        match bytes[k] {
            b'\\' => k += 2,
            b'(' => {
                depth += 1;
                k += 1;
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
                k += 1;
            }
            b'\n' => return None,
            _ => k += 1,
        }
    }
    if k >= bytes.len() {
        return None;
    }
    let paren_close = k;
    let text = open + 1..bracket;
    Some((
        paren_close + 1 - open,
        LinkParts {
            text,
            bracket,
            paren_open,
            paren_close,
        },
    ))
}

/// 自动链接 `<scheme:...>` / `<mailto:...>`。
fn autolink(base: usize, s: &str, i: usize) -> Option<(usize, Vec<SyntaxSpan>)> {
    let rest = &s[i + 1..];
    let gt = rest.find('>')?;
    let inner = &rest[..gt];
    let looks_link = inner.contains("://") || inner.starts_with("mailto:") || inner.contains('@');
    if !looks_link || inner.is_empty() || inner.contains(' ') {
        return None;
    }
    let spans = vec![
        SyntaxSpan::marker(base + i..base + i + 1, SpanRole::LinkMarker),
        SyntaxSpan::text(base + i + 1..base + i + 1 + gt, SpanRole::LinkText),
        SyntaxSpan::marker(base + i + 1 + gt..base + i + 2 + gt, SpanRole::LinkMarker),
    ];
    Some((gt + 2, spans))
}

/// 简单 HTML 内联标签（Typora 常用）：`<font ...>...</font>`、`<span ...>...</span>`、
/// `<b>`/`<strong>`/`<i>`/`<em>`/`<u>`/`<s>`/`<del>` 等。
/// 仅做最小处理：剥离标签，尽量保留内容；支持 `color`/`style` 中的 color（桌面端后续应用）。
fn parse_color_attr(val: &str) -> Option<(u8, u8, u8)> {
    let v = val
        .trim()
        .trim_matches(|c: char| c == '"' || c == '\'' || c == ';');
    let vl = v.to_ascii_lowercase();
    if let Some(hex) = vl.strip_prefix('#') {
        // 先确认纯 ASCII，避免按字节切片时把多字节字符切成半个而 panic
        if !hex.is_ascii() {
            return None;
        }
        if hex.len() == 6 {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            return Some((r, g, b));
        }
        if hex.len() == 3 {
            let r = u8::from_str_radix(&hex[0..1].repeat(2), 16).ok()?;
            let g = u8::from_str_radix(&hex[1..2].repeat(2), 16).ok()?;
            let b = u8::from_str_radix(&hex[2..3].repeat(2), 16).ok()?;
            return Some((r, g, b));
        }
        return None;
    }
    if vl.starts_with("rgb(") {
        let inner = vl.trim_start_matches("rgb(").trim_end_matches(')');
        let mut parts = inner.split(',');
        let r: u8 = parts.next()?.trim().parse().ok()?;
        let g: u8 = parts.next()?.trim().parse().ok()?;
        let b: u8 = parts.next()?.trim().parse().ok()?;
        return Some((r, g, b));
    }
    // 简单命名（黑白红绿蓝常用）
    match vl.as_str() {
        "black" => Some((0, 0, 0)),
        "white" => Some((255, 255, 255)),
        "red" => Some((255, 0, 0)),
        "green" => Some((0, 128, 0)),
        "blue" => Some((0, 0, 255)),
        "gray" | "grey" => Some((128, 128, 128)),
        _ => None,
    }
}

fn parse_font_color(tag_inner: &str) -> Option<(u8, u8, u8)> {
    let tl = tag_inner.to_ascii_lowercase();
    // color="..."
    if let Some(i) = tl.find("color") {
        let rest = &tag_inner[i..];
        // 找 =
        if let Some(eq) = rest.find('=') {
            let after = rest[eq + 1..].trim();
            if after.is_empty() {
                return None;
            }
            if let Some(q @ ('"' | '\'')) = after.chars().next() {
                let body = &after[1..];
                if let Some(end) = body.find(q) {
                    return parse_color_attr(&body[..end]);
                }
            } else {
                // 空格或 > 之前
                let mut end = 0;
                let ab = after.as_bytes();
                while end < ab.len() && ab[end] != b' ' && ab[end] != b'\t' && ab[end] != b'>' {
                    end += 1;
                }
                if end > 0 {
                    return parse_color_attr(&after[..end]);
                }
            }
        }
    }
    // style="...color:rgb(...)..."
    if let Some(i) = tl.find("style") {
        let rest = &tag_inner[i..];
        if let Some(eq) = rest.find('=') {
            let after = rest[eq + 1..].trim();
            let style_str = if let Some(q @ ('"' | '\'')) = after.chars().next() {
                after[1..].find(q).map_or(after, |end| &after[1..=end])
            } else {
                after
            };
            let sl = style_str.to_ascii_lowercase();
            if let Some(ci) = sl.find("color") {
                let srest = &style_str[ci..];
                if let Some(col) = srest.find(':') {
                    let val = &srest[col + 1..];
                    // 分号前
                    let mut vend = val.len();
                    for (k, c) in val.char_indices() {
                        if c == ';' {
                            vend = k;
                            break;
                        }
                    }
                    if vend > 0 {
                        return parse_color_attr(&val[..vend]);
                    }
                }
            }
        }
    }
    None
}

/// 判断标签文本是否以 `video` / `audio` 标签名开头（含结束标签）。
fn is_media_tag(tag: &str) -> bool {
    let raw_name = tag.strip_prefix('/').unwrap_or(tag).trim_start();
    let name_end = raw_name
        .char_indices()
        .find(|(_, ch)| ch.is_whitespace() || *ch == '/')
        .map_or(raw_name.len(), |(offset, _)| offset);
    raw_name[..name_end].eq_ignore_ascii_case("video")
        || raw_name[..name_end].eq_ignore_ascii_case("audio")
}

/// 把 HTML 行内标签的语义映射到内部 span 角色与自定义颜色。
fn apply_html_style(spans: &mut [SyntaxSpan], name: &str, custom_color: Option<(u8, u8, u8)>) {
    let role = match name {
        "b" | "strong" => Some(SpanRole::Strong),
        "i" | "em" => Some(SpanRole::Emphasis),
        "s" | "del" | "strike" => Some(SpanRole::Strikethrough),
        "code" => Some(SpanRole::InlineCode),
        _ => None,
    };
    for span in spans {
        if span.kind != SpanKind::Text {
            continue;
        }
        if span.role == SpanRole::Normal
            && let Some(role) = role
        {
            span.role = role;
        }
        if let Some(color) = custom_color {
            span.color = Some(color);
        }
    }
}

fn html_tag_inline(
    source: &str,
    s: &str,
    base: usize,
    i: usize,
) -> Option<(usize, Vec<SyntaxSpan>)> {
    let bytes = s.as_bytes();
    if bytes.get(i) != Some(&b'<') {
        return None;
    }
    // `<video>` / `<audio>` 整体作为媒体占位；普通 HTML 标签仍走下面的剥壳逻辑。
    if let Some((consumed, _)) = crate::media::parse_media_element_at(s, i) {
        return Some((
            consumed,
            vec![SyntaxSpan {
                range: base + i..base + i + consumed,
                kind: SpanKind::Widget,
                role: SpanRole::MediaMarker,
                color: None,
            }],
        ));
    }
    // 查找 '>'
    let mut gt = None;
    let mut j = i + 1;
    while j < bytes.len() && bytes[j] != b'\n' {
        if bytes[j] == b'>' {
            gt = Some(j);
            break;
        }
        j += 1;
    }
    let gt = gt?;
    let tag_inner = &s[i + 1..gt];
    let tag_trim = tag_inner.trim();
    if tag_trim.is_empty() {
        return None;
    }
    // 合法完整媒体已在上方整体接管；其余 video/audio 标签必须逐字显示，不能再由
    // 通用 HTML 分支只隐藏开标签或闭标签，造成残缺文本与错误偏移。
    if is_media_tag(tag_trim) {
        return None;
    }
    // 自闭合或注释等：直接剥离
    if tag_trim.starts_with('!') || tag_trim.ends_with('/') {
        return Some((gt + 1 - i, vec![]));
    }
    // 结束标签：直接剥离
    if tag_trim.starts_with('/') {
        return Some((gt + 1 - i, vec![]));
    }
    // 起始标签：取标签名（大小写不敏感）
    let mut name_end = 0;
    let tbytes = tag_trim.as_bytes();
    while name_end < tbytes.len()
        && (tbytes[name_end].is_ascii_alphabetic() || tbytes[name_end] == b':')
    {
        name_end += 1;
    }
    if name_end == 0 {
        return None;
    }
    let name = tag_trim[..name_end].to_ascii_lowercase();
    // 仅处理常用标签
    match name.as_str() {
        "font" | "span" | "b" | "strong" | "i" | "em" | "u" | "s" | "del" | "strike" | "code"
        | "mark" => {}
        "br" => {
            // `<br>` 是行内模型无法表达的真实换行：只隐藏标签本身、不插入字符
            // （插入空格会改变源文本↔显示列映射）。跨行换行请直接回车。
            return Some((gt + 1 - i, vec![]));
        }
        _ => return None,
    }
    let custom_color = if name == "font" || name == "span" {
        parse_font_color(tag_inner)
    } else {
        None
    };
    // 找对应结束标签 </name>。**只在当前解析区间内查找**：行内解析按行/块进行，
    // 若允许越过区间，未闭合的 `<font>` 会吞掉后续多行内容并破坏偏移映射。
    let end_tag = format!("</{name}>");
    let rest_abs = base + gt + 1;
    let lower_region = s[gt + 1..].to_ascii_lowercase();
    let end_pos = lower_region.find(&end_tag)?;
    let end_abs = rest_abs + end_pos;
    // 标签之间的内容按行内语法解析，保留样式
    let mut spans = Vec::new();
    // 解析内部内容
    if end_abs > rest_abs {
        let inner_spans = parse_inline(source, rest_abs..end_abs);
        // 根据标签应用简单样式映射
        let mut mapped: Vec<SyntaxSpan> = inner_spans;
        apply_html_style(&mut mapped, &name, custom_color);
        spans.extend(mapped);
    }
    let total = (end_abs + end_tag.len()) - base - i;
    Some((total, spans))
}

/// 把 `[from..]` 内"普通文本"片段重新着色为外层角色的强调文本。
///
/// 递归解析内层后，未被更内层构造改写的普通文本应继承外层样式
/// （如 `**粗 *斜* 体**` 中的 `粗`、`体`），故仅改写 `Normal` 的 Text 片段。
fn retag_normal(spans: &mut [SyntaxSpan], role: SpanRole) {
    for s in spans {
        if s.kind == SpanKind::Text && s.role == SpanRole::Normal {
            s.role = role;
        }
    }
}

/// 删除线 `~~x~~`。
fn strike(source: &str, base: usize, bytes: &[u8], i: usize) -> Option<(usize, Vec<SyntaxSpan>)> {
    let inner_start = i + 2;
    let j = find_delim(bytes, inner_start, b'~', 2)?;
    if j <= inner_start {
        return None;
    }
    let mut spans = vec![SyntaxSpan::marker(
        base + i..base + inner_start,
        SpanRole::Strikethrough,
    )];
    let mark = spans.len();
    scan(source, base + inner_start..base + j, &mut spans);
    retag_normal(&mut spans[mark..], SpanRole::Strikethrough);
    spans.push(SyntaxSpan::marker(
        base + j..base + j + 2,
        SpanRole::Strikethrough,
    ));
    Some((j + 2 - i, spans))
}

/// 强调/粗体：`*`/`_`（`**`/`__` 为粗体）。
fn emphasis(source: &str, base: usize, bytes: &[u8], i: usize) -> Option<(usize, Vec<SyntaxSpan>)> {
    let ch = bytes[i];
    let doubled = bytes.get(i + 1) == Some(&ch);
    let n = if doubled { 2 } else { 1 };
    // `_` 不做词内强调：开分隔符前一个字符不能是字母/数字
    if ch == b'_' && prev_is_word(source, base + i) {
        return None;
    }
    let inner_start = i + n;
    let j = find_delim(bytes, inner_start, ch, n)?;
    if j <= inner_start {
        return None;
    }
    let role = if n == 2 {
        SpanRole::Strong
    } else {
        SpanRole::Emphasis
    };
    let mut spans = vec![SyntaxSpan::marker(base + i..base + inner_start, role)];
    let mark = spans.len();
    scan(source, base + inner_start..base + j, &mut spans);
    retag_normal(&mut spans[mark..], role);
    spans.push(SyntaxSpan::marker(base + j..base + j + n, role));
    Some((j + n - i, spans))
}

/// 从 `from` 起寻找长度为 `n` 的 `ch` 分隔符（跳过转义），返回其起始下标。
fn find_delim(bytes: &[u8], from: usize, ch: u8, n: usize) -> Option<usize> {
    let mut j = from;
    while j < bytes.len() {
        if bytes[j] == b'\\' {
            j += 2;
            continue;
        }
        if bytes[j] == ch {
            let run = bytes[j..].iter().take_while(|&&b| b == ch).count();
            if run >= n {
                return Some(j);
            }
            j += run;
        } else {
            j += 1;
        }
    }
    None
}

/// 绝对偏移 `abs` 前一个字符是否为字母/数字（用于 `_` 词内判断）。
fn prev_is_word(source: &str, abs: usize) -> bool {
    source[..abs]
        .chars()
        .next_back()
        .is_some_and(char::is_alphanumeric)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Vec<SyntaxSpan> {
        parse_inline(s, 0..s.len())
    }

    #[test]
    fn plain_text_single_span() {
        assert_eq!(
            parse("hello"),
            vec![SyntaxSpan::text(0..5, SpanRole::Normal)]
        );
    }

    #[test]
    fn strong_markers_and_text() {
        let spans = parse("**bold**");
        assert_eq!(spans[0], SyntaxSpan::marker(0..2, SpanRole::Strong));
        assert_eq!(spans[1], SyntaxSpan::text(2..6, SpanRole::Strong));
        assert_eq!(spans[2], SyntaxSpan::marker(6..8, SpanRole::Strong));
    }

    #[test]
    fn emphasis_markers() {
        let spans = parse("*it*");
        assert_eq!(spans[0], SyntaxSpan::marker(0..1, SpanRole::Emphasis));
        assert_eq!(spans[1], SyntaxSpan::text(1..3, SpanRole::Emphasis));
        assert_eq!(spans[2], SyntaxSpan::marker(3..4, SpanRole::Emphasis));
    }

    #[test]
    fn strikethrough() {
        let spans = parse("~~gone~~");
        assert_eq!(spans[0].role, SpanRole::Strikethrough);
        assert_eq!(spans[1], SyntaxSpan::text(2..6, SpanRole::Strikethrough));
    }

    #[test]
    fn inline_code_is_literal() {
        let spans = parse("a `x*y` b");
        assert_eq!(spans[0], SyntaxSpan::text(0..2, SpanRole::Normal));
        assert_eq!(spans[1], SyntaxSpan::marker(2..3, SpanRole::InlineCode));
        assert_eq!(spans[2], SyntaxSpan::text(3..6, SpanRole::InlineCode));
        assert_eq!(spans[3], SyntaxSpan::marker(6..7, SpanRole::InlineCode));
        assert_eq!(spans[4], SyntaxSpan::text(7..9, SpanRole::Normal));
    }

    #[test]
    fn inline_code_space_trim() {
        let spans = parse("` x `");
        assert_eq!(spans[1], SyntaxSpan::text(2..3, SpanRole::InlineCode));
    }

    #[test]
    fn link_hides_destination() {
        let spans = parse("[文字](https://x.com)");
        assert_eq!(spans[0], SyntaxSpan::marker(0..1, SpanRole::LinkMarker));
        assert_eq!(spans[1], SyntaxSpan::text(1..7, SpanRole::LinkText)); // 文字
        // 目标 URL 被隐藏
        assert!(
            spans
                .iter()
                .any(|s| s.role == SpanRole::LinkDest && s.kind == SpanKind::Marker)
        );
    }

    #[test]
    fn image_is_widget() {
        let spans = parse("![alt](img.png)");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].kind, SpanKind::Widget);
        assert_eq!(spans[0].range, 0..15);
    }

    #[test]
    fn escape_backslash_is_marker() {
        let spans = parse("\\*not\\*");
        assert_eq!(spans[0], SyntaxSpan::marker(0..1, SpanRole::EscapeMarker));
        assert_eq!(spans[1], SyntaxSpan::text(1..2, SpanRole::Normal));
    }

    #[test]
    fn intraword_underscore_not_emphasis() {
        let spans = parse("snake_case_word");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].kind, SpanKind::Text);
    }

    #[test]
    fn nested_strong_emphasis() {
        let spans = parse("**a *b* c**");
        // 外层粗体标记 + 内层斜体
        assert!(
            spans
                .iter()
                .any(|s| s.role == SpanRole::Strong && s.kind == SpanKind::Marker)
        );
        assert!(
            spans
                .iter()
                .any(|s| s.role == SpanRole::Emphasis && s.kind == SpanKind::Marker)
        );
    }

    #[test]
    fn cjk_inside_strong() {
        let spans = parse("**你好**世界");
        assert_eq!(spans[1], SyntaxSpan::text(2..8, SpanRole::Strong)); // 你好=6 字节
        assert_eq!(spans[3], SyntaxSpan::text(10..16, SpanRole::Normal)); // 世界
    }

    #[test]
    fn visible_runs_after_inline() {
        let spans = parse("**粗**体");
        let runs = crate::span::visible_runs(&spans);
        assert_eq!(runs, vec![2..5, 7..10]); // 粗、体（各 3 字节）
    }

    #[test]
    fn wikilink_no_alias() {
        let src = "[[Page]]";
        let spans = parse_inline(src, 0..src.len());
        // `[[` 标记 + 目标（可见 Text）+ `]]` 标记
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].role, SpanRole::WikiLinkMarker);
        assert_eq!(spans[0].kind, SpanKind::Marker);
        assert_eq!(spans[1].role, SpanRole::WikiLinkTarget);
        assert_eq!(spans[1].kind, SpanKind::Text, "无别名时目标可见");
        assert_eq!(&src[spans[1].range.clone()], "Page");
        assert_eq!(spans[2].role, SpanRole::WikiLinkMarker);
    }

    #[test]
    fn wikilink_with_alias() {
        let src = "[[Page|别名]]";
        let spans = parse_inline(src, 0..src.len());
        // `[[` + 目标（隐藏）+ `|` + 别名（可见）+ `]]`
        assert_eq!(spans.len(), 5);
        assert_eq!(spans[1].role, SpanRole::WikiLinkTarget);
        assert_eq!(spans[1].kind, SpanKind::Marker, "有别名时目标隐藏");
        assert_eq!(spans[3].role, SpanRole::WikiLinkText);
        assert_eq!(spans[3].kind, SpanKind::Text, "别名可见");
        assert_eq!(&src[spans[3].range.clone()], "别名");
    }

    #[test]
    fn wikilink_empty_rejected() {
        // ` [[] ` 空内容不构成 wiki-link
        let src = "[[]]";
        let spans = parse_inline(src, 0..src.len());
        assert!(spans.iter().all(|s| s.role != SpanRole::WikiLinkMarker));
    }

    #[test]
    fn wikilink_empty_alias_rejected() {
        // `[[Page|]]` 空别名不构成
        let src = "[[Page|]]";
        let spans = parse_inline(src, 0..src.len());
        assert!(spans.iter().all(|s| s.role != SpanRole::WikiLinkMarker));
    }

    #[test]
    fn wikilink_inline_mixed_with_text() {
        let src = "前缀 [[Page]] 后缀";
        let spans = parse_inline(src, 0..src.len());
        // 前缀 Normal + wiki-link 3 段 + 后缀 Normal
        assert!(spans.iter().any(|s| s.role == SpanRole::WikiLinkMarker));
        assert!(spans.iter().any(|s| s.role == SpanRole::WikiLinkTarget));
    }

    #[test]
    fn tag_simple() {
        let src = "#标签";
        let spans = parse_inline(src, 0..src.len());
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].role, SpanRole::Tag);
        assert_eq!(spans[0].kind, SpanKind::Text, "标签可见");
        assert_eq!(&src[spans[0].range.clone()], "#标签");
    }

    #[test]
    fn tag_nested() {
        let src = "#nested/tag";
        let spans = parse_inline(src, 0..src.len());
        assert_eq!(spans.len(), 1);
        assert_eq!(&src[spans[0].range.clone()], "#nested/tag");
    }

    #[test]
    fn tag_with_dash_underscore() {
        let src = "#my-tag_2";
        let spans = parse_inline(src, 0..src.len());
        assert_eq!(spans.len(), 1);
        assert_eq!(&src[spans[0].range.clone()], "#my-tag_2");
    }

    #[test]
    fn tag_not_after_word_char() {
        // `a#b` 不算标签
        let src = "a#b";
        let spans = parse_inline(src, 0..src.len());
        assert!(spans.iter().all(|s| s.role != SpanRole::Tag));
    }

    #[test]
    fn tag_hash_alone_not_tag() {
        // `#` 后无字母数字
        let src = "# hello";
        let spans = parse_inline(src, 0..src.len());
        assert!(spans.iter().all(|s| s.role != SpanRole::Tag));
    }

    #[test]
    fn tag_trailing_slash_rejected() {
        // 末尾 `/` 不算
        let src = "#tag/";
        let spans = parse_inline(src, 0..src.len());
        assert!(spans.iter().all(|s| s.role != SpanRole::Tag));
    }

    #[test]
    fn tag_mixed_with_text() {
        let src = "前缀 #标签 后缀";
        let spans = parse_inline(src, 0..src.len());
        assert!(spans.iter().any(|s| s.role == SpanRole::Tag));
    }

    #[test]
    fn tag_escaped_not_tag() {
        // `\#tag` 转义后不算标签
        let src = "\\#tag";
        let spans = parse_inline(src, 0..src.len());
        assert!(spans.iter().all(|s| s.role != SpanRole::Tag));
    }

    // ---------------- 行内公式 $…$（D34） ----------------

    #[test]
    fn inline_math_basic() {
        // 定界符隐藏、公式体可见
        let src = "a $x^2$ b";
        let spans = parse(src);
        assert_eq!(spans.len(), 5, "{spans:?}");
        assert_eq!(spans[0], SyntaxSpan::text(0..2, SpanRole::Normal));
        assert_eq!(spans[1], SyntaxSpan::marker(2..3, SpanRole::MathMarker));
        assert_eq!(spans[2], SyntaxSpan::text(3..6, SpanRole::Math));
        assert_eq!(spans[3], SyntaxSpan::marker(6..7, SpanRole::MathMarker));
        assert_eq!(spans[4], SyntaxSpan::text(7..9, SpanRole::Normal));
        // 可见区间 = 去掉两个 `$`
        assert_eq!(crate::span::visible_runs(&spans), vec![0..2, 3..6, 7..9]);
    }

    #[test]
    fn inline_math_after_cjk_with_space() {
        // 左邻空白（非字母数字）成立；中文正文里的 `$x$` 正常识别
        let src = "当 $x$ 时";
        let spans = parse(src);
        assert!(spans.iter().any(|s| s.role == SpanRole::Math));
    }

    #[test]
    fn inline_math_left_alnum_rejected() {
        // 左邻是字母 → 不开公式（词内 `$` 不误判）
        let spans = parse("a$x$");
        assert!(spans.iter().all(|s| s.role != SpanRole::Math));
    }

    #[test]
    fn inline_math_currency_not_math() {
        // 金额：闭 `$` 右侧是空白/数字时都不成立
        let spans = parse("$5 and $10");
        assert!(spans.iter().all(|s| s.role != SpanRole::Math));
    }

    #[test]
    fn inline_math_close_before_digit_rejected() {
        let spans = parse("$x$5");
        assert!(spans.iter().all(|s| s.role != SpanRole::Math));
    }

    #[test]
    fn inline_math_double_dollar_not_math() {
        // `$$x$$` 行内不构成行内公式（行首的才交给块级解析）
        let src = "$$x$$";
        let spans = parse(src);
        assert!(spans.iter().all(|s| s.role != SpanRole::Math));
        assert_eq!(crate::span::visible_runs(&spans), vec![0..src.len()]);
    }

    #[test]
    fn inline_math_empty_or_blank_body_rejected() {
        assert!(parse("$  $").iter().all(|s| s.role != SpanRole::Math));
        assert!(parse("$").iter().all(|s| s.role != SpanRole::Math));
    }

    #[test]
    fn inline_math_escaped_dollar_not_math() {
        // `\$x$`：反斜杠转义先命中，剩余 `$` 不再开公式
        let src = "\\$x$";
        let spans = parse(src);
        assert!(spans.iter().all(|s| s.role != SpanRole::Math));
    }

    // ---------------- 内联 HTML 标签（D56） ----------------

    #[test]
    fn html_font_named_color() {
        let src = "<font color=\"red\">红</font>";
        let spans = parse(src);
        // 标签本身隐藏，只留内部可见文本
        assert_eq!(spans.len(), 1, "{spans:?}");
        assert_eq!(&src[spans[0].range.clone()], "红");
        assert_eq!(spans[0].color, Some((255, 0, 0)));
        assert_eq!(crate::span::visible_runs(&spans), vec![18..21]);
    }

    #[test]
    fn html_span_style_hex_color() {
        let src = "<span style=\"color:#0af\">x</span>";
        let spans = parse(src);
        assert_eq!(spans.len(), 1, "{spans:?}");
        assert_eq!(spans[0].color, Some((0, 170, 255)));
    }

    #[test]
    fn html_font_rgb_color() {
        let src = "<font color=\"rgb(1,2,3)\">x</font>";
        let spans = parse(src);
        assert_eq!(spans[0].color, Some((1, 2, 3)));
    }

    #[test]
    fn html_strong_maps_to_strong_role() {
        let src = "<strong>粗</strong>";
        let spans = parse(src);
        assert_eq!(spans.len(), 1, "{spans:?}");
        assert_eq!(spans[0].role, SpanRole::Strong);
        assert_eq!(spans[0].kind, SpanKind::Text);
    }

    #[test]
    fn html_br_hidden_without_inserting_text() {
        // 行内模型不能真正换行：`<br>` 只隐藏标签，不能凭空插入空格
        let src = "a<br>b";
        let spans = parse(src);
        assert_eq!(crate::span::visible_runs(&spans), vec![0..1, 5..6]);
    }

    #[test]
    fn media_tag_is_widget() {
        let src = r#"a <video src="x.mp4"></video> b"#;
        let spans = parse(src);
        assert_eq!(spans.len(), 3, "{spans:?}");
        assert_eq!(spans[0], SyntaxSpan::text(0..2, SpanRole::Normal));
        assert_eq!(spans[1].kind, SpanKind::Widget);
        assert_eq!(spans[1].role, SpanRole::MediaMarker);
        assert_eq!(
            &src[spans[1].range.clone()],
            "<video src=\"x.mp4\"></video>"
        );
        assert_eq!(
            crate::span::visible_runs(&spans),
            vec![0..2, spans[2].range.clone()]
        );
    }

    #[test]
    fn media_without_src_stays_literal() {
        let src = "<video controls></video>";
        let spans = parse(src);
        assert_eq!(crate::span::visible_runs(&spans), vec![0..src.len()]);
    }

    #[test]
    fn media_multiline_stays_literal() {
        let src = "<video src=\"a.mp4\">\ntext</video>";
        let line_end = src.find('\n').unwrap();
        let spans = parse_inline(src, 0..line_end);
        assert_eq!(crate::span::visible_runs(&spans), vec![0..line_end]);
    }

    #[test]
    fn html_unclosed_tag_stays_literal() {
        let src = "a <font color=red>b";
        let spans = parse(src);
        assert_eq!(
            crate::span::visible_runs(&spans),
            vec![0..src.len()],
            "未闭合标签应按普通文本显示"
        );
    }

    #[test]
    fn html_closing_tag_outside_region_not_swallowed() {
        // 结束标签在下一行：当前区间内不得吞掉后续内容
        let src = "a <font color=red>b\nz</font>";
        let line_end = src.find('\n').unwrap();
        let spans = parse_inline(src, 0..line_end);
        assert_eq!(crate::span::visible_runs(&spans), vec![0..line_end]);
    }

    #[test]
    fn html_bad_hex_color_does_not_panic() {
        // 非 ASCII / 位数不对的十六进制颜色应安全忽略
        let spans = parse("<font color=\"#éa\">x</font>");
        assert_eq!(spans.len(), 1, "{spans:?}");
        assert_eq!(spans[0].color, None);
    }

    #[test]
    fn html_nested_emphasis_inside_font() {
        let src = "<font color=\"#ff0000\">a *b* c</font>";
        let spans = parse(src);
        // 外层颜色施加到所有文本片段，内层斜体保留
        assert!(
            spans
                .iter()
                .filter(|s| s.kind == SpanKind::Text)
                .all(|s| s.color == Some((255, 0, 0)))
        );
        assert!(
            spans
                .iter()
                .any(|s| s.role == SpanRole::Emphasis && s.kind == SpanKind::Marker)
        );
    }
}
