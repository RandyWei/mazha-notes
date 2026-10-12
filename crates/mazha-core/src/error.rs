//! 错误类型：编辑操作失败的原因。
//!
//! 所有偏移量都是对源文本的 **UTF-8 字节偏移**，且必须落在字符边界上。
//! 非法偏移用本模块的错误类型表达，而不是 panic。

use std::result::Result as StdResult;

/// 编辑操作失败的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    /// 偏移量落在多字节字符的中间，不是一个合法的 UTF-8 字符边界。
    ///
    /// 例如对中文「你」（3 字节）插入，`offset = 1` 就会触发此错误。
    #[error("偏移量 {offset} 不是字符边界")]
    InvalidBoundary {
        /// 非法的字节偏移量。
        offset: usize,
    },

    /// 编辑范围不合法：`start > end`，或越出文档长度。
    #[error("范围 {start}..{end} 不合法（文档长度 {doc_len}）")]
    InvalidRange {
        /// 范围起点（字节偏移）。
        start: usize,
        /// 范围终点（字节偏移）。
        end: usize,
        /// 文档当前字节长度。
        doc_len: usize,
    },
}

/// 本 crate 统一的 `Result` 别名。
pub type Result<T> = StdResult<T, EditError>;
