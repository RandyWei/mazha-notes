//! # mazha-sync-local
//!
//! Phase 5 S4 的本地同步编排（D101，契约见 `docs/SYNC-SPEC.md` §5.9）：把
//! [`mazha_sync::SyncSession`] 接到工作区 `.md` 文件监听、名册帧与墓碑持久化。
//!
//! 设计边界：
//! - **纯 Rust 同步阻塞**：不依赖 `mazha-core`、GUI、网络实现或 async runtime；
//!   传输由调用方注入 `&mut dyn SyncTransport`（FakeTransport / HttpTransport 均可），
//!   因此不反向依赖 `mazha-sync-server` / `mazha-sync-http`。
//! - **事件只标脏**：`handle_watch_events` 不读盘；`drain_dirty` 才读文件并暂存，
//!   `sync_once` 统一做暂存 → 同步 → 写回 → 落盘，避免一个窗口内多次写产生多批次。
//! - **回声抑制两层**：`write_marks` 字节比对（命中即消费）+ 名册文本再比对。
//! - **状态不静默回滚**：`.mazha/sync-state.bin` 用 `MZLS`/版本/CRC32 持久化游标、
//!   `op_clock`、`batch_counter` 与 outbox；坏文件返回错误而非空状态。
//! - **冲突可见**：快照确认门槛与墓碑冲突副本通过 `LocalSyncStatus` 暴露，交由 S5 UI 处理。

mod engine;
mod error;
mod mapping;
mod resolve;
mod state;

pub use engine::{LocalSyncEngine, LocalSyncOutcome, LocalSyncStatus};
pub use error::LocalSyncError;
pub use mapping::{
    LocalWriteAction, ObservedBytes, doc_id_from_path, is_syncable_md, local_text_to_frames,
    merge_watch_events, remote_frames_to_local_writes, should_ignore_echo,
};
pub use resolve::{
    ConflictChoice, ResolveOutcome, SnapshotChoice, conflict_copy_text, merged_conflict_text,
};
pub use state::{LocalSyncState, LocalSyncStore};
