//! 行扫描块级解析器（SPEC §8，决策 D23）。
//!
//! 采用**行优先**扫描：先把源文本切成行，再逐行识别块的首缀（`#`、`>`、`-`、``` 等），
//! 所有区间都直接落在源字节偏移上——这正是 WYSIWYG 精确标记显隐与后续"脏块增量重解析"
//! 所需要的基础（`pulldown-cmark` 0.13 不暴露块级偏移，故自研）。
//!
//! MVP 支持的块：ATX/Setext 标题、段落、分割线、围栏/缩进代码块、引用、列表
//! （有序/无序/任务）。表格、脚注、HTML 块、惰性续行等留待后续迭代。

use crate::block::{Block, BlockKind};
use crate::span::{SpanRole, SyntaxSpan};

/// 一行（去除换行后）在源文本中的位置。
///
/// `content` 用于容器前缀剥离：引用/列表项内部的子扫描会把 `content` 前移，
/// 使同一套扫描逻辑既能处理顶层行、也能处理"剥掉 `> ` 前缀后"的行，而无需重造字符串。
#[derive(Debug, Clone, Copy)]
struct Line {
    /// 行首字节偏移（含容器前缀）。
    start: usize,
    /// 行尾字节偏移（不含换行符）。
    end: usize,
    /// 内容起点（跳过容器前缀后）。
    content: usize,
}

impl Line {
    /// 该行的内容切片。
    fn rest<'a>(&self, source: &'a str) -> &'a str {
        &source[self.content..self.end]
    }

    /// 顶层行（无前缀）。
    fn plain(start: usize, end: usize) -> Self {
        Self {
            start,
            end,
            content: start,
        }
    }
}

/// 把源文本按行切开（保留每行的起止字节；去掉行尾的 `\r`）。
fn split_lines(source: &str) -> Vec<Line> {
    let bytes = source.as_bytes();
    let mut lines = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            let mut end = i;
            if end > start && bytes[end - 1] == b'\r' {
                end -= 1;
            }
            lines.push(Line::plain(start, end));
            start = i + 1;
        }
        i += 1;
    }
    if start < bytes.len() {
        lines.push(Line::plain(start, bytes.len()));
    }
    lines
}

/// 前导空格数（制表符按 1 计，MVP 简化）。
fn indent(rest: &str) -> usize {
    rest.bytes().take_while(|&b| b == b' ').count()
}

/// 行是否为空行（去空白后为空）。
fn is_blank(rest: &str) -> bool {
    rest.trim().is_empty()
}

/// 去掉行尾空白后的字节长度（相对 rest）。
fn trim_end_len(rest: &str) -> usize {
    rest.trim_end().len()
}

/// 解析 ATX 标题：`#{1,6}` + 空白。返回 `(level, 前缀标记长度, 内容结束相对长度)`。
///
/// 前缀标记包含前导空格、`#` 与紧随的空白；若存在结尾的闭合式 `#`（` #+`），
/// 也计入内容结束之前（即内容不含尾随 `#`）。
fn atx_heading(rest: &str) -> Option<(u8, usize, usize)> {
    let ind = indent(rest);
    if ind > 3 {
        return None;
    }
    let after = &rest[ind..];
    let hashes = after.bytes().take_while(|&b| b == b'#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    // `#` 之后必须是空白或行尾
    let next = after.as_bytes().get(hashes).copied();
    if !matches!(next, None | Some(b' ' | b'\t')) {
        return None;
    }
    let level = u8::try_from(hashes).ok()?;
    // 前缀标记：前导空格 + `#` + 紧随的空白
    let mut prefix_end = ind + hashes;
    while matches!(rest.as_bytes().get(prefix_end), Some(b' ' | b'\t')) {
        prefix_end += 1;
    }
    // 内容范围：从 prefix_end 到行尾（trim 掉尾部空白与闭合式 `#`）
    let body = &rest[prefix_end..];
    let mut content_len = trim_end_len(body);
    // 检测结尾闭合式 `#+`（前面至少一个空格）
    let trimmed = &body[..content_len];
    if let Some(pos) = closing_hashes_pos(trimmed) {
        content_len = pos;
    }
    Some((level, prefix_end, content_len))
}

/// 若 `s`（已去尾空白）以 ` #+` 结尾，返回闭合式 `#` 的起点相对偏移。
fn closing_hashes_pos(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = bytes.len();
    while i > 0 && bytes[i - 1] == b'#' {
        i -= 1;
    }
    if i == bytes.len() {
        return None; // 没有尾部 `#`
    }
    // 闭合式 `#` 前必须有一个空白
    if i > 0 && bytes[i - 1] == b' ' {
        // 该空白也归入标记，返回它之前
        let mut j = i - 1;
        while j > 0 && bytes[j - 1] == b' ' {
            j -= 1;
        }
        Some(j)
    } else {
        None
    }
}

/// Setext 下划线：`===`（1 级）或 `---`（2 级）。返回级别。
fn setext_underline(rest: &str) -> Option<u8> {
    let ind = indent(rest);
    if ind > 3 {
        return None;
    }
    let t = rest[ind..].trim_end();
    if t.is_empty() {
        return None;
    }
    if t.bytes().all(|b| b == b'=') {
        Some(1)
    } else if t.bytes().all(|b| b == b'-') {
        Some(2)
    } else {
        None
    }
}

/// 分割线：`***`/`---`/`___`，至少 3 个，字符间可含空格。
fn is_thematic_break(rest: &str) -> bool {
    let ind = indent(rest);
    if ind > 3 {
        return false;
    }
    let t = rest[ind..].trim_end();
    let mut chars = t.bytes().filter(|b| *b != b' ');
    let Some(first) = chars.next() else {
        return false;
    };
    if !matches!(first, b'*' | b'-' | b'_') {
        return false;
    }
    let mut count = 1;
    for b in chars {
        if b != first {
            return false;
        }
        count += 1;
    }
    count >= 3
}

/// 围栏开启行：返回 `(围栏字符, 长度, 语言标识)`。
fn fence_open(rest: &str) -> Option<(u8, usize, Option<String>)> {
    let ind = indent(rest);
    if ind > 3 {
        return None;
    }
    let after = &rest.as_bytes()[ind..];
    let ch = *after.first()?;
    if ch != b'`' && ch != b'~' {
        return None;
    }
    let len = after.iter().take_while(|&&b| b == ch).count();
    if len < 3 {
        return None;
    }
    let info = rest[ind + len..].trim();
    let lang = info.split_whitespace().next().map(str::to_string);
    Some((ch, len, lang))
}

/// 是否为匹配的闭合围栏行。
fn is_closing_fence(rest: &str, ch: u8, len: usize) -> bool {
    let ind = indent(rest);
    if ind > 3 {
        return false;
    }
    let t = rest[ind..].trim_end().as_bytes();
    if t.is_empty() || t[0] != ch {
        return false;
    }
    let run = t.iter().take_while(|&&b| b == ch).count();
    run >= len && t[run..].iter().all(|&b| b == b' ' || b == ch)
}

/// 引用前缀 `>`（前导最多 3 空格 + `>` + 可选一个空格）。返回前缀字节长度。
fn quote_prefix(rest: &str) -> Option<usize> {
    let ind = indent(rest);
    if ind > 3 {
        return None;
    }
    if rest.as_bytes().get(ind) != Some(&b'>') {
        return None;
    }
    let mut n = ind + 1;
    if matches!(rest.as_bytes().get(n), Some(b' ')) {
        n += 1;
    }
    Some(n)
}

/// 列表项标记信息。
#[derive(Debug, Clone, Copy)]
struct ListMarker {
    /// 是否有序。
    ordered: bool,
    /// 起始序号（无序为 1）。
    start: u64,
    /// 标记字节长度（含前导空格与后随空白），用于定位内容起点。
    len: usize,
    /// 任务复选框状态（若存在）。
    task: Option<bool>,
    /// 该项的缩进（前导空格数）。
    indent: usize,
}

/// 解析列表项标记：`-`/`+`/`*` 或 `1.`/`1)`，后跟空白；再尝试解析任务复选框。
fn list_marker(rest: &str) -> Option<ListMarker> {
    let ind = indent(rest);
    let after = &rest[ind..];
    let bytes = after.as_bytes();
    let (ordered, start, marker_len) = if let Some(&first) = bytes.first() {
        if matches!(first, b'-' | b'+' | b'*') {
            (false, 1u64, 1usize)
        } else if first.is_ascii_digit() {
            let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
            if digits > 9 {
                return None;
            }
            let delim = *bytes.get(digits)?;
            if delim != b'.' && delim != b')' {
                return None;
            }
            let n: u64 = after[..digits].parse().ok()?;
            (true, n, digits + 1)
        } else {
            return None;
        }
    } else {
        return None;
    };
    // 标记后必须是空白（或行尾）
    let after_marker = ind + marker_len;
    if !matches!(rest.as_bytes().get(after_marker), None | Some(b' ' | b'\t')) {
        return None;
    }
    // 跳过标记后的空白
    let mut content = after_marker;
    while matches!(rest.as_bytes().get(content), Some(b' ' | b'\t')) {
        content += 1;
    }
    let len = content;
    // 任务复选框 `[ ]` / `[x]`
    let (task, len) = parse_task(source_slice(rest, content), len);
    Some(ListMarker {
        ordered,
        start,
        len,
        task,
        indent: ind,
    })
}

/// 从 `content` 起尝试解析任务复选框；返回 `(状态, 新的标记长度)`。
fn parse_task(rest_at_content: &str, content: usize) -> (Option<bool>, usize) {
    let b = rest_at_content.as_bytes();
    if b.len() >= 3 && b[0] == b'[' && b[2] == b']' {
        let state = match b[1] {
            b' ' => Some(false),
            b'x' | b'X' => Some(true),
            _ => None,
        };
        if let Some(s) = state {
            let mut after = content + 3;
            // 复选框后若是空白则一并吞掉
            if matches!(b.get(3), Some(b' ' | b'\t')) {
                after += 1;
            }
            return (Some(s), after);
        }
    }
    (None, content)
}

/// 取 `rest` 中从 `at` 起的子串（越界返回空串）。
fn source_slice(rest: &str, at: usize) -> &str {
    rest.get(at..).unwrap_or("")
}

/// 该行是否开启一个新的非段落块（用于段落终止判断）。
fn starts_new_block(source: &str, line: Line) -> bool {
    let rest = line.rest(source);
    atx_heading(rest).is_some()
        || fence_open(rest).is_some()
        || is_thematic_break(rest)
        || quote_prefix(rest).is_some()
        || list_marker(rest).is_some()
}

/// 解析源文本为顶层块森林。
pub fn parse(source: &str) -> Vec<Block> {
    let lines = split_lines(source);
    scan_blocks(source, &lines)
}

/// 对一组（可能是前缀剥离后的）行做块级扫描。
fn scan_blocks(source: &str, lines: &[Line]) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        let rest = line.rest(source);
        if is_blank(rest) {
            i += 1;
            continue;
        }
        if let Some((level, prefix_end, content_len)) = atx_heading(rest) {
            blocks.push(build_atx_heading(
                source,
                line,
                level,
                prefix_end,
                content_len,
            ));
            i += 1;
        } else if is_thematic_break(rest) {
            let spans = vec![SyntaxSpan::marker(
                line.content..line.end,
                SpanRole::ThematicBreak,
            )];
            blocks.push(Block::leaf(
                BlockKind::ThematicBreak,
                line.start..line.end,
                spans,
            ));
            i += 1;
        } else if let Some((ch, len, lang)) = fence_open(rest) {
            let (block, next) = build_fenced_code(source, lines, i, ch, len, lang);
            blocks.push(block);
            i = next;
        } else if quote_prefix(rest).is_some() {
            let (block, next) = build_block_quote(source, lines, i);
            blocks.push(block);
            i = next;
        } else if list_marker(rest).is_some() {
            let (block, next) = build_list(source, lines, i);
            blocks.push(block);
            i = next;
        } else {
            let (block, next) = build_paragraph(source, lines, i);
            blocks.push(block);
            i = next;
        }
    }
    blocks
}

/// 构造 ATX 标题块。
fn build_atx_heading(
    source: &str,
    line: Line,
    level: u8,
    prefix_end: usize,
    content_len: usize,
) -> Block {
    let content_start = line.content + prefix_end;
    let content_end = content_start + content_len;
    let mut spans = vec![SyntaxSpan::marker(
        line.content..content_start,
        SpanRole::HeadingMarker,
    )];
    if content_start < content_end {
        // 标题正文按行内语法解析（粗体/斜体/代码/链接…）
        spans.extend(crate::inline::parse_inline(
            source,
            content_start..content_end,
        ));
    }
    // 结尾闭合式 `#`
    if content_end < line.end {
        spans.push(SyntaxSpan::marker(
            content_end..line.end,
            SpanRole::HeadingMarker,
        ));
    }
    Block::leaf(BlockKind::Heading { level }, line.start..line.end, spans)
}

/// 构造围栏代码块；返回块与下一个待处理行号。
fn build_fenced_code(
    source: &str,
    lines: &[Line],
    open_idx: usize,
    ch: u8,
    len: usize,
    lang: Option<String>,
) -> (Block, usize) {
    let open_line = lines[open_idx];
    let mut spans = vec![SyntaxSpan::marker(
        open_line.content..open_line.end,
        SpanRole::FenceMarker,
    )];
    let mut i = open_idx + 1;
    let mut closed: Option<Line> = None;
    let mut last_end = open_line.end;
    while i < lines.len() {
        let l = lines[i];
        if is_closing_fence(l.rest(source), ch, len) {
            closed = Some(l);
            last_end = l.end;
            i += 1;
            break;
        }
        // 代码内容按原样（content == start，无容器前缀）
        spans.push(SyntaxSpan::text(l.start..l.end, SpanRole::CodeContent));
        last_end = l.end;
        i += 1;
    }
    if let Some(c) = closed {
        spans.push(SyntaxSpan::marker(c.start..c.end, SpanRole::FenceMarker));
    }
    let kind = BlockKind::CodeBlock { lang, fenced: true };
    (Block::leaf(kind, open_line.start..last_end, spans), i)
}

/// 构造引用块（仅支持带 `>` 前缀的续行，暂不支持惰性续行）。
fn build_block_quote(source: &str, lines: &[Line], start_idx: usize) -> (Block, usize) {
    let mut spans = Vec::new();
    let mut inner: Vec<Line> = Vec::new();
    let first_start = lines[start_idx].start;
    let mut last_end = lines[start_idx].end;
    let mut i = start_idx;
    while i < lines.len() {
        let l = lines[i];
        let rest = l.rest(source);
        let Some(consumed) = quote_prefix(rest) else {
            break;
        };
        spans.push(SyntaxSpan::marker(
            l.content..l.content + consumed,
            SpanRole::QuoteMarker,
        ));
        inner.push(Line {
            start: l.start,
            end: l.end,
            content: l.content + consumed,
        });
        last_end = l.end;
        i += 1;
    }
    let children = scan_blocks(source, &inner);
    (
        Block::container(
            BlockKind::BlockQuote,
            first_start..last_end,
            spans,
            children,
        ),
        i,
    )
}

/// 构造列表（容器 + 若干列表项）。
fn build_list(source: &str, lines: &[Line], start_idx: usize) -> (Block, usize) {
    let first = list_marker(lines[start_idx].rest(source)).expect("调用方已确认是列表");
    let ordered = first.ordered;
    let start = first.start;
    let list_start = lines[start_idx].start;
    let mut items = Vec::new();
    let mut i = start_idx;
    let mut list_end = lines[start_idx].end;
    while i < lines.len() {
        let l = lines[i];
        let rest = l.rest(source);
        if is_blank(rest) {
            break;
        }
        let Some(marker) = list_marker(rest) else {
            break;
        };
        if marker.ordered != ordered || marker.indent != first.indent {
            break; // 不同列表/缩进：结束当前列表
        }
        let (item, next) = build_list_item(source, lines, i, &marker);
        list_end = item.range.end;
        items.push(item);
        i = next;
    }
    let kind = BlockKind::List { ordered, start };
    (
        Block::container(kind, list_start..list_end, Vec::new(), items),
        i,
    )
}

/// 构造单个列表项（含其缩进续行）。
fn build_list_item(
    source: &str,
    lines: &[Line],
    idx: usize,
    marker: &ListMarker,
) -> (Block, usize) {
    let l = lines[idx];
    let spans = vec![SyntaxSpan::marker(
        l.content..l.content + marker.len,
        marker_role(marker),
    )];
    let content = l.content + marker.len;
    let mut inner = vec![Line {
        start: l.start,
        end: l.end,
        content,
    }];
    let mut i = idx + 1;
    while i < lines.len() {
        let nl = lines[i];
        let nrest = nl.rest(source);
        if is_blank(nrest) {
            break;
        }
        // 同级或更浅的列表标记 → 下一个项；否则视为续行
        if let Some(m) = list_marker(nrest) {
            if m.indent <= marker.indent {
                break;
            }
        }
        // 续行：保留其余部分（缩进已在 content 中被后续扫描处理）
        let stripped = indent(nrest).min(nl.content + marker.len);
        let content = (nl.content + stripped).min(nl.end);
        inner.push(Line {
            start: nl.start,
            end: nl.end,
            content,
        });
        i += 1;
    }
    let children = scan_blocks(source, &inner);
    let range = l.start..inner.last().map_or(l.end, |x| x.end);
    let kind = BlockKind::ListItem { task: marker.task };
    (Block::container(kind, range, spans, children), i)
}

/// 列表项标记的角色（任务项用 TaskMarker，其它用 ListMarker）。
fn marker_role(marker: &ListMarker) -> SpanRole {
    if marker.task.is_some() {
        SpanRole::TaskMarker
    } else {
        SpanRole::ListMarker
    }
}

/// 构造段落；若其后紧跟 Setext 下划线，则升级为标题。
fn build_paragraph(source: &str, lines: &[Line], start_idx: usize) -> (Block, usize) {
    let mut para: Vec<Line> = Vec::new();
    let mut i = start_idx;
    let mut heading: Option<(u8, Line)> = None;
    while i < lines.len() {
        let l = lines[i];
        let rest = l.rest(source);
        if is_blank(rest) {
            break;
        }
        if !para.is_empty() {
            if let Some(level) = setext_underline(rest) {
                heading = Some((level, l));
                i += 1;
                break;
            }
            if starts_new_block(source, l) {
                break;
            }
        }
        para.push(l);
        i += 1;
    }
    let start = para.first().map_or(lines[start_idx].start, |l| l.start);
    let mut end = para.last().map_or(lines[start_idx].end, |l| l.end);
    let mut spans = paragraph_spans(source, &para);
    let kind = if let Some((level, underline)) = heading {
        // Setext 下划线整行作为标记
        spans.push(SyntaxSpan::marker(
            underline.content..underline.end,
            SpanRole::HeadingMarker,
        ));
        end = underline.end;
        BlockKind::Heading { level }
    } else {
        BlockKind::Paragraph
    };
    (Block::leaf(kind, start..end, spans), i)
}

/// 逐行生成段落片段：内容按行内语法解析 + 行间软/硬换行标记。
fn paragraph_spans(source: &str, para: &[Line]) -> Vec<SyntaxSpan> {
    let mut spans = Vec::new();
    for (k, l) in para.iter().enumerate() {
        // 行尾连续两个及以上空格 = 硬换行（这些空格归为标记）
        let content = &source[l.content..l.end];
        let trailing = content.len() - content.trim_end_matches(' ').len();
        let hard = trailing >= 2;
        let text_end = if hard { l.end - trailing } else { l.end };
        spans.extend(crate::inline::parse_inline(source, l.content..text_end));
        if hard {
            spans.push(SyntaxSpan::marker(text_end..l.end, SpanRole::HardBreak));
        }
        if k + 1 < para.len() {
            spans.push(SyntaxSpan::marker(
                l.end..para[k + 1].start,
                SpanRole::SoftBreak,
            ));
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::visible_runs;

    #[test]
    fn single_paragraph() {
        let blocks = parse("hello world\n");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, BlockKind::Paragraph);
        assert_eq!(blocks[0].range, 0..11);
    }

    #[test]
    fn paragraph_soft_break_marker() {
        let src = "line1\nline2\n";
        let blocks = parse(src);
        assert_eq!(blocks.len(), 1);
        let spans = &blocks[0].spans;
        assert_eq!(spans[0], SyntaxSpan::text(0..5, SpanRole::Normal));
        assert_eq!(spans[1], SyntaxSpan::marker(5..6, SpanRole::SoftBreak));
        assert_eq!(spans[2], SyntaxSpan::text(6..11, SpanRole::Normal));
    }

    #[test]
    fn atx_heading_with_closing_hashes() {
        let src = "## 标题 ##\n";
        let blocks = parse(src);
        assert_eq!(blocks[0].kind, BlockKind::Heading { level: 2 });
        assert_eq!(blocks[0].range, 0..12);
        assert_eq!(
            blocks[0].spans[0],
            SyntaxSpan::marker(0..3, SpanRole::HeadingMarker)
        );
        assert_eq!(blocks[0].spans[1], SyntaxSpan::text(3..9, SpanRole::Normal));
        assert_eq!(
            blocks[0].spans[2],
            SyntaxSpan::marker(9..12, SpanRole::HeadingMarker)
        );
    }

    #[test]
    fn atx_levels_and_leading_spaces() {
        assert_eq!(parse("# a\n")[0].kind, BlockKind::Heading { level: 1 });
        assert_eq!(parse("###### f\n")[0].kind, BlockKind::Heading { level: 6 });
        assert_eq!(
            parse("   ### indented\n")[0].kind,
            BlockKind::Heading { level: 3 }
        );
        // 7 个 # 不是标题，而是段落
        assert_eq!(parse("####### x\n")[0].kind, BlockKind::Paragraph);
    }

    #[test]
    fn setext_heading() {
        let src = "标题\n===\n";
        let blocks = parse(src);
        assert_eq!(blocks[0].kind, BlockKind::Heading { level: 1 });
        assert_eq!(blocks[0].range, 0..10);
        assert_eq!(
            blocks[0].spans.last().unwrap(),
            &SyntaxSpan::marker(7..10, SpanRole::HeadingMarker)
        );
        let src2 = "sub\n---\n";
        assert_eq!(parse(src2)[0].kind, BlockKind::Heading { level: 2 });
    }

    #[test]
    fn thematic_break() {
        for s in ["---\n", "***\n", "___\n", "- - -\n", "  * * *\n"] {
            let blocks = parse(s);
            assert_eq!(blocks.len(), 1, "{s:?}");
            assert_eq!(blocks[0].kind, BlockKind::ThematicBreak);
            assert_eq!(blocks[0].spans[0].role, SpanRole::ThematicBreak);
        }
    }

    #[test]
    fn fenced_code_block() {
        let src = "```rust\nlet x = 1;\n```\n";
        let blocks = parse(src);
        assert_eq!(
            blocks[0].kind,
            BlockKind::CodeBlock {
                lang: Some("rust".into()),
                fenced: true
            }
        );
        assert_eq!(blocks[0].range, 0..22);
        assert_eq!(
            blocks[0].spans[0],
            SyntaxSpan::marker(0..7, SpanRole::FenceMarker)
        );
        assert_eq!(
            blocks[0].spans[1],
            SyntaxSpan::text(8..18, SpanRole::CodeContent)
        );
        assert_eq!(
            blocks[0].spans[2],
            SyntaxSpan::marker(19..22, SpanRole::FenceMarker)
        );
    }

    #[test]
    fn block_quote_with_children() {
        let src = "> hello\n> world\n";
        let blocks = parse(src);
        assert_eq!(blocks[0].kind, BlockKind::BlockQuote);
        assert_eq!(blocks[0].range, 0..15);
        assert_eq!(
            blocks[0].spans[0],
            SyntaxSpan::marker(0..2, SpanRole::QuoteMarker)
        );
        assert_eq!(blocks[0].children.len(), 1);
        assert_eq!(blocks[0].children[0].kind, BlockKind::Paragraph);
    }

    #[test]
    fn unordered_list_items() {
        let src = "- a\n- b\n";
        let blocks = parse(src);
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            blocks[0].kind,
            BlockKind::List {
                ordered: false,
                start: 1
            }
        );
        assert_eq!(blocks[0].children.len(), 2);
        assert_eq!(
            blocks[0].children[0].kind,
            BlockKind::ListItem { task: None }
        );
    }

    #[test]
    fn ordered_list_start() {
        let src = "3. a\n4. b\n";
        let blocks = parse(src);
        assert_eq!(
            blocks[0].kind,
            BlockKind::List {
                ordered: true,
                start: 3
            }
        );
        assert_eq!(blocks[0].children.len(), 2);
    }

    #[test]
    fn task_list_items() {
        let src = "- [x] done\n- [ ] todo\n";
        let blocks = parse(src);
        assert_eq!(
            blocks[0].children[0].kind,
            BlockKind::ListItem { task: Some(true) }
        );
        assert_eq!(
            blocks[0].children[1].kind,
            BlockKind::ListItem { task: Some(false) }
        );
        // 任务标记整段（"- [x] "）被标为 TaskMarker
        let item = &blocks[0].children[0];
        assert_eq!(item.spans[0].role, SpanRole::TaskMarker);
        assert_eq!(item.spans[0].range, 0..6);
    }

    #[test]
    fn multiple_blocks_with_blank_lines() {
        let src = "# Title\n\npara one\n\npara two\n";
        let blocks = parse(src);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].kind, BlockKind::Heading { level: 1 });
        assert_eq!(blocks[1].kind, BlockKind::Paragraph);
        assert_eq!(blocks[2].kind, BlockKind::Paragraph);
    }

    #[test]
    fn blank_source_yields_no_blocks() {
        assert!(parse("").is_empty());
        assert!(parse("\n\n  \n").is_empty());
    }

    #[test]
    fn visible_runs_hide_markers() {
        let src = "# 标题\n";
        let blocks = parse(src);
        let runs = visible_runs(&blocks[0].spans);
        assert_eq!(runs, vec![2..8]); // "# " 被隐藏
    }

    #[test]
    fn cjk_offsets_are_byte_based() {
        // "标题" = 各 3 字节，"# " 前缀 2 字节
        let src = "### 大标题\n";
        let blocks = parse(src);
        assert_eq!(blocks[0].kind, BlockKind::Heading { level: 3 });
        assert_eq!(blocks[0].spans[0].range, 0..4); // "### "
        assert_eq!(blocks[0].spans[1].range, 4..13); // "大标题" = 9 字节
    }
}
