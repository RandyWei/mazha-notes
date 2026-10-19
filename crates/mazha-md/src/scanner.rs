//! 行扫描块级解析器（SPEC §8，决策 D23）。
//!
//! 采用**行优先**扫描：先把源文本切成行，再逐行识别块的首缀（`#`、`>`、`-`、``` 等），
//! 所有区间都直接落在源字节偏移上——这正是 WYSIWYG 精确标记显隐与后续"脏块增量重解析"
//! 所需要的基础（`pulldown-cmark` 0.13 不暴露块级偏移，故自研）。
//!
//! MVP 支持的块：ATX/Setext 标题、段落、分割线、围栏/缩进代码块、引用、列表
//! （有序/无序/任务）。表格、脚注、HTML 块、惰性续行等留待后续迭代。

use crate::block::{Align, Block, BlockKind, CalloutKind};
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

/// 解析提示块标记 `[!TYPE]` + 可选自定义标题。
///
/// `rest` 是引用前缀剥离后的内容。返回 `(kind, title, marker_byte_len)`；
/// `marker_byte_len` 是 `[!TYPE]` 部分的字节长度（不含标题）。
fn parse_callout_marker(rest: &str) -> Option<(CalloutKind, Option<String>, usize)> {
    if !rest.starts_with("[!") {
        return None;
    }
    let close = rest.find(']')?;
    let name = &rest[2..close];
    if name.is_empty() {
        return None;
    }
    let after = &rest[close + 1..];
    let after_trimmed = after.trim_start();
    let title = if after_trimmed.is_empty() {
        None
    } else {
        Some(after_trimmed.trim_end().to_string())
    };
    Some((CalloutKind::parse(name), title, close + 1))
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

/// 显示公式开栏行的解析结果（SPEC D34）。
#[derive(Debug, Clone, Copy)]
enum MathFence {
    /// 单行 `$$…$$`：值为公式体字节数（收栏 `$$` 紧随其后，行尾其余文字另存为可见文本）。
    Single(usize),
    /// 多行开栏：`$$` 自成一行（或行尾另有文字），需继续扫描"仅含 `$$`"的收尾行。
    Multi,
}

/// 识别显示公式开栏行：缩进 ≤3 且行内容以 `$$` 起始（D34 ①）。
///
/// 单行/多行判定：跳过开栏 `$$` 后（下标从 2 起，避免与开栏自身重叠）
/// 仍能找到 `$$` 就是单行公式；否则按多行开栏，由 [`build_math_block`] 扫描收尾行。
/// 因此 `$$` 单独成行、`$$x` 这类行都会开启多行公式（未收尾时延伸到文档末尾）。
fn math_fence_open(rest: &str) -> Option<MathFence> {
    let ind = indent(rest);
    if ind > 3 {
        return None;
    }
    let after = &rest[ind..];
    if !after.starts_with("$$") {
        return None;
    }
    let close = after.as_bytes()[2..]
        .windows(2)
        .position(|w| w == b"$$")
        .map(|p| p + 2);
    Some(match close {
        Some(c) => MathFence::Single(c - 2),
        None => MathFence::Multi,
    })
}

/// 显示公式收尾行：缩进 ≤3 且去掉行尾空白后恰为 `$$`（与围栏收尾同规则）。
fn is_math_close(rest: &str) -> bool {
    let ind = indent(rest);
    ind <= 3 && rest[ind..].trim_end() == "$$"
}

/// 该行是否开启一个新的非段落块（用于段落终止判断）。
fn starts_new_block(source: &str, line: Line) -> bool {
    let rest = line.rest(source);
    atx_heading(rest).is_some()
        || fence_open(rest).is_some()
        || math_fence_open(rest).is_some()
        || is_thematic_break(rest)
        || quote_prefix(rest).is_some()
        || list_marker(rest).is_some()
}

/// 解析源文本为顶层块森林。
pub fn parse(source: &str) -> Vec<Block> {
    let lines = split_lines(source);
    let mut blocks = Vec::new();
    let mut start = 0usize;
    // front-matter 仅出现在文档最开头（第一行 `---` 且后有闭合 `---`/`...`）
    if let Some((block, next)) = build_front_matter(source, &lines) {
        blocks.push(block);
        start = next;
    }
    blocks.extend(scan_blocks_range(source, &lines, start));
    blocks
}

/// 从 `start` 行起解析顶层块森林。
fn scan_blocks_range(source: &str, lines: &[Line], start: usize) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut i = start;
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
        } else if let Some((block, next)) = build_table(source, lines, i) {
            // 表格：表头行 + 分隔行 + 连续数据行
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
        } else if let Some(fence) = math_fence_open(rest) {
            // 显示公式块 `$$…$$`（D34）：放在引用/列表之后，`> $$` 仍归引用
            let (block, next) = build_math_block(source, lines, i, fence);
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

/// 对一组（可能是前缀剥离后的）行做块级扫描。
fn scan_blocks(source: &str, lines: &[Line]) -> Vec<Block> {
    scan_blocks_range(source, lines, 0)
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
        // 代码内容从 `l.content` 起算（剥掉 `> `、列表缩进等容器前缀，
        // 否则前缀会串进可见文本、序列化时又被 prefix_lines 重复加一遍）
        spans.push(SyntaxSpan::text(l.content..l.end, SpanRole::CodeContent));
        last_end = l.end;
        i += 1;
    }
    if let Some(c) = closed {
        spans.push(SyntaxSpan::marker(c.content..c.end, SpanRole::FenceMarker));
    }
    let kind = BlockKind::CodeBlock { lang, fenced: true };
    (Block::leaf(kind, open_line.start..last_end, spans), i)
}

/// 构造显示公式块 `$$…$$`；返回块与下一个待处理行号（D34 ①）。
///
/// 片段约定：开栏/收栏 `$$` 记 `MathMarker`（隐藏），公式体逐行记 `Math`（可见），
/// **换行不进片段**——序列化按片段间隙里的换行回补（与代码块同一约定）。
/// 片段一律从 `line.content` 起算（剥掉 `> `、列表标记等容器前缀），
/// 这样引用/列表项里的公式既不会把前缀吞进可见文本，序列化时也不会重复加前缀。
/// 开栏/收栏行上多出来的文字（如 `$$a$$ 后缀`）另存为可见的普通文本，保证无损往返。
fn build_math_block(
    source: &str,
    lines: &[Line],
    open_idx: usize,
    fence: MathFence,
) -> (Block, usize) {
    let open_line = lines[open_idx];
    let ind = indent(open_line.rest(source));
    // 开栏标记只覆盖缩进 + `$$`；其后的行内文字交由下面的可见文本片段
    let body_start = open_line.content + ind + 2;
    let mut spans = vec![SyntaxSpan::marker(
        open_line.content..body_start,
        SpanRole::MathMarker,
    )];
    let mut last_end = open_line.end;
    let mut i = open_idx + 1;
    match fence {
        MathFence::Single(body) => {
            let content_end = body_start + body;
            if body_start < content_end {
                spans.push(SyntaxSpan::text(body_start..content_end, SpanRole::Math));
            }
            // 收栏 `$$`：只到公式体结束，行尾其余文字不隐藏
            spans.push(SyntaxSpan::marker(
                content_end..content_end + 2,
                SpanRole::MathMarker,
            ));
            if content_end + 2 < open_line.end {
                spans.push(SyntaxSpan::text(
                    content_end + 2..open_line.end,
                    SpanRole::Normal,
                ));
            }
        }
        MathFence::Multi => {
            if body_start < open_line.end {
                // 开栏行 `$$` 之后还有文字（如 `$$x` 或 `$$a$$ 后缀`）：保持可见
                spans.push(SyntaxSpan::text(
                    body_start..open_line.end,
                    SpanRole::Normal,
                ));
            }
            while i < lines.len() {
                let l = lines[i];
                if is_math_close(l.rest(source)) {
                    spans.push(SyntaxSpan::marker(l.content..l.end, SpanRole::MathMarker));
                    last_end = l.end;
                    i += 1;
                    break;
                }
                // 空行不产出片段（片段间隙里的多个换行足以还原空行）
                if l.content < l.end {
                    spans.push(SyntaxSpan::text(l.content..l.end, SpanRole::Math));
                }
                last_end = l.end;
                i += 1;
            }
        }
    }
    (
        Block::leaf(BlockKind::MathBlock, open_line.start..last_end, spans),
        i,
    )
}

/// 构造 GFM 表格；不是表格（或只有表头没有分隔行）时返回 `None`。///
/// 表格三要素（缺一不可）：
/// 1. 表头行：含未转义的 `|`；
/// 2. 分隔行：`|---|---|` / `| :-: |` 形式，列数决定列数与对齐；
/// 3. 数据行：后续**连续**的、含 `|` 且不开启其它块的行。
fn build_table(source: &str, lines: &[Line], start_idx: usize) -> Option<(Block, usize)> {
    let header = lines[start_idx];
    if !is_table_row(source, header) {
        return None;
    }
    // 分隔行必须在紧随的下一行
    let delim = *lines.get(start_idx + 1)?;
    let cells = delim_cells(source, delim)?;

    let mut spans: Vec<SyntaxSpan> = Vec::new();
    spans.extend(row_spans(source, header));
    spans.push(SyntaxSpan::marker(
        delim.content..delim.end,
        SpanRole::TableDelim,
    ));

    // 数据行：连续、含 `|`、且不开启其它块
    let mut i = start_idx + 2;
    let mut last_end = delim.end;
    while i < lines.len() {
        let l = lines[i];
        let rest = l.rest(source);
        if is_blank(rest) || starts_new_block(source, l) || !is_table_row(source, l) {
            break;
        }
        spans.extend(row_spans(source, l));
        last_end = l.end;
        i += 1;
    }

    let align = cells.into_iter().map(|c| c.align()).collect();
    Some((
        Block::leaf(BlockKind::Table { align }, header.start..last_end, spans),
        i,
    ))
}

/// 该行是否可作为表格行（含未转义的 `|`，且不是分隔行自身）。
fn is_table_row(source: &str, line: Line) -> bool {
    let rest = line.rest(source);
    rest.contains('|') && !is_table_delimiter(rest)
}

/// 表格分隔行？形如 `| --- | :-: | --: |`（至少一列，只含 `|-:` 与空白）。
fn is_table_delimiter(rest: &str) -> bool {
    let t = rest.trim();
    if t.is_empty() {
        return false;
    }
    let inner = t.strip_prefix('|').unwrap_or(t);
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    if inner.is_empty() {
        return false;
    }
    inner.split('|').all(|cell| {
        let c = cell.trim();
        !c.is_empty() && c.chars().all(|ch| ch == '-' || ch == ':') && c.contains('-') && {
            // 冒号只能在两端
            let body = c.trim_start_matches(':').trim_end_matches(':');
            !body.contains(':')
        }
    })
}

/// 解析分隔行列，得到列数与对齐。
fn delim_cells(source: &str, line: Line) -> Option<Vec<DelimCell>> {
    let rest = line.rest(source);
    if !is_table_delimiter(rest) {
        return None;
    }
    let inner = rest.trim();
    let inner = inner.strip_prefix('|').unwrap_or(inner);
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    let cells = inner
        .split('|')
        .map(|c| DelimCell(c.trim().to_string()))
        .collect();
    Some(cells)
}

/// 分隔行的一个单元格（原样保留，用 `align()` 取对齐）。
struct DelimCell(String);

impl DelimCell {
    /// 由 `:--` / `:-:` / `--:` 推出对齐。
    fn align(&self) -> Align {
        let left = self.0.starts_with(':');
        let right = self.0.ends_with(':');
        match (left, right) {
            (true, true) => Align::Center,
            (false, true) => Align::Right,
            _ => Align::Left,
        }
    }
}

/// 一行的表格片段：竖线（可见）+ 单元格内容（按行内语法解析）。
fn row_spans(source: &str, line: Line) -> Vec<SyntaxSpan> {
    let rest = line.rest(source);
    let base = line.content;
    let bytes = rest.as_bytes();
    let mut out = Vec::new();
    // 逐段切分：`|` 为可见分隔，段内按行内语法解析
    let mut seg_start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'|' && (i == 0 || bytes[i - 1] != b'\\') {
            if seg_start < i {
                // 去掉单元格两端空白（渲染时不显示）
                let (s, e) = trim_ascii_ws(bytes, seg_start, i);
                if s < e {
                    out.extend(crate::inline::parse_inline(source, base + s..base + e));
                }
            }
            out.push(SyntaxSpan::text(
                base + i..base + i + 1,
                SpanRole::TablePipe,
            ));
            i += 1;
            seg_start = i;
        } else {
            i += 1;
        }
    }
    if seg_start < bytes.len() {
        let (s, e) = trim_ascii_ws(bytes, seg_start, bytes.len());
        if s < e {
            out.extend(crate::inline::parse_inline(source, base + s..base + e));
        }
    }
    out
}

/// 收缩 `[start, end)` 两侧的 ASCII 空白，返回新的 (start, end)。
fn trim_ascii_ws(bytes: &[u8], mut start: usize, mut end: usize) -> (usize, usize) {
    while start < end && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    (start, end)
}

/// 构造引用块（仅支持带 `>` 前缀的续行，暂不支持惰性续行）。
fn build_block_quote(source: &str, lines: &[Line], start_idx: usize) -> (Block, usize) {
    let mut spans = Vec::new();
    let mut inner: Vec<Line> = Vec::new();
    let first_start = lines[start_idx].start;
    let mut last_end = lines[start_idx].end;
    let mut i = start_idx;
    let mut callout: Option<(CalloutKind, Option<String>)> = None;
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
        let content_start = l.content + consumed;
        // 仅在第一行检查提示块标记 `[!TYPE]`
        if i == start_idx && callout.is_none() {
            let content = &source[content_start..l.end];
            if let Some((kind, title, marker_len)) = parse_callout_marker(content) {
                callout = Some((kind, title));
                spans.push(SyntaxSpan::marker(
                    content_start..content_start + marker_len,
                    SpanRole::CalloutMarker,
                ));
                // 标题部分已单独存储，不作为内容行；第一行内容置空
                inner.push(Line {
                    start: l.start,
                    end: l.end,
                    content: l.end,
                });
                last_end = l.end;
                i += 1;
                continue;
            }
        }
        inner.push(Line {
            start: l.start,
            end: l.end,
            content: content_start,
        });
        last_end = l.end;
        i += 1;
    }
    let children = scan_blocks(source, &inner);
    let kind = match callout {
        Some((kind, title)) => BlockKind::Callout { kind, title },
        None => BlockKind::BlockQuote,
    };
    (
        Block::container(kind, first_start..last_end, spans, children),
        i,
    )
}

/// 构造 front-matter（文档开头 `---` YAML `---`）。
///
/// 仅当第一行恰为 `---`（无前导空格）且后续存在闭合行 `---`/`...` 时成立；
/// 否则返回 `None`（第一行按普通块处理）。
fn build_front_matter(source: &str, lines: &[Line]) -> Option<(Block, usize)> {
    let first = *lines.first()?;
    let first_rest = first.rest(source);
    // 第一行必须恰为 `---`（trim 后），且 content 在行首（无前导空格）
    if first.content != first.start || first_rest.trim_end() != "---" {
        return None;
    }
    // 找闭合行（`---` 或 `...`）
    let mut close_idx = None;
    for (j, l) in lines.iter().enumerate().skip(1) {
        let r = l.rest(source).trim_end();
        if r == "---" || r == "..." {
            close_idx = Some(j);
            break;
        }
    }
    let close_idx = close_idx?;
    // YAML 内容：第一行之后到闭合行之前
    let content_start = first.end + 1; // 跳过第一行的换行
    let content_end = lines[close_idx].start;
    let content = if content_start < content_end {
        source[content_start..content_end].to_string()
    } else {
        String::new()
    };
    let block = Block::leaf(
        BlockKind::FrontMatter { content },
        first.start..lines[close_idx].end,
        Vec::new(),
    );
    Some((block, close_idx + 1))
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
        if let Some(m) = list_marker(nrest)
            && m.indent <= marker.indent
        {
            break;
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

#[cfg(test)]
mod table_tests {
    use super::*;
    use crate::serialize::to_md;
    use crate::span::SpanKind;

    fn table(src: &str) -> Block {
        parse(src)
            .into_iter()
            .find(|b| matches!(b.kind, BlockKind::Table { .. }))
            .expect("应解析出表格")
    }

    #[test]
    fn simple_table() {
        let b = table("| a | b |\n| --- | --- |\n| 1 | 2 |\n");
        assert_eq!(b.range, 0..33);
        match b.kind {
            BlockKind::Table { align } => {
                assert_eq!(align, vec![Align::Left, Align::Left]);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn alignment_from_delimiter() {
        let b = table("| a | b | c |\n| :- | :-: | -: |\n| 1 | 2 | 3 |\n");
        match b.kind {
            BlockKind::Table { align } => {
                assert_eq!(align, vec![Align::Left, Align::Center, Align::Right]);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn no_delimiter_is_not_table() {
        // 只有表头、没有分隔行 → 维护段落
        let blocks = parse("| a | b |\n");
        assert!(
            blocks
                .iter()
                .all(|b| !matches!(b.kind, BlockKind::Table { .. }))
        );
        assert!(matches!(blocks[0].kind, BlockKind::Paragraph));
    }

    #[test]
    fn cells_parse_inline_marks() {
        let b = table("| a | b |\n| - | - |\n| **粗** | *斜* |\n");
        let has_strong = b
            .spans
            .iter()
            .any(|s| s.role == SpanRole::Strong && s.kind == SpanKind::Text);
        let has_em = b
            .spans
            .iter()
            .any(|s| s.role == SpanRole::Emphasis && s.kind == SpanKind::Text);
        assert!(has_strong, "单元格内粗体应被行内解析");
        assert!(has_em, "单元格内斜体应被行内解析");
    }

    #[test]
    fn delimiter_row_is_hidden() {
        let b = table("| a |\n| --- |\n| 1 |\n");
        // 分隔行是隐藏标记
        assert!(b.spans.iter().any(|s| s.role == SpanRole::TableDelim));
        // 竖线是可见文本
        assert!(
            b.spans
                .iter()
                .any(|s| s.role == SpanRole::TablePipe && s.kind == SpanKind::Text)
        );
    }

    #[test]
    fn table_ends_at_blank_or_other_block() {
        let b = table("| a |\n| - |\n| 1 |\n\n后续段落\n");
        assert_eq!(b.range, 0..17); // 到数据行末，不含空行与后续段落
    }

    #[test]
    fn table_roundtrip() {
        for src in [
            "| a | b |\n| --- | --- |\n| 1 | 2 |\n",
            "| a | b | c |\n| --- | :-: | --: |\n| 1 | 2 | 3 |\n",
            "| a |\n| --- |\n",
        ] {
            let out = to_md(src);
            assert_eq!(out, src, "规范化表格应逐字节往返");
        }
    }

    #[test]
    fn escaped_pipe_in_cell() {
        // 单元格内转义竖线不算分隔
        let b = table("| a | b |\n| - | - |\n| x \\| y | z |\n");
        assert!(matches!(b.kind, BlockKind::Table { .. }));
    }
}

#[cfg(test)]
mod callout_tests {
    use super::*;
    use crate::serialize::to_md;
    use crate::span::SpanKind;

    fn block(src: &str) -> Block {
        parse(src).into_iter().next().expect("至少一个块")
    }

    #[test]
    fn callout_parse_note() {
        let b = block("> [!NOTE]\n> 内容\n");
        assert!(matches!(
            b.kind,
            BlockKind::Callout {
                kind: CalloutKind::Note,
                ..
            }
        ));
        assert_eq!(b.children.len(), 1);
        assert_eq!(b.children[0].kind, BlockKind::Paragraph);
    }

    #[test]
    fn callout_parse_custom_title() {
        let b = block("> [!TIP] 自定义标题\n> 内容\n");
        assert!(matches!(
            b.kind,
            BlockKind::Callout {
                kind: CalloutKind::Tip,
                ..
            }
        ));
        // title 是 Option<String>
        if let BlockKind::Callout { title, .. } = &b.kind {
            assert_eq!(title.as_deref(), Some("自定义标题"));
        } else {
            panic!("应为 Callout");
        }
    }

    #[test]
    fn callout_parse_case_insensitive() {
        let b = block("> [!warning]\n> 内容\n");
        assert!(matches!(
            b.kind,
            BlockKind::Callout {
                kind: CalloutKind::Warning,
                ..
            }
        ));
    }

    #[test]
    fn callout_parse_custom_type() {
        let b = block("> [!BUG]\n> 内容\n");
        assert!(matches!(
            b.kind,
            BlockKind::Callout {
                kind: CalloutKind::Custom(ref s),
                ..
            } if s == "BUG"
        ));
    }

    #[test]
    fn callout_marker_span_hidden() {
        let src = "> [!NOTE]\n> 内容\n";
        let b = parse(src).into_iter().next().expect("至少一个块");
        let marker = b
            .spans
            .iter()
            .find(|s| s.role == SpanRole::CalloutMarker)
            .expect("应有 CalloutMarker 片段");
        assert_eq!(marker.kind, SpanKind::Marker);
        assert_eq!(&src[marker.range.clone()], "[!NOTE]");
    }

    #[test]
    fn callout_roundtrip() {
        let src = "> [!NOTE]\n> 内容\n";
        let md = to_md(src);
        assert_eq!(md, src);
    }

    #[test]
    fn callout_roundtrip_custom_title() {
        let src = "> [!TIP] 自定义\n> 内容\n";
        let md = to_md(src);
        assert_eq!(md, src);
    }

    #[test]
    fn quote_without_marker_stays_blockquote() {
        let b = block("> 普通引用\n");
        assert_eq!(b.kind, BlockKind::BlockQuote);
    }

    #[test]
    fn callout_empty_name_rejected() {
        // `[!]` 空名不算 callout，仍是普通引用
        let b = block("> [!]\n> 内容\n");
        assert_eq!(b.kind, BlockKind::BlockQuote);
    }
}

#[cfg(test)]
mod frontmatter_tests {
    use super::*;
    use crate::serialize::to_md;

    #[test]
    fn frontmatter_parse() {
        let src = "---\ntitle: 测试\n---\n正文\n";
        let blocks = parse(src);
        assert_eq!(blocks.len(), 2);
        assert!(matches!(
            &blocks[0].kind,
            BlockKind::FrontMatter { content } if content == "title: 测试\n"
        ));
        assert_eq!(blocks[1].kind, BlockKind::Paragraph);
    }

    #[test]
    fn frontmatter_empty() {
        let src = "---\n---\n正文\n";
        let blocks = parse(src);
        assert!(matches!(
            &blocks[0].kind,
            BlockKind::FrontMatter { content } if content.is_empty()
        ));
    }

    #[test]
    fn frontmatter_dot_close() {
        let src = "---\ntitle: x\n...\n正文\n";
        let blocks = parse(src);
        assert!(matches!(blocks[0].kind, BlockKind::FrontMatter { .. }));
    }

    #[test]
    fn frontmatter_not_at_start_is_thematic_break() {
        // 非文档开头的 `---` 不是 front-matter
        let src = "正文\n---\n更多\n";
        let blocks = parse(src);
        assert!(
            blocks
                .iter()
                .all(|b| !matches!(b.kind, BlockKind::FrontMatter { .. }))
        );
    }

    #[test]
    fn frontmatter_no_close_is_thematic_break() {
        // 开头 `---` 但无闭合：按分割线处理
        let src = "---\ntitle: x\n正文\n";
        let blocks = parse(src);
        assert!(
            blocks
                .iter()
                .all(|b| !matches!(b.kind, BlockKind::FrontMatter { .. }))
        );
    }

    #[test]
    fn frontmatter_indented_not_recognized() {
        // 前导空格的 `---` 不算 front-matter
        let src = " ---\ntitle: x\n---\n正文\n";
        let blocks = parse(src);
        assert!(
            blocks
                .iter()
                .all(|b| !matches!(b.kind, BlockKind::FrontMatter { .. }))
        );
    }

    #[test]
    fn frontmatter_roundtrip() {
        let src = "---\ntitle: 测试\n---\n正文\n";
        let out = to_md(src);
        assert_eq!(out, src);
    }

    #[test]
    fn frontmatter_roundtrip_empty() {
        let src = "---\n---\n正文\n";
        let out = to_md(src);
        assert_eq!(out, src);
    }
}

#[cfg(test)]
mod nested_prefix_tests {
    use super::*;
    use crate::serialize::to_md;

    /// 取某角色片段的源文本切片（用于断言前缀是否串进片段）。
    fn role_texts<'a>(src: &'a str, b: &Block, role: SpanRole) -> Vec<&'a str> {
        b.spans
            .iter()
            .filter(|s| s.role == role)
            .map(|s| &src[s.range.clone()])
            .collect()
    }

    #[test]
    fn quote_code_content_excludes_quote_prefix() {
        // 片段一律从 `line.content` 起算，否则 `> ` 会串进代码文本
        let src = "> ```\n> x\n> ```\n";
        let code = &parse(src)[0].children[0];
        assert_eq!(role_texts(src, code, SpanRole::CodeContent), vec!["x"]);
        assert_eq!(
            role_texts(src, code, SpanRole::FenceMarker),
            vec!["```", "```"]
        );
    }

    #[test]
    fn list_code_content_excludes_item_indent() {
        let src = "- ```\n  x\n  ```\n";
        let item = &parse(src)[0].children[0];
        let code = &item.children[0];
        assert_eq!(role_texts(src, code, SpanRole::CodeContent), vec!["x"]);
        assert_eq!(
            role_texts(src, code, SpanRole::FenceMarker),
            vec!["```", "```"]
        );
    }

    #[test]
    fn nested_code_roundtrip() {
        for src in [
            "> ```\n> x\n> ```\n",
            "- ```\n  x\n  ```\n",
            "- para\n  ```\n  x\n  ```\n",
        ] {
            assert_eq!(to_md(src), src, "roundtrip {src:?}");
        }
    }
}

#[cfg(test)]
mod math_tests {
    use super::*;
    use crate::serialize::to_md;
    use crate::span::{SpanKind, visible_runs};

    /// 取块内片段的 `(kind, role, 源文本)`，逐条断言片段契约。
    fn parts<'a>(src: &'a str, b: &Block) -> Vec<(SpanKind, SpanRole, &'a str)> {
        b.spans
            .iter()
            .map(|s| (s.kind, s.role, &src[s.range.clone()]))
            .collect()
    }

    #[test]
    fn single_line_math_block() {
        let src = "$$E=mc^2$$\n";
        let blocks = parse(src);
        assert_eq!(blocks.len(), 1, "{blocks:#?}");
        assert_eq!(blocks[0].kind, BlockKind::MathBlock);
        assert_eq!(&src[blocks[0].range.clone()], "$$E=mc^2$$");
        assert_eq!(
            parts(src, &blocks[0]),
            vec![
                (SpanKind::Marker, SpanRole::MathMarker, "$$"),
                (SpanKind::Text, SpanRole::Math, "E=mc^2"),
                (SpanKind::Marker, SpanRole::MathMarker, "$$"),
            ]
        );
        // 定界符隐藏，公式体可见
        assert_eq!(visible_runs(&blocks[0].spans), vec![2..8]);
    }

    #[test]
    fn multiline_math_block() {
        let src = "$$\na + b\n$$\ntail\n";
        let blocks = parse(src);
        assert_eq!(blocks.len(), 2, "{blocks:#?}");
        assert_eq!(blocks[0].kind, BlockKind::MathBlock);
        assert_eq!(&src[blocks[0].range.clone()], "$$\na + b\n$$");
        assert_eq!(
            parts(src, &blocks[0]),
            vec![
                (SpanKind::Marker, SpanRole::MathMarker, "$$"),
                (SpanKind::Text, SpanRole::Math, "a + b"),
                (SpanKind::Marker, SpanRole::MathMarker, "$$"),
            ]
        );
        assert_eq!(blocks[1].kind, BlockKind::Paragraph);
        assert_eq!(
            visible_runs(&blocks[0].spans),
            vec![3..8],
            "可见文本只有公式体"
        );
    }

    #[test]
    fn unclosed_math_block_runs_to_end() {
        // 未闭合：延伸到文档末尾（与未闭合围栏同规则）
        let src = "$$\nx + y\n";
        let blocks = parse(src);
        assert_eq!(blocks.len(), 1, "{blocks:#?}");
        assert_eq!(blocks[0].kind, BlockKind::MathBlock);
        assert_eq!(&src[blocks[0].range.clone()], "$$\nx + y");
        assert_eq!(
            parts(src, &blocks[0]),
            vec![
                (SpanKind::Marker, SpanRole::MathMarker, "$$"),
                (SpanKind::Text, SpanRole::Math, "x + y"),
            ]
        );
    }

    #[test]
    fn lone_double_dollar_opens_multiline() {
        // `$$` 单独成行 → 多行开栏（同行为下也未闭合）
        let src = "$$\n";
        let blocks = parse(src);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, BlockKind::MathBlock);
        assert_eq!(
            parts(src, &blocks[0]),
            vec![(SpanKind::Marker, SpanRole::MathMarker, "$$")]
        );
    }

    #[test]
    fn math_block_blank_line_inside() {
        let src = "$$\na\n\nb\n$$\n";
        let blocks = parse(src);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, BlockKind::MathBlock);
        // 空行不产出片段，但可见文本保留两段
        assert_eq!(
            parts(src, &blocks[0])
                .into_iter()
                .filter(|p| p.1 == SpanRole::Math)
                .map(|p| p.2)
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
    }

    #[test]
    fn math_block_indent_limit() {
        // 缩进 ≤3 仍是公式块
        let indented = parse("   $$\n   x\n   $$\n");
        assert_eq!(indented[0].kind, BlockKind::MathBlock);
        assert_eq!(
            parts("   $$\n   x\n   $$\n", &indented[0])[1],
            (SpanKind::Text, SpanRole::Math, "   x")
        );
        // 4 空格缩进 → 普通段落（不是公式块）
        let deep = parse("    $$\n    x\n");
        assert!(deep.iter().all(|b| b.kind != BlockKind::MathBlock));
    }

    #[test]
    fn math_block_interrupts_paragraph() {
        let src = "前文\n$$\nx\n$$\n";
        let blocks = parse(src);
        assert_eq!(blocks.len(), 2, "{blocks:#?}");
        assert_eq!(blocks[0].kind, BlockKind::Paragraph);
        assert_eq!(blocks[1].kind, BlockKind::MathBlock);
    }

    #[test]
    fn single_line_with_trailing_text_stays_visible() {
        // 行尾还有文字：公式仍是单行块，其余文字保持可见（无损往返）
        let src = "$$a$$ 后缀\n";
        let blocks = parse(src);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, BlockKind::MathBlock);
        assert_eq!(
            parts(src, &blocks[0]),
            vec![
                (SpanKind::Marker, SpanRole::MathMarker, "$$"),
                (SpanKind::Text, SpanRole::Math, "a"),
                (SpanKind::Marker, SpanRole::MathMarker, "$$"),
                (SpanKind::Text, SpanRole::Normal, " 后缀"),
            ]
        );
        assert_eq!(to_md(src), src);
    }

    #[test]
    fn math_block_in_quote_strips_prefix() {
        let src = "> $$\n> x\n> $$\n";
        let blocks = parse(src);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, BlockKind::BlockQuote);
        let math = &blocks[0].children[0];
        assert_eq!(math.kind, BlockKind::MathBlock);
        assert_eq!(
            parts(src, math),
            vec![
                (SpanKind::Marker, SpanRole::MathMarker, "$$"),
                (SpanKind::Text, SpanRole::Math, "x"),
                (SpanKind::Marker, SpanRole::MathMarker, "$$"),
            ],
            "片段不得把 `> ` 前缀算进去"
        );
    }

    #[test]
    fn math_block_in_list_strips_marker() {
        let src = "- $$\n  x\n  $$\n";
        let blocks = parse(src);
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            blocks[0].kind,
            BlockKind::List {
                ordered: false,
                start: 1
            }
        );
        let item = &blocks[0].children[0];
        let math = &item.children[0];
        assert_eq!(math.kind, BlockKind::MathBlock);
        assert_eq!(
            parts(src, math),
            vec![
                (SpanKind::Marker, SpanRole::MathMarker, "$$"),
                (SpanKind::Text, SpanRole::Math, "x"),
                (SpanKind::Marker, SpanRole::MathMarker, "$$"),
            ],
            "片段不得把列表标记/缩进算进去"
        );
    }

    #[test]
    fn math_block_roundtrip() {
        for src in [
            "$$E=mc^2$$\n",
            "$$\na + b\n$$\n",
            "$$\nx\n",       // 未闭合
            "$$\n\ny\n$$\n", // 公式体中的空行
            "   $$\n   x\n   $$\n",
            "$$a$$ 后缀\n",
            "> $$\n> x\n> $$\n",
            "- $$\n  x\n  $$\n",
            "前文\n\n$$\na\n$$\n\n后文\n",
        ] {
            assert_eq!(to_md(src), src, "roundtrip {src:?}");
            // 幂等：再解析再序列化仍一致
            assert_eq!(to_md(&to_md(src)), to_md(src), "idempotent {src:?}");
        }
    }
}
