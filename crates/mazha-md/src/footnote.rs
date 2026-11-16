//! 脚注索引（决策 D69）。
//!
//! 把散落在块森林里的脚注信息汇总成一张表：标签、序号、引用位置与定义块位置。
//! 这是**纯模型层**的派生数据（不涉及 GUI，也不改写源文本），供两个上层使用：
//!
//! - `mazha-core` / 桌面端：命中脚注标签后决定跳转目标（引用 ↔ 定义）；
//! - `mazha-export`：导出前按序号重写标签并把定义归集到文末。
//!
//! ## 编号规则
//!
//! 序号 = **引用在文档中首次出现的顺序**（1 起）。只有定义、从未被引用的标签
//! 排在所有被引用标签之后，仍按定义出现顺序继续编号，避免导出时丢内容。
//!
//! ## 位置约定
//!
//! - 引用位置是**标签可见文本**的源字节区间（不含 `[^`/`]` 标记）；
//! - 定义位置是整个定义块的范围（从 `[^label]:` 行首到最后一个续行行尾），
//!   便于 UI 把定义块整体滚动到视口顶部；
//! - 同一标签被多次引用时 `references` 保留全部区间，按出现顺序排列；
//!   重复定义只取**第一个**（与 GFM「重复定义无效」一致）。
//!
//! 所有偏移都是源文本字节偏移，越界输入按区间比较自然返回 `None`，不 panic。

use crate::{Block, BlockKind, SpanKind, SpanRole};
use std::ops::Range;

/// 单个脚注在文档中的汇总信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FootnoteEntry {
    /// 标签原文（`[^` 与 `]` 之间的文本）。
    pub label: String,
    /// 显示序号（1 起，按引用首次出现顺序分配）。
    pub number: usize,
    /// 所有引用标签的源区间（按出现顺序；定义头部的标签不算引用）。
    pub references: Vec<Range<usize>>,
    /// 定义块范围；只有引用、没有定义时为 `None`。
    pub definition: Option<Range<usize>>,
}

impl FootnoteEntry {
    /// 该脚注是否真的有定义块（导出时据此决定是否归集）。
    pub fn has_definition(&self) -> bool {
        self.definition.is_some()
    }
}

/// 整篇文档的脚注索引。
///
/// 构造后只读；桌面端点击时才重建（文档量级下遍历块森林足够便宜），
/// 导出各格式在入口构建一次。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FootnoteIndex {
    /// 按序号排列（`number == index + 1`）。
    entries: Vec<FootnoteEntry>,
}

impl FootnoteIndex {
    /// 从源文本与块森林构建索引。
    ///
    /// `source` 必须与 `blocks` 同源（块区间是相对该文本的字节偏移）；
    /// 文档没有脚注时返回空索引（`entries` 为空）。
    pub fn new(source: &str, blocks: &[Block]) -> Self {
        // 先按文档顺序收集「引用」与「定义」两类原始记录。
        // 分开收集的原因：编号只看引用顺序，而定义可能出现在引用之前或之后。
        let mut refs: Vec<(String, Range<usize>)> = Vec::new();
        let mut defs: Vec<(String, Range<usize>)> = Vec::new();
        collect(source, blocks, &mut refs, &mut defs);

        let mut entries: Vec<FootnoteEntry> = Vec::new();
        // 引用先入表：保证序号 == 首次引用顺序。
        for (label, range) in refs {
            let idx = entry_index(&mut entries, &label);
            entries[idx].references.push(range);
        }
        // 只有定义、没有引用的标签追加到末尾；重复定义保留第一个。
        for (label, range) in defs {
            let idx = entry_index(&mut entries, &label);
            if entries[idx].definition.is_none() {
                entries[idx].definition = Some(range);
            }
        }
        for (i, entry) in entries.iter_mut().enumerate() {
            entry.number = i + 1;
        }
        Self { entries }
    }

    /// 按序号排列的全部脚注。
    pub fn entries(&self) -> &[FootnoteEntry] {
        &self.entries
    }

    /// 是否没有任何脚注（引用或定义都没有）。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 脚注数量。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 按标签查找（标签区分大小写，与解析器一致）。
    pub fn get(&self, label: &str) -> Option<&FootnoteEntry> {
        self.entries.iter().find(|e| e.label == label)
    }

    /// 按标签取序号（1 起）。
    pub fn number_of(&self, label: &str) -> Option<usize> {
        self.get(label).map(|e| e.number)
    }

    /// 查找 `src` 落在哪个引用标签上（两端包含，便于光标停在标签末尾）。
    pub fn entry_for_reference(&self, src: usize) -> Option<&FootnoteEntry> {
        self.entries
            .iter()
            .find(|e| e.references.iter().any(|r| touches(r, src)))
    }

    /// 查找 `src` 落在哪个定义块内（整个块范围，便于点击定义正文也能回跳）。
    pub fn entry_for_definition(&self, src: usize) -> Option<&FootnoteEntry> {
        self.entries
            .iter()
            .find(|e| e.definition.as_ref().is_some_and(|r| touches(r, src)))
    }
}

/// 区间是否覆盖 `src`（终点视为包含，兼容"光标停在标签末尾"）。
fn touches(range: &Range<usize>, src: usize) -> bool {
    src >= range.start && src <= range.end
}

/// 在表中查找标签；不存在时插入一个空条目并返回下标。
fn entry_index(entries: &mut Vec<FootnoteEntry>, label: &str) -> usize {
    if let Some(i) = entries.iter().position(|e| e.label == label) {
        return i;
    }
    entries.push(FootnoteEntry {
        label: label.to_string(),
        number: 0,
        references: Vec::new(),
        definition: None,
    });
    entries.len() - 1
}

/// 深度优先收集引用与定义（文档顺序）。
///
/// 定义块头部也有 `SpanRole::FootnoteRef`（标签本身可见），必须跳过**第一个**
/// 该角色片段，否则每个定义都会被误算成一次引用、打乱编号。
fn collect(
    source: &str,
    blocks: &[Block],
    refs: &mut Vec<(String, Range<usize>)>,
    defs: &mut Vec<(String, Range<usize>)>,
) {
    for block in blocks {
        let is_def = matches!(block.kind, BlockKind::FootnoteDef { .. });
        let mut skipped_head = false;
        for span in &block.spans {
            if span.kind != SpanKind::Text || span.role != SpanRole::FootnoteRef {
                continue;
            }
            if is_def && !skipped_head {
                skipped_head = true;
                continue;
            }
            // 标签是可见文本，按源区间切出原文（`&str` 索引在解析器保证的字符边界上）。
            if let Some(label) = source.get(span.range.clone()) {
                refs.push((label.to_string(), span.range.clone()));
            }
        }
        if let BlockKind::FootnoteDef { label } = &block.kind {
            defs.push((label.clone(), block.range.clone()));
        }
        collect(source, &block.children, refs, defs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    fn index_of(source: &str) -> FootnoteIndex {
        FootnoteIndex::new(source, &parse(source))
    }

    #[test]
    fn numbers_follow_first_reference_order() {
        // 引用顺序 b → a，因此 b 是 1、a 是 2（与定义书写顺序无关）
        let src = "见[^b]与[^a]。\n\n[^a]: A\n[^b]: B\n";
        let index = index_of(src);
        assert_eq!(index.len(), 2);
        let b = index.get("b").unwrap();
        let a = index.get("a").unwrap();
        assert_eq!((b.number, a.number), (1, 2));
        assert_eq!(index.entries()[0].label, "b");
        assert_eq!(index.numbers_for_test(), vec![1, 2]);
    }

    #[test]
    fn repeated_reference_shares_entry_and_keeps_all_ranges() {
        let src = "甲[^1] 乙[^1]\n\n[^1]: note\n";
        let index = index_of(src);
        assert_eq!(index.len(), 1);
        let e = index.get("1").unwrap();
        assert_eq!(e.references.len(), 2);
        assert_eq!(e.number, 1);
    }

    #[test]
    fn definition_head_is_not_a_reference() {
        // 定义头部标签不能再算一次引用（否则编号/引用计数翻倍）
        let src = "[^a]: note\n\n见[^a]。\n";
        let index = index_of(src);
        let e = index.get("a").unwrap();
        assert_eq!(e.references.len(), 1);
        assert_eq!(e.number, 1);
    }

    #[test]
    fn unreferenced_definition_is_numbered_after_referenced() {
        let src = "见[^a]。\n\n[^b]: B\n[^a]: A\n";
        let index = index_of(src);
        let a = index.get("a").unwrap();
        let b = index.get("b").unwrap();
        assert_eq!((a.number, b.number), (1, 2));
        assert!(b.references.is_empty());
    }

    #[test]
    fn lookup_helpers_resolve_reference_and_definition_offsets() {
        let src = "见[^a]。\n\n[^a]: note\n";
        let index = index_of(src);
        let entry = index.get("a").unwrap();
        let ref_range = entry.references[0].clone();
        let def_range = entry.definition.clone().unwrap();
        assert_eq!(
            index
                .entry_for_reference(ref_range.start)
                .map(|e| e.label.as_str()),
            Some("a")
        );
        assert_eq!(
            index
                .entry_for_reference(ref_range.end)
                .map(|e| e.label.as_str()),
            Some("a"),
            "标签末尾（光标可落点）也算命中"
        );
        assert!(
            index.entry_for_reference(def_range.start).is_none(),
            "定义块起点不是引用位置"
        );
        assert_eq!(
            index
                .entry_for_definition(def_range.end - 1)
                .map(|e| e.label.as_str()),
            Some("a"),
            "定义正文任意位置都能回跳"
        );
    }

    #[test]
    fn duplicate_definition_keeps_first_block() {
        let src = "[^a]: first\n\n[^a]: second\n";
        let index = index_of(src);
        let def = index.get("a").unwrap().definition.clone().unwrap();
        assert_eq!(&src[def], "[^a]: first");
    }

    #[test]
    fn empty_or_plain_documents_have_empty_index() {
        assert!(index_of("").is_empty());
        assert!(index_of("# 标题\n\n普通段落\n").is_empty());
        assert_eq!(FootnoteIndex::default(), index_of("普通段落\n"));
    }

    #[test]
    fn out_of_range_lookups_do_not_panic() {
        let index = index_of("见[^a]。\n\n[^a]: note\n");
        assert!(index.entry_for_reference(usize::MAX).is_none());
        assert!(index.entry_for_definition(usize::MAX).is_none());
        assert!(index.number_of("missing").is_none());
    }

    // 小工具：让序号断言可以一次性比较（避免测试里重复 map/collect）。
    impl FootnoteIndex {
        fn numbers_for_test(&self) -> Vec<usize> {
            self.entries.iter().map(|e| e.number).collect()
        }
    }
}
