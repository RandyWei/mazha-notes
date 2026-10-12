//! 存储层错误。

use crate::path::DocPath;

/// 存储操作的错误类型。
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// 路径为空。
    #[error("路径为空")]
    EmptyPath,

    /// 路径非法（绝对路径、含 `..`/`.`/反斜杠或空段）。
    #[error("路径非法：{0}")]
    InvalidPath(String),

    /// 目标不存在。
    #[error("不存在：{0}")]
    NotFound(String),

    /// 目标已存在。
    #[error("已存在：{0}")]
    AlreadyExists(String),

    /// 期望目录但路径不是目录。
    #[error("不是目录：{0}")]
    NotDirectory(String),

    /// 底层 IO 错误。
    #[error("IO 错误：{0}")]
    Io(#[from] std::io::Error),
}

impl StoreError {
    /// 把 `std::io::Error` 连同路径上下文映射为 [`StoreError`]。
    pub(crate) fn from_io(err: std::io::Error, path: &DocPath) -> Self {
        match err.kind() {
            std::io::ErrorKind::NotFound => StoreError::NotFound(path.to_string()),
            std::io::ErrorKind::AlreadyExists => StoreError::AlreadyExists(path.to_string()),
            _ => StoreError::Io(err),
        }
    }
}

/// 本模块的 `Result` 别名。
pub type StoreResult<T> = std::result::Result<T, StoreError>;
