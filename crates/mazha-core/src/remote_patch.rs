//! 远端增量补丁（S5b 第一批 D103，契约见 `docs/SYNC-SPEC.md` §5.11.1）。
//!
//! `apply_remote` 旧实现用 `crdt_to_document` 整体重建 `Document`，远端每来一帧都会
//! 丢掉 undo/redo 与选区。本模块把「旧文本 → 新文本」压缩成**一个** `Edit::Replace`
//! 语义的补丁，并提供纯函数偏移重映射（核心不持有 UI 选区，调用方自行 remap）。
//!
//! 裁剪公共前后缀时必须在 UTF-8 字符边界停下：汉字/emoji 的首字节可能相同，
//! 按字节贪心会得到半个多字节字符的切点，进而让 `Edit` 校验失败或切坏文本。

use std::ops::Range;

/// 一次远端变更的增量补丁：把 `old_len` 文本的 `range` 换成 `text`。
///
/// `removed` 保存被替换的原文，供应用前校验补丁是否仍与当前文本匹配（防止把
/// 过期补丁应用到已变化的文档上）。所有偏移都是 UTF-8 字节偏移且落在字符边界。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePatch {
    /// 补丁应用前文档的字节长度。
    pub old_len: usize,
    /// 补丁应用后文档的字节长度。
    pub new_len: usize,
    /// 被替换的旧文本字节范围（纯插入时为空区间）。
    pub range: Range<usize>,
    /// 新文本（纯删除时为空串）。
    pub text: String,
    /// 被替换掉的旧文本，用于校验与诊断。
    pub removed: String,
}

impl RemotePatch {
    /// 从新旧文本构造补丁；两文本相同时返回 `None`（无变化不得触动文档/历史）。
    ///
    /// 最长公共前后缀按字节比较后**回退到字符边界**，因此 `range` 两端、
    /// `text`/`removed` 的切点都不会切断 CJK 或多字节字符。
    #[must_use]
    pub fn from_texts(old: &str, new: &str) -> Option<Self> {
        if old == new {
            return None;
        }
        let prefix = common_prefix_bytes(old, new);
        let suffix = common_suffix_bytes(old, new, prefix);
        let old_end = old.len() - suffix;
        let new_end = new.len() - suffix;
        Some(Self {
            old_len: old.len(),
            new_len: new.len(),
            range: prefix..old_end,
            text: new[prefix..new_end].to_owned(),
            removed: old[prefix..old_end].to_owned(),
        })
    }

    /// 把旧坐标的单字节偏移映射到新坐标。
    ///
    /// 规则（契约 §5.11.1）：`offset <= range.start` 不变；`offset >= range.end`
    /// 平移 `new_len - old_len`；落在被替换区间内部则钳到新插入文本末端。
    /// 缩减时先 saturating 相减，避免长度差导致 usize 下溢。
    #[must_use]
    pub fn remap_offset(&self, offset: usize) -> usize {
        let offset = offset.min(self.old_len);
        if offset <= self.range.start {
            offset
        } else if offset >= self.range.end {
            if self.new_len >= self.old_len {
                offset + (self.new_len - self.old_len)
            } else {
                offset.saturating_sub(self.old_len - self.new_len)
            }
        } else {
            self.range.start + self.text.len()
        }
    }

    /// 把旧坐标的字节范围映射到新坐标（光标选区/历史编辑范围共用）。
    #[must_use]
    pub fn remap_range(&self, range: Range<usize>) -> Range<usize> {
        self.remap_offset(range.start)..self.remap_offset(range.end)
    }

    /// 补丁是否可直接作用于给定旧文本：长度一致且被替换片段逐字节相同。
    #[must_use]
    pub fn matches(&self, old: &str) -> bool {
        old.len() == self.old_len && old.get(self.range.clone()) == Some(self.removed.as_str())
    }
}

/// 公共前缀的字节数，回退到两串都合法的字符边界。
fn common_prefix_bytes(old: &str, new: &str) -> usize {
    let mut count = old
        .bytes()
        .zip(new.bytes())
        .take_while(|(left, right)| left == right)
        .count();
    while count > 0 && (!old.is_char_boundary(count) || !new.is_char_boundary(count)) {
        count -= 1;
    }
    count
}

/// 去掉公共前缀后的公共后缀字节数，同样回退到两串都合法的字符边界。
fn common_suffix_bytes(old: &str, new: &str, prefix: usize) -> usize {
    let old_rest = &old[prefix..];
    let new_rest = &new[prefix..];
    let mut count = old_rest
        .bytes()
        .rev()
        .zip(new_rest.bytes().rev())
        .take_while(|(left, right)| left == right)
        .count()
        .min(old_rest.len())
        .min(new_rest.len());
    while count > 0
        && (!old_rest.is_char_boundary(old_rest.len() - count)
            || !new_rest.is_char_boundary(new_rest.len() - count))
    {
        count -= 1;
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 补丁必须能把旧文本逐字节重放回新文本（所有用例的共同不变式）。
    fn assert_replay(old: &str, new: &str) -> RemotePatch {
        let patch = RemotePatch::from_texts(old, new).expect("不同文本应产生补丁");
        assert!(patch.matches(old));
        assert_eq!(&old[..patch.range.start], &new[..patch.range.start]);
        assert_eq!(patch.removed, &old[patch.range.clone()]);
        assert_eq!(
            patch.text,
            &new[patch.range.start..patch.new_len - (old.len() - patch.range.end)]
        );
        patch
    }

    #[test]
    fn identical_text_yields_no_patch() {
        assert!(RemotePatch::from_texts("你好", "你好").is_none());
        assert!(RemotePatch::from_texts("", "").is_none());
    }

    #[test]
    fn prefix_and_suffix_are_trimmed() {
        let patch = assert_replay("前缀hello后缀", "前缀hello世界后缀");
        assert_eq!(patch.range, "前缀hello".len().."前缀hello".len());
        assert_eq!(patch.text, "世界");
        assert_eq!(patch.removed, "");
    }

    #[test]
    fn pure_delete_keeps_empty_text() {
        let patch = assert_replay("前缀你好后缀", "前缀后缀");
        assert_eq!(patch.removed, "你好");
        assert_eq!(patch.text, "");
        assert_eq!(patch.new_len, "前缀后缀".len());
    }

    #[test]
    fn full_replace_and_empty_document() {
        let patch = assert_replay("旧文本", "新内容");
        assert_eq!(patch.range, 0.."旧文本".len());
        let from_empty = assert_replay("", "从零开始");
        assert_eq!(from_empty.range, 0..0);
        let to_empty = assert_replay("整段删除", "");
        assert_eq!(to_empty.range, 0.."整段删除".len());
        assert_eq!(to_empty.new_len, 0);
    }

    #[test]
    fn cjk_and_emoji_never_split_multibyte_characters() {
        // "一"(E4 B8 80) 与 "丁"(E4 B8 81) 共享两个字节，贪心前缀会切坏汉字。
        let patch = assert_replay("一乙", "丁乙");
        assert_eq!(patch.removed, "一");
        assert_eq!(patch.text, "丁");
        let emoji = assert_replay("🌿甲", "🌱甲");
        assert_eq!(emoji.removed, "🌿");
        assert_eq!(emoji.text, "🌱");
    }

    #[test]
    fn remap_offset_covers_insert_delete_and_replace() {
        let insert = assert_replay("abc", "abXYZc");
        assert_eq!(insert.remap_offset(0), 0);
        assert_eq!(insert.remap_offset(2), 2);
        assert_eq!(insert.remap_offset(3), 6);
        let delete = assert_replay("abXYZc", "abc");
        assert_eq!(delete.remap_offset(2), 2);
        assert_eq!(delete.remap_offset(5), 2);
        let replace = assert_replay("abXYZc", "ab世界c");
        // 落在被替换区间内的偏移钳到新文本末端（世界共 6 字节，起点 2）。
        assert_eq!(replace.remap_offset(4), 8);
        assert_eq!(replace.remap_offset(5), 8);
        assert_eq!(replace.remap_offset(6), 9);
    }

    #[test]
    fn remap_range_preserves_order_and_clamps_out_of_bounds() {
        let patch = assert_replay("你好世界", "你好");
        assert_eq!(patch.remap_range(0..6), 0..6);
        assert_eq!(patch.remap_range(3..5), 3..5);
        assert_eq!(patch.remap_range(8..10), 6..6);
        assert_eq!(patch.remap_range(0..999), 0..6);
    }

    #[test]
    fn stale_patch_is_rejected_without_touching_text() {
        let patch = assert_replay("原文", "新文");
        assert!(!patch.matches("别的"));
        assert!(!patch.matches("原文更多"));
    }
}
