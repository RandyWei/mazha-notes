//! CRDT 层错误类型。

/// CRDT 层结果别名。
pub type CrdtResult<T> = std::result::Result<T, CrdtError>;

/// CRDT 操作错误。
#[derive(Debug, thiserror::Error)]
pub enum CrdtError {
    /// 字符索引越界（索引超过当前字符数）。
    #[error("字符索引越界：index={index}, 文本字符数={len}")]
    OutOfBounds {
        /// 请求的字符索引。
        index: usize,
        /// 当前文本字符数（允许的最大索引）。
        len: usize,
    },

    /// Yjs 更新/状态向量解码失败。
    #[error("CRDT 编解码失败：{0}")]
    Decode(#[from] yrs::encoding::read::Error),

    /// 应用更新帧失败。
    #[error("CRDT 更新应用失败：{0}")]
    Apply(#[from] yrs::error::UpdateError),

    /// 文本规模超出 yrs 的 `u32` 索引范围。
    #[error("文本过大，超出 yrs u32 索引范围：{0}")]
    TooLarge(usize),
}
