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

    /// 文档 ID 非法：必须非空且不含控制字符。
    #[error("文档 ID 非法（必须非空且不含控制字符）：{0}")]
    InvalidDocumentId(String),

    /// 创建文档时 ID 已被占用。
    #[error("文档已存在：{0}")]
    DocumentAlreadyExists(String),

    /// 操作目标文档不存在。
    #[error("文档不存在：{0}")]
    DocumentNotFound(String),

    /// 操作 ID 非法：设备标识必须非空且不含控制字符（S1/D98）。
    #[error("操作 ID 非法（设备标识必须非空且不含控制字符）：{0}")]
    InvalidOpId(String),

    /// 冲突副本帧非法：墓碑保存的可恢复 Yjs v1 帧无法解码（S1/D98）。
    #[error("冲突副本帧非法（文档 {doc_id}）：{source}")]
    InvalidConflictFrame {
        /// 所属文档 ID，便于定位坏人元数据。
        doc_id: String,
        /// yrs v1 解码错误。
        #[source]
        source: yrs::encoding::read::Error,
    },

    /// 元数据帧非法：upsert 携带的 Yjs v1 内容帧无法解码（S1/D98）。
    #[error("元数据帧非法（文档 {doc_id}）：{source}")]
    InvalidMetaFrame {
        /// 所属文档 ID，便于定位坏元数据。
        doc_id: String,
        /// yrs v1 解码错误。
        #[source]
        source: yrs::encoding::read::Error,
    },

    /// 墓碑冲突：试图把不同文档的墓碑归并到一起（S1/D98）。
    #[error("墓碑冲突：doc_id 不一致（{0}）")]
    TombstoneConflict(String),
}
