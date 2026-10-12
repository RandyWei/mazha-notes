//! # mazha-store
//!
//! 存储抽象层（SPEC §5、§6.2）：`StorageProvider` trait 与默认的**本地文件系统**实现
//! `FileSystemProvider`。云端 / 自部署 / 用户自定义存储均以 Provider 形式接入。
//!
//! **本 crate 不得依赖任何 GUI**（SPEC §4.1）。MVP 采用同步接口（决策 D24）。

mod error;
mod local;
mod path;
mod provider;

pub use error::{StoreError, StoreResult};
pub use local::FileSystemProvider;
pub use path::DocPath;
pub use provider::{Entry, EntryKind, StorageCaps, StorageProvider};
