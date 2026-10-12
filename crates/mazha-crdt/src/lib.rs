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

    /// 当前文本的 **UTF-8 字节** 长度（yrs 原生单位）。
    pub fn byte_len(&self) -> u32 {
        let txn = self.doc.transact();
        self.root.len(&txn)
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
