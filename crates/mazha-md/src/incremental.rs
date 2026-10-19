//! 增量块级重解析（SPEC §6.1「增量重建」的解析侧）。
//!
//! 全量解析（[`crate::parse`]）的时间与文档大小成正比；编辑时只有局部变化，
//! 因此维护一份旧 [`Block`] 森林，按"受影响的行区间"重解析，其余块只平移字节范围。
//!
//! ## 算法（保守但正确）
//! 1. 用**旧文本**中被替换的范围 `edit_range` 定位受影响的旧块区间 `[lo, hi]`
//!    （与该范围相交的所有块），并向两端各扩展一块作为安全余量——避免"删除块间
//!    空行使相邻两块合并"这类跨块边界的情况被漏掉。
//! 2. 在新文本中取 `[head, tail)` 区间重解析：`head` 取 `lo` 块起点（不受影响，
//!    无需平移）；`tail` 从**尾部对齐**（`new.len() - (old.len() - 旧 tail 起点)`），
//!    避免在前部做区间平移时踩到边界外的内容。
//! 3. 结果 = 旧块 `[..lo]`（原样）+ 重解析出的中间块（整体偏移 `head`）+ 旧块
//!    `[hi+1..]`（range 平移 `delta`）。
//!
//! ## 安全兜底
//! 文档很短、旧块缺失或受影响块占比过高时退化为**全量解析**，宁可多做也不做错。

use crate::block::{Block, BlockKind};
use crate::scanner::parse;
use crate::span::SpanRole;
use std::ops::Range;

/// 全量重解析的兜底阈值：旧块数少于该值时直接全量（省不了多少还容易错）。
const MIN_BLOCKS_FOR_INCREMENTAL: usize = 3;
/// 受影响块占比阈值（用整数化简写：`affected * DEN > total * NUM` 时判为过高）。
const RATIO_DEN: usize = 10;
const MAX_AFFECTED_NUM: usize = 6; // 0.6

/// 增量重解析：从旧块森林 `blocks` 推导新文本 `new` 的块森林。
///
/// `old_len` 是**旧文本的字节长度**，`edit_range` 是 **旧文本中被替换的字节范围**
/// （纯插入可传空区间 `at..at`）。注意：函数只需要旧文本长度（供尾部对齐与
/// delta 计算），不需要旧文本内容本身——调用方因此无需保留旧文本副本。
///
/// 调用方保证 `blocks` 是旧文本的解析结果（由 [`crate::parse`] 或本函数产出），
/// 否则行为未定义。
pub fn reparse(
    new: &str,
    old_len: usize,
    blocks: &[Block],
    edit_range: Range<usize>,
) -> Vec<Block> {
    // delta = new.len() - old_len
    let delta = isize::try_from(new.len()).unwrap_or(isize::MAX)
        - isize::try_from(old_len).unwrap_or(isize::MAX);
    // 兜底：文档太短或没有旧块，直接全量
    if blocks.is_empty() || blocks.len() < MIN_BLOCKS_FOR_INCREMENTAL {
        return parse(new);
    }

    // 1. 定位受影响的旧块区间（与 edit_range 相交），并前后各放宽一块。
    //    找不到时兜底到首/末块：编辑落在所有块之后=末尾追加，落在所有块之前=开头插入，
    //    这两种情况都必须重解析（否则新增内容不会被解析成块）。
    let last = blocks.len() - 1;
    let lo = blocks
        .iter()
        .position(|b| b.range.end > edit_range.start)
        .unwrap_or(last)
        .saturating_sub(1);
    let hi = blocks
        .iter()
        .rposition(|b| b.range.start < edit_range.end)
        .unwrap_or(0);
    let hi = (hi + 1).min(last);

    // 命中比例过高 → 全量（用整数乘避免浮点转换的精度告警）
    // affected / total > 0.6  ⟺  affected * 10 > total * 6
    let affected = hi - lo + 1;
    if affected * RATIO_DEN > blocks.len() * MAX_AFFECTED_NUM {
        return parse(new);
    }

    // 2. 计算新文本中的重解析区间 [head, tail)
    //    - head：lo 块起点（在编辑之前，无需平移）
    //    - tail：从尾部对齐（旧 tail 起点之后的字节数不变）
    let head = blocks[lo].range.start;
    let old_tail_start = blocks.get(hi + 1).map_or(old_len, |b| b.range.start);
    // 尾部对齐：旧 tail 起点之后的字节数不变（保持原顺序不被重新解析）。
    // old_tail_start 理论上不会超过 old.len()（块范围来自旧文本），仍做饱和保护。
    let tail = new
        .len()
        .saturating_sub(old_len.saturating_sub(old_tail_start));
    // 防线：区间必须单调且落在文本内，否则退化全量
    if head > tail || tail > new.len() || !new.is_char_boundary(head) || !new.is_char_boundary(tail)
    {
        return parse(new);
    }

    // 3. 重解析中间段，并把结果的 range 平移到全局坐标（注意 shift_block 返回新块）
    let mut middle: Vec<Block> = parse(&new[head..tail])
        .iter()
        .map(|b| shift_block(b, head, 0))
        .collect();

    // 3b. 健全性校验：若裁切点"不安全"，扩充到文档末尾重解析。
    //     安全 = tail 前是空行（段落/引用/列表/标题都会被空行闭合）
    //           且区域最后一块不是"未闭合"的围栏代码块（围栏会跨越空行）。
    //     命中任一不安全条件，说明该构造可能延伸到 tail 之外，只能整段重解析。
    if !is_safe_cut(new, tail, &middle) {
        middle = parse(&new[head..])
            .iter()
            .map(|b| shift_block(b, head, 0))
            .collect();
        return splice(blocks, lo, &middle, &[], delta);
    }

    // 4. 拼接：前段原样 + 中段 + 后段平移 delta
    splice(blocks, lo, &middle, &blocks[hi + 1..], delta)
}

/// 拼接：前段 `blocks[..lo]` 原样 + 中段 + 后段平移 `delta`。
fn splice(
    blocks: &[Block],
    lo: usize,
    middle: &[Block],
    tail_blocks: &[Block],
    delta: isize,
) -> Vec<Block> {
    let mut out = Vec::with_capacity(blocks.len());
    out.extend(blocks[..lo].iter().cloned());
    out.extend(middle.iter().cloned());
    out.extend(tail_blocks.iter().map(|b| shift_block(b, 0, delta)));
    out
}

/// 判定 `tail` 是否为安全裁切点（配合 `region` 的解析结果）。
///
/// 安全条件：
/// 1. `tail` **之前是空行**——段落/标题/引用/列表都会被空行闭合（本扫描器不支持惰性续行）；
/// 2. 区域最后一块不是**未闭合的围栏代码块**——围栏是唯一能跨越空行的构造。
fn is_safe_cut(new: &str, tail: usize, region: &[Block]) -> bool {
    let blank_before =
        tail <= 1 || new[..tail].ends_with("\n\n") || new[..tail].ends_with("\n\r\n");
    if !blank_before {
        return false;
    }
    !region.last().is_some_and(is_unclosed_fence)
}

/// 是否为**未闭合**的围栏代码块 / 显示公式块（都只剩开栏标记，不足两个）。
///
/// 公式块与围栏一样可以跨越空行（未闭合时一直延伸到文档末尾），
/// 所以裁切点校验必须同样识别它，否则会把公式块拦腰切断（D34 ①）。
fn is_unclosed_fence(block: &Block) -> bool {
    let n = |role: SpanRole| block.spans.iter().filter(|s| s.role == role).count();
    match block.kind {
        BlockKind::CodeBlock { fenced: true, .. } => n(SpanRole::FenceMarker) < 2,
        BlockKind::MathBlock => n(SpanRole::MathMarker) < 2,
        _ => false,
    }
}

/// 平移一个块及其全部子孙/片段的 range（`start` 前置偏移 + `delta` 全局平移）。
fn shift_block(block: &Block, start: usize, delta: isize) -> Block {
    let mut b = block.clone();
    shift_ranges_in(&mut b, start, delta);
    b
}

/// 在块内递归平移所有 range 字段。
fn shift_ranges_in(block: &mut Block, start: usize, delta: isize) {
    block.range = shift_range(block.range.clone(), start, delta);
    for sp in &mut block.spans {
        sp.range = shift_range(sp.range.clone(), start, delta);
    }
    for ch in &mut block.children {
        shift_ranges_in(ch, start, delta);
    }
}

/// 平移单个偏移量：先加前置偏移，再按 delta 加/减（全程饱和运算）。
fn shift_offset(v: usize, start: usize, delta: isize) -> usize {
    let base = v.saturating_add(start);
    let step = delta.unsigned_abs();
    if delta >= 0 {
        base.saturating_add(step)
    } else {
        base.saturating_sub(step)
    }
}

/// 平移区间（起点/终点各自平移）。
fn shift_range(r: Range<usize>, start: usize, delta: isize) -> Range<usize> {
    let s = shift_offset(r.start, start, delta);
    let e = shift_offset(r.end, start, delta);
    s..e
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BlockKind;

    /// 断言增量结果与全量一致（比较可读文本 + 结构种类序列）。
    fn assert_matches_full(old: &str, new: &str, edit_range: Range<usize>) {
        let blocks = parse(old);
        let inc = reparse(new, old.len(), &blocks, edit_range);
        let full = parse(new);
        assert_eq!(
            shape(&inc),
            shape(&full),
            "增量结果应与全量一致\nold={old:?}\nnew={new:?}\ninc={inc:#?}\nfull={full:#?}"
        );
        assert_eq!(ranges(&inc), ranges(&full), "字节范围应一致");
    }

    /// 结构签名：块类型 + 可见文本，便于快速定位差异。
    fn shape(blocks: &[Block]) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for b in blocks {
            out.push((format!("{:?}", b.kind), visible(b)));
            for c in &b.children {
                out.push((format!("{:?}", c.kind), visible(c)));
            }
        }
        out
    }

    /// 块的可见文本（取 spans 中非空 Text 片段拼接）。
    fn visible(b: &Block) -> String {
        let mut s = String::new();
        for sp in &b.spans {
            if sp.is_text() && !sp.is_empty() {
                // Text 片段的文本内容需要源文本；这里仅用 range 长度做轻量签名
                s.push_str("<t");
                s.push_str(&sp.len().to_string());
                s.push('>');
            }
        }
        s
    }

    /// 全部块 range 的平铺签名（含嵌套）。
    fn ranges(blocks: &[Block]) -> Vec<Range<usize>> {
        let mut out = Vec::new();
        for b in blocks {
            out.push(b.range.clone());
            for c in &b.children {
                out.push(c.range.clone());
            }
        }
        out
    }

    #[test]
    fn insert_into_paragraph_matches_full() {
        let old = "# 标题\n\n第一段内容\n\n第二段内容\n";
        // 在"第一段"的"容"后插入"XX"（old 字节偏移按 ASCII 估：标题 4 + 换行 1 + 空行 1 + 其余）
        let at = old.find("内").unwrap();
        let mut new = String::new();
        new.push_str(&old[..at]);
        new.push_str("新增");
        new.push_str(&old[at..]);
        assert_matches_full(old, &new, at..at);
    }

    #[test]
    fn insert_cjk_matches_full() {
        let old = "你好\n世界\n";
        let new = "你好呀\n世界\n";
        assert_matches_full(old, new, 3..6); // "好"（3..6）替换为 "好呀"
    }

    #[test]
    fn delete_blank_line_merges_paragraphs() {
        let old = "第一段\n\n第二段\n";
        let blank = old.find("\n\n").unwrap();
        let new = format!("{}\n{}", &old[..blank], &old[blank + 2..]);
        // 删除空行：old_range 覆盖被删的空行
        assert_matches_full(old, &new, blank..blank + 2);
    }

    #[test]
    fn insert_inside_code_block_matches_full() {
        let old = "```rust\nfn a() {}\nfn b() {}\n```\n";
        let at = old.find("fn b").unwrap();
        let mut new = String::new();
        new.push_str(&old[..at]);
        new.push_str("fn c() {}\n");
        new.push_str(&old[at..]);
        assert_matches_full(old, &new, at..at);
    }

    #[test]
    fn heading_marker_makes_paragraph_heading() {
        let old = "标题文字\n\n第二段\n";
        let new = "# 标题文字\n\n第二段\n".to_string();
        assert_matches_full(old, &new, 0..0);
    }

    #[test]
    fn purely_appended_text_shifts_tails() {
        let old = "a\n\nb\n\nc\n";
        let mut new = String::from(old);
        new.push_str("\n新增末尾\n");
        assert_matches_full(old, &new, old.len()..old.len());
    }

    #[test]
    fn short_document_falls_back_to_full() {
        // 块数少于阈值：应退化为全量（结果仍须正确）
        let old = "a\n";
        let new = "a\nb\n";
        let blocks = parse(old);
        let inc = reparse(new, old.len(), &blocks, 2..2);
        assert_eq!(shape(&inc), shape(&parse(new)));
    }

    #[test]
    fn block_kind_helpers_are_reachable() {
        // 保证 BlockKind 在测试中被使用（无死代码警告）
        let b = parse("# x\n").into_iter().next().unwrap();
        assert!(matches!(b.kind, BlockKind::Heading { level: 1 }));
        assert!(b.is_container() || !b.is_container());
    }

    /// 单条编辑：插入/删除一段文本。
    enum Op {
        Insert(usize, &'static str),
        Delete(usize, usize),
    }

    impl Op {
        /// 应用到文本上；返回旧文本中被替换的范围（供 reparse 定位脏区）。
        fn apply(&self, text: &mut String) -> Range<usize> {
            match *self {
                Op::Insert(at, s) => {
                    let at = at.min(text.len());
                    // 边界修正：插入点必须落在字符边界
                    let at = snap(text, at);
                    text.insert_str(at, s);
                    at..at
                }
                Op::Delete(start, len) => {
                    let start = snap(text, start.min(text.len()));
                    let end = snap(text, (start + len).min(text.len()));
                    let range = start..end.max(start);
                    text.replace_range(range.clone(), "");
                    range
                }
            }
        }
    }

    /// 把任意偏移吸附到最近的字符边界。
    fn snap(text: &str, at: usize) -> usize {
        let mut at = at.min(text.len());
        while at > 0 && !text.is_char_boundary(at) {
            at -= 1;
        }
        at
    }

    #[test]
    fn inserted_before_fence_makes_it_unclosed() {
        // 在围栏开栏行前插入文本 → 开栏行不再是行首 → 围栏未闭合并吞掉后续内容。
        // 必须整段重解析到文档末尾，否则会得到错误结构（回归测试）。
        let old = "开头\n\n```rust\nfn a() {}\n```\n\n引用\n\n结尾\n";
        let at = old.find("```rust").unwrap();
        let new = format!("{}插入的{}", &old[..at], &old[at..]);
        assert_matches_full(old, &new, at..at);
    }

    #[test]
    fn adjacent_blocks_require_blank_before_tail() {
        // 无空行相邻的块（标题后紧跟段落）：裁切点前不是空行 → 必须整段重解析
        let old = "# 标题\n正文一段\n\n中间\n\n第二段\n\n第三段\n";
        let at = old.find("正文").unwrap();
        let new = format!("{}插入{}", &old[..at], &old[at..]);
        assert_matches_full(old, &new, at..at);
    }

    #[test]
    fn edit_sequence_matches_full_parse() {
        // 一串确定性编辑，逐步与全量解析对比：覆盖跨块增删、CJK、代码块、引用、列表。
        let seed = "# 标题\n\n```rust\nfn a() {}\n```\n\n> 引用\n\n- 列表一\n- 列表二\n\n结尾\n";
        let ops = [
            Op::Insert(10, "插入的"),       // 标题行尾
            Op::Insert(0, "# "),            // 标题前缀化
            Op::Delete(0, 2),               // 撤销前缀化
            Op::Insert(30, "\n\n新段落\n"), // 代码块前插入段落
            Op::Insert(60, "> "),           // 引用前缀
            Op::Insert(80, "- [x] "),       // 任务项
            Op::Delete(40, 6),              // 删一段代码行内容
            Op::Insert(200, "\n\n末尾追加\n"),
            Op::Insert(3, "**粗** "), // 行内标记插入
            Op::Delete(0, 1),         // 删标题首字符
        ];

        let mut text = seed.to_string();
        let mut blocks = parse(&text);
        for (i, op) in ops.iter().enumerate() {
            let old_len = text.len();
            let affected = op.apply(&mut text);
            blocks = reparse(&text, old_len, &blocks, affected);
            let full = parse(&text);
            assert_eq!(
                shape(&blocks),
                shape(&full),
                "第 {i} 步编辑后增量与全量不一致\ntext={text:?}\ncached={:?}\nfull={:?}",
                shape(&blocks),
                shape(&full),
            );
            assert_eq!(ranges(&blocks), ranges(&full), "第 {i} 步字节范围不一致");
        }

        // 最终文本仍可往返（块覆盖完整）
        assert!(!blocks.is_empty());
    }

    // ---------------- 公式块的增量重解析（D34 ①） ----------------

    #[test]
    fn unclosed_math_block_not_cut_across_blank() {
        // 未闭合公式块跨越空行：裁切点校验必须识别它，否则会把公式块拦腰切断
        let old = "前言\n\n$$\nbody\n";
        let at = old.find("body").unwrap();
        let new = format!("{}BODY{}", &old[..at], &old[at + 4..]);
        assert_matches_full(old, &new, at..at + 4);
    }

    #[test]
    fn edit_inside_closed_math_block_matches_full() {
        let old = "前言\n\n$$\na\n$$\n\n尾\n";
        let at = old.find("\na\n").unwrap() + 1;
        let new = format!("{}ab{}", &old[..at], &old[at + 1..]);
        assert_matches_full(old, &new, at..at + 1);
    }

    #[test]
    fn append_close_dollar_matches_full() {
        // 未闭合 → 补上收尾行
        let old = "$$\nx\n";
        let new = "$$\nx\n$$\n";
        assert_matches_full(old, new, old.len()..old.len());
    }

    #[test]
    fn destroy_open_marker_drops_math_block() {
        // 开栏行被改写 → 不再是公式块（后续 `$$` 单独成块）
        let old = "前文\n\n$$\nx\n$$\n";
        let at = old.find("$$").unwrap();
        let new = "前文\n\n!!\nx\n$$\n";
        assert_matches_full(old, new, at..at + 2);
    }

    #[test]
    fn edit_math_inside_quote_matches_full() {
        let old = "> $$\n> x\n> $$\n";
        let at = old.find('x').unwrap();
        let new = format!("{}y{}", &old[..at], &old[at + 1..]);
        assert_matches_full(old, &new, at..at + 1);
    }

    // ---------------- 脚注的增量重解析（D35） ----------------

    #[test]
    fn edit_footnote_def_body_matches_full() {
        let old = "前文\n\n[^1]: note\n\n尾\n";
        let at = old.find("note").unwrap();
        let new = format!("{}长注解{}", &old[..at], &old[at + 4..]);
        assert_matches_full(old, &new, at..at + 4);
    }

    #[test]
    fn add_footnote_ref_matches_full() {
        let old = "正文\n\n[^1]: note\n";
        let new = "正文[^1]\n\n[^1]: note\n";
        assert_matches_full(old, new, 6..6);
    }

    #[test]
    fn footnote_def_continuation_edit_matches_full() {
        // 续行的增补/删除不改变块结构（仍在同一定义内）
        let old = "前文\n\n[^1]: first\n    续行\n\n尾\n";
        let at = old.find("续行").unwrap();
        let new = format!("{}第二行{}", &old[..at], &old[at + 6..]);
        assert_matches_full(old, &new, at..at + 6);
    }

    #[test]
    fn unindent_footnote_continuation_splits_block() {
        // 去掉续行缩进 → 定义结束，尾随行成为独立段落
        let old = "前文\n\n[^1]: first\n    second\n";
        let at = old.find("    second").unwrap();
        let new = format!("{}second{}", &old[..at], &old[at + 4..]);
        assert_matches_full(old, &new, at..at + 4);
    }
}
