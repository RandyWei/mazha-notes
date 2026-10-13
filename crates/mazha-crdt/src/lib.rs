//! # mazha-crdt
//!
//! 基于 [yrs](https://crates.io/crates/yrs) 的多端同步层：在 `Y.Text` 中保存
//! Markdown 源文本，产出/消费标准 Yjs 更新帧，支持状态向量与合并。
//! 见 `SPEC.md` §6.5。
//!
//! ## 索引单位
//! 本层的对外接口使用 **字符索引**（与 `mazha-core` 的 rope 一致）；而 `yrs::TextRef`
//! 内部按 **UTF-8 字节**计量。所有跨边界处用 [`char_to_byte`] 转换，避免中文被切坏。

mod error;

pub use error::{CrdtError, CrdtResult};

use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{Doc, GetString, ReadTxn, StateVector, Text, TextRef, Transact, Update};

/// 保存源文本的根 `Y.Text` 名称。
pub const ROOT_TEXT: &str = "content";

/// 一份可同步文档：内部持有一个 yrs 文档与其根 `Y.Text`（保存 Markdown 源文本）。
#[derive(Debug)]
pub struct CrdtDoc {
    /// 底层 yrs 文档。
    doc: Doc,
    /// 根文本引用（保存源文本）。
    root: TextRef,
}

impl Default for CrdtDoc {
    fn default() -> Self {
        Self::new()
    }
}

impl CrdtDoc {
    /// 新建空文档。
    pub fn new() -> Self {
        let doc = Doc::new();
        let root = doc.get_or_insert_text(ROOT_TEXT);
        Self { doc, root }
    }

    /// 以初始源文本创建文档。
    pub fn from_text(text: &str) -> Self {
        let me = Self::new();
        {
            let mut txn = me.doc.transact_mut();
            me.root.push(&mut txn, text);
        }
        me
    }

    /// 读取当前源文本。
    pub fn text(&self) -> String {
        let txn = self.doc.transact();
        self.root.get_string(&txn)
    }

    /// 克隆一份 **CRDT 状态相同** 的独立文档（同一文档在另一端的副本）。
    ///
    /// 与 [`CrdtDoc::snapshot`] 的区别：`fork` 保留完整 CRDT 历史（可与原文档
    /// 无缝互相同步）；`snapshot` 只保留当前文本快照（历史清零）。
    ///
    /// # Panics
    /// 理论上不会 panic：自身的更新帧必然可被自身状态构成的更新解码。
    #[must_use]
    pub fn fork(&self) -> Self {
        let mut me = Self::new();
        me.apply_update(&self.encode_update())
            .expect("fork 应用自身更新应可解码");
        me
    }

    /// 生成当前文本的**快照文档**（CRDT 历史清零）。
    ///
    /// 用途：更新帧文件会随编辑历史**无限增长**（每次插入/删除都留下记录），
    /// 定期用快照替换可把存储收敛到 O(文本长度)。
    ///
    /// 代价：快照是新的 CRDT 文档（新身份），因此
    /// - 与快照**之前**的旧状态向量/更新帧不再兼容（远端需要重新全量同步）；
    /// - 快照后未同步的本地并发编辑若以旧帧发给快照后的文档，会被当作
    ///   "另一个文档的内容"合并进来（理论上不丢数据，但顺序可能与预期不同）。
    ///
    /// 因此：**仅在确认没有待同步的远端**（或愿意重新全量同步）时压缩。
    #[must_use]
    pub fn snapshot(&self) -> Self {
        Self::from_text(&self.text())
    }

    /// 当前文本的 **UTF-8 字节** 长度（yrs 原生单位）。
    pub fn byte_len(&self) -> u32 {
        let txn = self.doc.transact();
        self.root.len(&txn)
    }

    /// 在末尾追加 `chunk`（等价于 `insert(当前字符数, chunk)`）。
    ///
    /// # Errors
    /// 越界时返回 [`CrdtError`]（正常情况下不会）。
    pub fn push(&mut self, chunk: &str) -> CrdtResult<()> {
        let idx = self.text().chars().count();
        self.insert(idx, chunk)
    }

    /// 在字符索引 `index` 处插入 `chunk`。
    ///
    /// # Errors
    /// 索引超出当前文本字符数时返回 [`CrdtError::OutOfBounds`]。
    pub fn insert(&mut self, index: usize, chunk: &str) -> CrdtResult<()> {
        let cur = self.text();
        let byte = char_to_byte_checked(&cur, index)?;
        let mut txn = self.doc.transact_mut();
        self.root.insert(&mut txn, to_u32(byte)?, chunk);
        Ok(())
    }

    /// 从字符索引 `index` 起删除 `len` 个字符。
    ///
    /// # Errors
    /// 索引或长度越界时返回 [`CrdtError::OutOfBounds`]。
    pub fn delete(&mut self, index: usize, len: usize) -> CrdtResult<()> {
        let cur = self.text();
        let start = char_to_byte_checked(&cur, index)?;
        let end = char_to_byte_checked(&cur, index + len)?;
        let mut txn = self.doc.transact_mut();
        self.root
            .remove_range(&mut txn, to_u32(start)?, to_u32(end.saturating_sub(start))?);
        Ok(())
    }

    /// 用 `replacement` 覆盖 `[index, index + len)`（删除 + 插入；不产生"替换"CRDT 语义）。
    ///
    /// # Errors
    /// 索引或长度越界时返回 [`CrdtError::OutOfBounds`]。
    pub fn replace(&mut self, index: usize, len: usize, replacement: &str) -> CrdtResult<()> {
        self.delete(index, len)?;
        self.insert(index, replacement)
    }

    /// 当前状态向量（Yjs v1 编码）。用于增量同步：把自身已知版本告诉对端。
    pub fn state_vector(&self) -> Vec<u8> {
        let txn = self.doc.transact();
        txn.state_vector().encode_v1()
    }

    /// 自 `remote_sv` 之后的增量更新（Yjs v1 编码）。
    ///
    /// # Errors
    /// 状态向量解码失败时返回 [`CrdtError::Decode`]。
    pub fn encode_diff(&self, remote_sv: &[u8]) -> CrdtResult<Vec<u8>> {
        let sv = StateVector::decode_v1(remote_sv)?;
        let txn = self.doc.transact();
        Ok(txn.encode_state_as_update_v1(&sv))
    }

    /// 完整更新（等价于对空状态向量的增量），用于初次同步/持久化。
    pub fn encode_update(&self) -> Vec<u8> {
        let txn = self.doc.transact();
        txn.encode_state_as_update_v1(&StateVector::default())
    }

    /// 应用一个更新帧（本地或远端）。
    ///
    /// # Errors
    /// 更新解码失败时返回 [`CrdtError::Decode`]。
    pub fn apply_update(&mut self, bytes: &[u8]) -> CrdtResult<()> {
        let update = Update::decode_v1(bytes)?;
        let mut txn = self.doc.transact_mut();
        txn.apply_update(update)?;
        Ok(())
    }
}

/// 字符索引 → UTF-8 字节偏移（越界返回 `None`）。
fn char_to_byte(text: &str, char_index: usize) -> Option<usize> {
    if char_index == 0 {
        return Some(0);
    }
    text.char_indices()
        .nth(char_index)
        .map(|(b, _)| b)
        // 恰好等于字符数：允许（即末尾）
        .or(if char_index == text.chars().count() {
            Some(text.len())
        } else {
            None
        })
}

/// 带错误校验的 [`char_to_byte`]。
fn char_to_byte_checked(text: &str, char_index: usize) -> CrdtResult<usize> {
    char_to_byte(text, char_index).ok_or(CrdtError::OutOfBounds {
        index: char_index,
        len: text.chars().count(),
    })
}

/// `usize` 字节偏移 → yrs 的 `u32` 索引（超范围报错）。
fn to_u32(v: usize) -> CrdtResult<u32> {
    u32::try_from(v).map_err(|_| CrdtError::TooLarge(v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_text_and_read() {
        let d = CrdtDoc::from_text("你好 a\n");
        assert_eq!(d.text(), "你好 a\n");
        assert_eq!(d.byte_len(), 9); // 你(3)+好(3)+空格(1)+a(1)+\n(1)
    }

    #[test]
    fn char_index_is_byte_safe_cjk() {
        let mut d = CrdtDoc::from_text("你好世界");
        // 在第 2 个字符（"世"）前插入
        d.insert(2, "XYZ").unwrap();
        assert_eq!(d.text(), "你好XYZ世界");
        // 删除 3 个字符（XYZ）
        d.delete(2, 3).unwrap();
        assert_eq!(d.text(), "你好世界");
    }

    #[test]
    fn replace_ranges() {
        let mut d = CrdtDoc::from_text("abc def");
        d.replace(4, 3, "XYZ").unwrap();
        assert_eq!(d.text(), "abc XYZ");
    }

    #[test]
    fn out_of_bounds() {
        let mut d = CrdtDoc::from_text("你好");
        assert!(matches!(
            d.insert(5, "x"),
            Err(CrdtError::OutOfBounds { .. })
        ));
    }

    #[test]
    fn sync_between_two_docs() {
        let mut a = CrdtDoc::from_text("hello");
        let mut b = CrdtDoc::new();

        // b 初次同步 a 的全量
        let full = a.encode_update();
        b.apply_update(&full).unwrap();
        assert_eq!(b.text(), "hello");

        // 之后 a 增量同步给 b
        let sv_b = b.state_vector();
        a.insert(5, " world").unwrap();
        let diff = a.encode_diff(&sv_b).unwrap();
        b.apply_update(&diff).unwrap();
        assert_eq!(b.text(), "hello world");
    }

    #[test]
    fn concurrent_inserts_converge() {
        let mut a = CrdtDoc::from_text("ab");
        let mut b = CrdtDoc::from_text("ab");
        // 各自基于同一版本并发插入
        a.insert(1, "X").unwrap();
        b.insert(1, "Y").unwrap();
        // 互相交换更新
        let ua = a.encode_update();
        let ub = b.encode_update();
        a.apply_update(&ub).unwrap();
        b.apply_update(&ua).unwrap();
        // 收敛到同一结果（内容包含 X、Y 且两端一致）
        assert_eq!(a.text(), b.text());
        assert!(a.text().contains('X') && a.text().contains('Y'));
    }
}

#[test]
fn fork_preserves_state() {
    let a = CrdtDoc::from_text("你好**世界**");
    let b = a.fork();
    assert_eq!(a.text(), b.text());
    // fork 后各自演进，交换更新可收敛
    let mut a = a;
    let mut b = b;
    a.insert(0, "> ").unwrap();
    let diff = a.encode_diff(&b.state_vector()).unwrap();
    b.apply_update(&diff).unwrap();
    assert_eq!(a.text(), b.text());
    assert_eq!(a.text(), "> 你好**世界**");
}

#[test]
fn fork_then_concurrent_inserts() {
    let base = CrdtDoc::from_text("ab");
    let mut a = base.fork();
    let mut b = base.fork();
    a.insert(1, "X").unwrap();
    b.insert(1, "Y").unwrap();
    let ua = a.encode_update();
    let ub = b.encode_update();
    a.apply_update(&ub).unwrap();
    b.apply_update(&ua).unwrap();
    assert_eq!(a.text(), b.text());
}

#[test]
fn snapshot_compacts_update_size() {
    // 反复编辑会让更新帧不断变大；快照应把体积收敛到 O(文本长度)
    let mut d = CrdtDoc::new();
    for i in 0..50 {
        let at = d.text().chars().count();
        d.insert(at, &format!("第 {i} 行内容\n")).unwrap();
        // 每轮再删掉几个字符，制造大量历史
        let len = d.text().chars().count();
        if len > 4 {
            d.delete(len - 3, 3).unwrap();
        }
    }
    let text = d.text();
    let fat = d.encode_update();
    let snap = d.snapshot();
    assert_eq!(snap.text(), text, "快照必须保留当前文本");
    let slim = snap.encode_update();
    assert!(
        slim.len() * 2 < fat.len(),
        "快照应显著压缩：fat={} slim={}",
        fat.len(),
        slim.len()
    );
}

#[test]
fn snapshot_keeps_text_and_accepts_new_edits() {
    let mut d = CrdtDoc::from_text("你好**世界**");
    d.insert(0, "> ").unwrap();
    d.delete(3, 4).unwrap();
    let snap = d.snapshot();
    assert_eq!(snap.text(), d.text());
    // 快照继续编辑：本地自洽
    let mut snap = snap;
    snap.push("!").unwrap();
    assert_eq!(snap.text(), format!("{}!", d.text()));
}

#[test]
fn fork_and_snapshot_differ_in_history() {
    let mut a = CrdtDoc::from_text("ab");
    let fork_b = a.fork();
    let snap_c = a.snapshot();
    // fork 保留身份：可同步
    a.insert(1, "X").unwrap();
    let mut b = fork_b;
    b.apply_update(&a.encode_update()).unwrap();
    assert_eq!(a.text(), b.text());
    // 快照是全新文档：文本相同但不共享历史
    assert_eq!(snap_c.text(), "ab");
}
