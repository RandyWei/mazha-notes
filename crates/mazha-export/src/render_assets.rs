//! 导出侧「渲染资产」桥接层（D84）。
//!
//! 桌面已具备 RaTeX（公式）与 mermaid-rs（图表）的真实渲染能力，但两者都带 GUI 依赖，
//! 不能进本 crate。本模块只做**无 GUI、可单测**的纯文本工作：
//! 1. [`collect_render_requests`] 从 Markdown 源文本收集需要渲染的公式/图表（键为源码文本）；
//! 2. 桌面按键渲染出自包含 SVG 填入 [`RenderAssets`]；
//! 3. [`prepare_renderables`] 在**独立字符串副本**上把这些源码区间替换成图片 Widget，
//!    后续 HTML/EPUB/PDF 各自既有的图片输出通道即可呈现渲染结果。
//!
//! 关键约束：不触碰编辑器 `Document`/`OffsetMap`/光标/选区，替换只作用于导出副本；
//! 找不到资产时保留原字面（降级为 D34 之前的观感，而不是让导出失败）。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use mazha_md::{Block, BlockKind, SpanKind, SpanRole, SyntaxSpan, parse};
use std::cmp::Reverse;
use std::collections::HashMap;
use std::ops::Range;

/// 需要渲染的内容种类（决定查 [`RenderAssets`] 的哪张表、替代文本的措辞）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderKind {
    /// 公式：块级 `$$…$$` 或行内 `$…$`，键为 TeX 正文。
    Math,
    /// 图表：`lang=mermaid` 围栏块，键为 DSL 正文。
    Chart,
}

/// 一处待渲染请求：源码区间 + 归一化键（桌面按键调渲染器）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderRequest {
    /// 内容种类。
    pub kind: RenderKind,
    /// 渲染器输入：公式 TeX 正文 / 图表 DSL 正文。
    pub key: String,
    /// 该内容在源文本中的字节区间（块级公式/图表覆盖整个块，含定界围栏）。
    pub range: Range<usize>,
}

/// 桌面渲染好的 SVG 资产集合（键与 [`RenderRequest::key`] 完全一致）。
///
/// 值为自包含 SVG 文本：公式来自 `ratex_gpui::render::render_latex_to_svg`
/// （内嵌字形轮廓），图表来自 `mermaid_rs_renderer::render_with_options`。
#[derive(Debug, Clone, Default)]
pub struct RenderAssets {
    /// TeX 正文 → SVG 文本。
    math: HashMap<String, String>,
    /// mermaid DSL 正文 → SVG 文本。
    charts: HashMap<String, String>,
}

impl RenderAssets {
    /// 创建空资产集合。
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一份公式 SVG；空白 SVG 视为渲染失败，直接忽略。
    pub fn insert_math(&mut self, key: impl Into<String>, svg: impl Into<String>) {
        insert_svg(&mut self.math, key, svg);
    }

    /// 登记一份图表 SVG；空白 SVG 视为渲染失败，直接忽略。
    pub fn insert_chart(&mut self, key: impl Into<String>, svg: impl Into<String>) {
        insert_svg(&mut self.charts, key, svg);
    }

    /// 按 TeX 正文查 SVG。
    pub fn math_svg(&self, key: &str) -> Option<&str> {
        self.math.get(key).map(String::as_str)
    }

    /// 按 mermaid DSL 正文查 SVG。
    pub fn chart_svg(&self, key: &str) -> Option<&str> {
        self.charts.get(key).map(String::as_str)
    }

    /// 是否没有任何可用资产。
    pub fn is_empty(&self) -> bool {
        self.math.is_empty() && self.charts.is_empty()
    }

    /// 资产条目总数（两张表之和）。
    pub fn len(&self) -> usize {
        self.math.len() + self.charts.len()
    }
}

/// 写入一张资产表；空白值不写入，避免后续图片 Widget 指向空 SVG 而编译/渲染失败。
fn insert_svg(table: &mut HashMap<String, String>, key: impl Into<String>, svg: impl Into<String>) {
    let svg = svg.into();
    if svg.trim().is_empty() {
        return;
    }
    table.insert(key.into(), svg);
}

/// 渲染资产的落地方式：HTML/EPUB 内嵌 data URI，PDF 走 Typst 虚拟资源文件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetTarget {
    /// `data:image/svg+xml;base64,…`，复用 HTML/EPUB 既有 `<img>` 输出。
    DataUri,
    /// `__mz_asset_N.svg`，由 PDF 的 `ExportWorld` 从内存资源表提供字节。
    VirtualFile,
}

/// 预处理结果：`source` 为替换后的导出副本，`files` 为虚拟资源表（仅 [`AssetTarget::VirtualFile`] 非空）。
#[derive(Debug, Clone, Default)]
pub struct PreparedMarkdown {
    /// 公式/图表已替换成图片 Widget 的 Markdown 副本。
    pub source: String,
    /// 虚拟文件名 → SVG 文本（供 Typst `World` 按路径供字节）。
    pub files: HashMap<String, String>,
}

/// 汇总公式块的可见 TeX 正文：跳过 `$$` 定界标记，仅当片段之间的源文本含换行时补 `\n`。
///
/// 与 `typst.rs`/`html.rs` 的 `math_body` 同规则；键必须与桌面渲染器输入一致，
/// 否则 `RenderAssets` 查不到而降级为字面（不报错，但观感回退）。
fn math_block_body(source: &str, block: &Block) -> String {
    let mut body = String::new();
    let mut prev_end: Option<usize> = None;
    for sp in &block.spans {
        if sp.kind != SpanKind::Text {
            continue;
        }
        if let Some(prev) = prev_end {
            for c in source[prev..sp.range.start].chars() {
                if c == '\n' {
                    body.push('\n');
                }
            }
        }
        body.push_str(&source[sp.range.clone()]);
        prev_end = Some(sp.range.end);
    }
    body
}

/// 汇总 mermaid 围栏块的 DSL 正文：逐行去空白、忽略空行，按行拼接。
///
/// 与桌面 `chart_source_body` 同规则（D77）：保留行结构但剥掉容器前缀与缩进，
/// 使 `mermaid_rs_renderer` 的解析输入与编辑器预览一致。
fn chart_block_body(source: &str, block: &Block) -> String {
    let mut lines: Vec<String> = Vec::new();
    for sp in &block.spans {
        if sp.role != SpanRole::CodeContent {
            continue;
        }
        let text = source[sp.range.clone()].trim();
        if text.is_empty() {
            continue;
        }
        // CodeContent 片段按行切分且不含行尾换行，片段内的换行再按行拆开。
        for line in text.lines() {
            let line = line.trim();
            if !line.is_empty() {
                lines.push(line.to_string());
            }
        }
    }
    lines.join("\n")
}

/// 纯函数：计算块内某个行内公式 `Math` 片段的**完整替换区间**。
///
/// `mazha-md` 把 `$x^2$` 解析为三个首尾相接的片段：开 `$`（`MathMarker`）、
/// 公式体（`Math`）、闭 `$`（`MathMarker`）。D84 只替换公式体会残留 `$…$`，
/// 导出器二次解析该副本时把 `![公式](dest)` 当公式字面输出（用户实机反馈，D85），
/// 因此这里把区间扩到覆盖整对定界符，替换后不再有 `$` 包裹图片语法。
///
/// 边界：只合并与该 `Math` 片段**首尾相接**且长度恰为 1 的前后 `MathMarker`，
/// 既避免误伤其它 marker，也避免把相邻公式的定界符跨公式合并进来；
/// 不满足条件时退回公式体原区间（保留既有降级行为）。
fn inline_math_range(spans: &[SyntaxSpan], index: usize) -> Range<usize> {
    let math = &spans[index];
    let mut start = math.range.start;
    let mut end = math.range.end;
    // 前一定界符：必须紧接公式体起点、角色为 MathMarker 且恰占一个字节（单个 `$`）。
    if let Some(prev) = index.checked_sub(1).and_then(|i| spans.get(i))
        && prev.role == SpanRole::MathMarker
        && prev.range.end == start
        && prev.range.end - prev.range.start == 1
    {
        start = prev.range.start;
    }
    // 后一定界符：必须紧接公式体终点、角色为 MathMarker 且恰占一个字节（单个 `$`）。
    if let Some(next) = spans.get(index + 1)
        && next.role == SpanRole::MathMarker
        && next.range.start == end
        && next.range.end - next.range.start == 1
    {
        end = next.range.end;
    }
    start..end
}

/// 递归收集块及其子块中的渲染请求。
fn collect_from_blocks(source: &str, blocks: &[Block], out: &mut Vec<RenderRequest>) {
    for block in blocks {
        match &block.kind {
            // 块级公式/图表整体作为一个请求：覆盖整个块区间（含 `$$`/围栏），
            // 这样替换后不会残留定界符而被二次解析成公式字面。
            BlockKind::MathBlock => push_request(
                out,
                RenderKind::Math,
                block.range.clone(),
                math_block_body(source, block),
            ),
            BlockKind::CodeBlock { lang, .. }
                if lang
                    .as_deref()
                    .is_some_and(|l| l.eq_ignore_ascii_case("mermaid")) =>
            {
                push_request(
                    out,
                    RenderKind::Chart,
                    block.range.clone(),
                    chart_block_body(source, block),
                );
            }
            _ => {
                // 行内公式：替换区间覆盖整对 `$…$`（含两端 MathMarker，D85），
                // 键仍取纯公式体，保证与桌面 RaTeX 渲染键一致（不能把 `$` 塞进 key）。
                for (i, sp) in block.spans.iter().enumerate() {
                    if sp.kind == SpanKind::Text && sp.role == SpanRole::Math {
                        push_request(
                            out,
                            RenderKind::Math,
                            inline_math_range(&block.spans, i),
                            source[sp.range.clone()].to_string(),
                        );
                    }
                }
                collect_from_blocks(source, &block.children, out);
            }
        }
    }
}

/// 追加一条非空请求：空键（如空公式块）无从渲染，跳过。
fn push_request(out: &mut Vec<RenderRequest>, kind: RenderKind, range: Range<usize>, key: String) {
    if key.trim().is_empty() || range.is_empty() {
        return;
    }
    out.push(RenderRequest { kind, key, range });
}

/// 纯函数：收集 Markdown 源文本中所有需要渲染的公式/图表。
///
/// 结果按源偏移升序，区间与键可直接交给桌面渲染器；不存在重复区间。
pub fn collect_render_requests(source: &str) -> Vec<RenderRequest> {
    let blocks = parse(source);
    let mut out = Vec::new();
    collect_from_blocks(source, &blocks, &mut out);
    out.sort_by_key(|r| r.range.start);
    out
}

/// 纯函数：用桌面渲染好的 SVG 在**导出副本**中替换公式/图表源码。
///
/// - 有资产：替换为 `![公式](dest)` / `![图表](dest)`，交给既有图片输出通道；
/// - 无资产：原样保留该区间（降级为字面，见 D34）；
/// - 替换按源偏移**倒序**进行，避免先改前面的内容导致后面的区间失效。
///
/// `target=VirtualFile` 时同时给出 `files` 表，供 PDF `ExportWorld` 按虚拟路径供字节。
pub fn prepare_renderables(
    source: &str,
    assets: &RenderAssets,
    target: AssetTarget,
) -> PreparedMarkdown {
    if assets.is_empty() {
        return PreparedMarkdown {
            source: source.to_string(),
            files: HashMap::new(),
        };
    }
    let requests = collect_render_requests(source);
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    let mut files = HashMap::new();
    for (index, request) in requests.iter().enumerate() {
        // 防御越界区间（源文本与解析结果不一致时跳过，绝不 panic）。
        if source.get(request.range.clone()).is_none() {
            continue;
        }
        let svg = match request.kind {
            RenderKind::Math => assets.math_svg(&request.key),
            RenderKind::Chart => assets.chart_svg(&request.key),
        };
        let Some(svg) = svg else { continue };
        let dest = match target {
            AssetTarget::DataUri => format!(
                "data:image/svg+xml;base64,{}",
                BASE64.encode(svg.as_bytes())
            ),
            AssetTarget::VirtualFile => {
                let name = format!("__mz_asset_{index}.svg");
                files.insert(name.clone(), svg.to_string());
                name
            }
        };
        let alt = match request.kind {
            RenderKind::Math => "公式",
            RenderKind::Chart => "图表",
        };
        edits.push((request.range.clone(), format!("![{alt}]({dest})")));
    }
    if edits.is_empty() {
        return PreparedMarkdown {
            source: source.to_string(),
            files,
        };
    }
    edits.sort_by_key(|(range, _)| Reverse(range.start));
    let mut out = source.to_string();
    for (range, replacement) in edits {
        out.replace_range(range, &replacement);
    }
    PreparedMarkdown { source: out, files }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SVG: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"8\" height=\"8\"><rect width=\"8\" height=\"8\"/></svg>";

    #[test]
    fn collects_block_inline_math_and_mermaid_chart() {
        let src =
            "行内 $x^2$ 与块级：\n\n$$\n\\frac{a}{b}\n$$\n\n```mermaid\ngraph LR\n A-->B\n```\n";
        let requests = collect_render_requests(src);
        assert_eq!(requests.len(), 3, "{requests:?}");
        assert_eq!(requests[0].kind, RenderKind::Math);
        assert_eq!(requests[0].key, "x^2");
        assert_eq!(requests[1].kind, RenderKind::Math);
        assert!(
            requests[1].key.contains("\\frac{a}{b}"),
            "{:?}",
            requests[1]
        );
        assert_eq!(requests[2].kind, RenderKind::Chart);
        assert_eq!(requests[2].key, "graph LR\nA-->B");
        // 行内请求也覆盖整对 `$…$`（D85），与块级公式/图表一致不残留定界符。
        assert_eq!(&src[requests[0].range.clone()], "$x^2$");
        // 块级请求覆盖 `$$`/围栏，替换后不会残留定界符。
        assert!(src[requests[1].range.clone()].contains("$$"));
        assert!(src[requests[2].range.clone()].contains("```mermaid"));
    }

    #[test]
    fn ignores_non_mermaid_code_and_math_in_code() {
        let src = "```rust\nlet x = $not_math$;\n// graph LR; A-->B\n```\n\n```mermaid\ngraph TD; A-->B\n```\n";
        let requests = collect_render_requests(src);
        assert_eq!(requests.len(), 1, "{requests:?}");
        assert_eq!(requests[0].kind, RenderKind::Chart);
    }

    #[test]
    fn data_uri_replaces_all_renderables() {
        // 单行 `$$b$$` 必须独占一行才是块级公式（与编辑器 D34 解析规则一致）；
        // 行中间的 `$$x$$` 按普通文本处理，不应被导出侧误渲染。
        let src = "行内 $x$ 与块级：\n\n$$b$$\n\n```mermaid\ngraph LR; A-->B\n```\n";
        let mut assets = RenderAssets::new();
        assets.insert_math("x", SVG);
        assets.insert_math("b", SVG);
        assets.insert_chart("graph LR; A-->B", SVG);
        let prepared = prepare_renderables(src, &assets, AssetTarget::DataUri);
        assert!(prepared.files.is_empty());
        assert_eq!(
            prepared
                .source
                .matches("data:image/svg+xml;base64,")
                .count(),
            3,
            "{}",
            prepared.source
        );
        assert!(!prepared.source.contains("$x$"), "{}", prepared.source);
        assert!(!prepared.source.contains("$$b$$"), "{}", prepared.source);
        assert!(!prepared.source.contains("graph LR"), "{}", prepared.source);
        // base64 载荷解码后就是注入的 SVG。
        let encoded = prepared
            .source
            .split("data:image/svg+xml;base64,")
            .nth(1)
            .expect("应有 data URI")
            .split(')')
            .next()
            .expect("应有载荷结尾");
        assert_eq!(BASE64.decode(encoded).expect("合法 base64"), SVG.as_bytes());
    }

    #[test]
    fn virtual_file_builds_asset_table() {
        let src = "$$\nx^2\n$$\n\n```mermaid\ngraph LR; A-->B\n```\n";
        let mut assets = RenderAssets::new();
        assets.insert_math("x^2", SVG);
        assets.insert_chart("graph LR; A-->B", SVG);
        let prepared = prepare_renderables(src, &assets, AssetTarget::VirtualFile);
        assert_eq!(prepared.files.len(), 2, "{:?}", prepared.files);
        assert!(
            prepared.source.contains("![公式](__mz_asset_0.svg)"),
            "{}",
            prepared.source
        );
        assert!(
            prepared.source.contains("![图表](__mz_asset_1.svg)"),
            "{}",
            prepared.source
        );
        assert_eq!(
            prepared.files.get("__mz_asset_0.svg").map(String::as_str),
            Some(SVG)
        );
    }

    #[test]
    fn missing_asset_keeps_original_literal() {
        let src = "行内 $x$ 与块级：\n\n$$b$$\n";
        let mut assets = RenderAssets::new();
        // 只覆盖行内键：块级公式必须原样保留（降级为字面）。
        assets.insert_math("x", SVG);
        let prepared = prepare_renderables(src, &assets, AssetTarget::DataUri);
        assert_eq!(
            prepared
                .source
                .matches("data:image/svg+xml;base64,")
                .count(),
            1
        );
        assert!(prepared.source.contains("$$b$$"), "{}", prepared.source);
    }

    #[test]
    fn empty_svg_is_ignored() {
        let mut assets = RenderAssets::new();
        assets.insert_math("x", "   ");
        assert!(assets.is_empty());
        assets.insert_chart("g", SVG);
        assert_eq!(assets.len(), 1);
        assert_eq!(assets.chart_svg("g"), Some(SVG));
    }

    #[test]
    fn no_requests_is_identity() {
        let src = "# 标题\n\n正文\n";
        let mut assets = RenderAssets::new();
        assets.insert_math("x", SVG);
        let prepared = prepare_renderables(src, &assets, AssetTarget::VirtualFile);
        assert_eq!(prepared.source, src);
        assert!(prepared.files.is_empty());
    }

    /// 递归收集解析结果里的全部片段，便于断言行内公式是否变成图片 Widget。
    fn all_spans(blocks: &[Block]) -> Vec<SyntaxSpan> {
        let mut out = Vec::new();
        for block in blocks {
            out.extend(block.spans.iter().cloned());
            out.extend(all_spans(&block.children));
        }
        out
    }

    #[test]
    fn inline_math_range_merges_only_adjacent_single_dollar_markers() {
        // 标准三片段 `$x$`：两侧定界符都应并入区间。
        let spans = vec![
            SyntaxSpan::marker(0..1, SpanRole::MathMarker),
            SyntaxSpan::text(1..2, SpanRole::Math),
            SyntaxSpan::marker(2..3, SpanRole::MathMarker),
        ];
        assert_eq!(inline_math_range(&spans, 1), 0..3);
        // 缺闭定界符：只扩左侧，不凭空造右侧区间。
        let missing_close = vec![
            SyntaxSpan::marker(0..1, SpanRole::MathMarker),
            SyntaxSpan::text(1..2, SpanRole::Math),
        ];
        assert_eq!(inline_math_range(&missing_close, 1), 0..2);
        // 相邻片段是其它 marker（如加粗）而非 `$`：不合并，避免误伤。
        let other_markers = vec![
            SyntaxSpan::marker(0..2, SpanRole::Strong),
            SyntaxSpan::text(2..3, SpanRole::Math),
            SyntaxSpan::marker(3..5, SpanRole::Strong),
        ];
        assert_eq!(inline_math_range(&other_markers, 1), 2..3);
        // MathMarker 长度不为 1（`$$`）：不合并，防止跨块级定界符。
        let double_marker = vec![
            SyntaxSpan::marker(0..2, SpanRole::MathMarker),
            SyntaxSpan::text(2..3, SpanRole::Math),
            SyntaxSpan::marker(3..5, SpanRole::MathMarker),
        ];
        assert_eq!(inline_math_range(&double_marker, 1), 2..3);
        // 前置 marker 不与公式体首尾相接：不合并。
        let gapped = vec![
            SyntaxSpan::marker(0..1, SpanRole::MathMarker),
            SyntaxSpan::text(1..2, SpanRole::Normal),
            SyntaxSpan::text(2..3, SpanRole::Math),
        ];
        assert_eq!(inline_math_range(&gapped, 2), 2..3);
    }

    #[test]
    fn inline_math_request_range_covers_both_delimiters() {
        let src = "$x^2$";
        let requests = collect_render_requests(src);
        assert_eq!(requests.len(), 1, "{requests:?}");
        assert_eq!(requests[0].kind, RenderKind::Math);
        // 键仍为纯公式体（不含 `$`），与桌面 RaTeX 渲染键一致。
        assert_eq!(requests[0].key, "x^2");
        // 区间覆盖整个 `$…$`，替换后不再残留 `$`。
        assert_eq!(requests[0].range, 0..src.len());
        assert_eq!(&src[requests[0].range.clone()], "$x^2$");
        let mut assets = RenderAssets::new();
        assets.insert_math("x^2", SVG);
        let prepared = prepare_renderables(src, &assets, AssetTarget::VirtualFile);
        assert!(!prepared.source.contains('$'), "{}", prepared.source);
    }

    #[test]
    fn inline_math_becomes_image_widget_after_prepare() {
        let src = "计算 $x^2$ 完毕";
        let mut assets = RenderAssets::new();
        assets.insert_math("x^2", SVG);
        let prepared = prepare_renderables(src, &assets, AssetTarget::DataUri);
        // 替换后没有 `$` 残留；图片语法不被 `$…$` 包裹。
        assert!(!prepared.source.contains('$'), "{}", prepared.source);
        assert!(
            prepared
                .source
                .contains("![公式](data:image/svg+xml;base64,"),
            "{}",
            prepared.source
        );
        // 重新解析 prepared 副本：图片语法必须被识别为 Widget/ImageMarker，
        // 而不是 Math 字面（这正是用户实机看到的回归现象）。
        let spans = all_spans(&parse(&prepared.source));
        assert!(
            spans
                .iter()
                .any(|s| { s.kind == SpanKind::Widget && s.role == SpanRole::ImageMarker }),
            "应识别为图片 Widget：{spans:?}"
        );
        assert!(
            !spans.iter().any(|s| s.role == SpanRole::Math),
            "不应再有行内公式字面：{spans:?}"
        );
    }

    #[test]
    fn multiple_inline_math_on_one_line_are_disjoint() {
        let src = "行内 $a$ 与 $b$ 结束";
        let requests = collect_render_requests(src);
        assert_eq!(requests.len(), 2, "{requests:?}");
        let a = src.find("$a$").expect("应有 $a$");
        let b = src.find("$b$").expect("应有 $b$");
        assert_eq!(requests[0].range, a..a + 3);
        assert_eq!(requests[1].range, b..b + 3);
        assert_eq!(requests[0].key, "a");
        assert_eq!(requests[1].key, "b");
        // 区间不重叠，也不覆盖两个公式之间的文字。
        assert!(requests[0].range.end <= requests[1].range.start);
        assert!(!requests[0].range.contains(&b));
        assert!(!requests[1].range.contains(&a));
        let mut assets = RenderAssets::new();
        assets.insert_math("a", SVG);
        assets.insert_math("b", SVG);
        let prepared = prepare_renderables(src, &assets, AssetTarget::DataUri);
        assert!(!prepared.source.contains('$'), "{}", prepared.source);
        assert_eq!(
            prepared
                .source
                .matches("data:image/svg+xml;base64,")
                .count(),
            2,
            "{}",
            prepared.source
        );
        let spans = all_spans(&parse(&prepared.source));
        assert_eq!(
            spans
                .iter()
                .filter(|s| s.kind == SpanKind::Widget && s.role == SpanRole::ImageMarker)
                .count(),
            2,
            "两个公式都应变成图片 Widget：{spans:?}"
        );
        assert!(
            !spans.iter().any(|s| s.role == SpanRole::Math),
            "不应残留公式字面：{spans:?}"
        );
    }

    #[test]
    fn invalid_inline_math_keeps_literal() {
        // 空白/非法公式不构成请求，导出副本原样保留、不中断。
        let src = "空公式 $ $ 与跨行 $a\nb$ 保留字面\n";
        let requests = collect_render_requests(src);
        assert!(requests.is_empty(), "{requests:?}");
        let mut assets = RenderAssets::new();
        assets.insert_math("a", SVG);
        let prepared = prepare_renderables(src, &assets, AssetTarget::DataUri);
        assert_eq!(prepared.source, src);
        assert!(prepared.files.is_empty());
    }
}
