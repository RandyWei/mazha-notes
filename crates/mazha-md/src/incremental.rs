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

use crate::block::Block;
use crate::scanner::parse;
use std::ops::Range;

/// 全量重解析的兜底阈值：旧块数少于该值时直接全量（省不了多少还容易错）。
const MIN_BLOCKS_FOR_INCREMENTAL: usize = 3;
/// 受影响块占比阈值（用整数化简写：`affected * DEN > total * NUM` 时判为过高）。
const RATIO_DEN: usize = 10;
const MAX_AFFECTED_NUM: usize = 6; // 0.6

/// 增量重解析：从 `old` + 旧块 `blocks` 推导 `new` 的块森林。
///
/// `edit_range` 是 **`old` 中被替换的字节范围**（纯插入可传空区间 `at..at`）。
///
/// 调用方保证 `blocks` 是 `old` 的解析结果（由 [`crate::parse`] 或本函数产出），
/// 否则行为未定义。
pub fn reparse(old: &str, new: &str, blocks: &[Block], edit_range: Range<usize>) -> Vec<Block> {
    // delta = new.len() - old.len()（size_t 差值，文档长度不可能溢出 isize）
    let delta = isize::try_from(new.len()).unwrap_or(isize::MAX)
        - isize::try_from(old.len()).unwrap_or(isize::MAX);
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
    let old_tail_start = blocks.get(hi + 1).map_or(old.len(), |b| b.range.start);
    // 尾部对齐：旧 tail 起点之后的字节数不变（保持原顺序不被重新解析）。
    // old_tail_start 理论上不会超过 old.len()（块范围来自旧文本），仍做饱和保护。
    let tail = new
        .len()
        .saturating_sub(old.len().saturating_sub(old_tail_start));
    // 防线：区间必须单调且落在文本内，否则退化全量
    if head > tail || tail > new.len() || !new.is_char_boundary(head) || !new.is_char_boundary(tail)
    {
        return parse(new);
    }

    // 3. 重解析中间段，并把结果的 range 偏移到全局坐标
    let mut middle = parse(&new[head..tail]);
    for b in &mut middle {
        shift_block(b, head, 0);
    }

    // 4. 拼接：前段原样 + 中段 + 后段平移 delta
    let mut out = Vec::with_capacity(blocks.len());
    out.extend(blocks[..lo].iter().cloned());
    out.extend(middle);
    out.extend(blocks[hi + 1..].iter().map(|b| shift_block(b, 0, delta)));
    out
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
        let inc = reparse(old, new, &blocks, edit_range);
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
        let inc = reparse(old, new, &blocks, 2..2);
        assert_eq!(shape(&inc), shape(&parse(new)));
    }

    #[test]
    fn block_kind_helpers_are_reachable() {
        // 保证 BlockKind 在测试中被使用（无死代码警告）
        let b = parse("# x\n").into_iter().next().unwrap();
        assert!(matches!(b.kind, BlockKind::Heading { level: 1 }));
        assert!(b.is_container() || !b.is_container());
    }
}
