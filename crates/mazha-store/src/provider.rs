//! 存储抽象：`StorageProvider` trait 与相关类型（SPEC §6.2）。

use crate::error::StoreResult;
use crate::path::DocPath;
use std::time::SystemTime;

/// 存储 Provider 能力描述。
#[allow(clippy::struct_excessive_bools)] // 能力开关用独立 bool 最直观，语义清晰
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StorageCaps {
    /// 是否支持写入（只读 Provider 为 `false`）。
    pub writable: bool,
    /// 是否支持流式读写。
    pub streaming: bool,
    /// 是否支持版本历史。
    pub versioning: bool,
    /// 是否支持协作同步。
    pub collaboration: bool,
}

/// 目录项类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// 文件。
    File,
    /// 目录。
    Dir,
}

/// 目录中的一个条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// 条目相对工作区根的路径。
    pub path: DocPath,
    /// 文件/目录名（不含路径）。
    pub name: String,
    /// 条目类型。
    pub kind: EntryKind,
    /// 大小（字节；目录为 0）。
    pub size: u64,
    /// 最后修改时间（若可得）。
    pub modified: Option<SystemTime>,
}

/// 存储提供方抽象。
///
/// 所有路径均为工作区相对的 [`DocPath`]。
///
/// > MVP 采用**同步**接口（决策 D24，SPEC §14）：本地文件系统是唯一 Provider，
/// > 同步 IO 已足够且免去异步运行时依赖；云端/插件 Provider 在 Phase 5 接入时可
/// > 在内部用线程池/异步运行时，或届时扩展为异步 trait。
pub trait StorageProvider: Send + Sync {
    /// Provider 标识（如 `"local"`）。
    fn id(&self) -> &'static str;

    /// 能力描述。
    fn capabilities(&self) -> StorageCaps;

    /// 列出目录下的条目（不递归）。
    ///
    /// # Errors
    /// `dir` 不存在或不是目录，或底层 IO 失败时返回 [`crate::StoreError`]。
    fn list(&self, dir: &DocPath) -> StoreResult<Vec<Entry>>;

    /// 读取文件全部字节。
    ///
    /// # Errors
    /// 文件不存在或读取失败时返回 [`crate::StoreError`]。
    fn read(&self, path: &DocPath) -> StoreResult<Vec<u8>>;

    /// 写入文件（覆盖；自动创建父目录）。
    ///
    /// # Errors
    /// 创建目录或写入失败时返回 [`crate::StoreError`]。
    fn write(&self, path: &DocPath, data: &[u8]) -> StoreResult<()>;

    /// 重命名/移动。
    ///
    /// # Errors
    /// 源不存在或目标父目录创建失败时返回 [`crate::StoreError`]。
    fn rename(&self, from: &DocPath, to: &DocPath) -> StoreResult<()>;

    /// 删除文件或目录（目录递归删除）。
    ///
    /// # Errors
    /// 目标不存在或删除失败时返回 [`crate::StoreError`]。
    fn remove(&self, path: &DocPath) -> StoreResult<()>;
}
