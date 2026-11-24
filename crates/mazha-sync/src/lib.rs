//! # mazha-sync
//!
//! Phase 5 S2 的同步会话层（D99，契约见 `docs/SYNC-SPEC.md` §5.7/§6.4）：
//! 内存协议模型（消息/信封/类型化 ID/游标/批次）、同步请求-响应 [`SyncTransport`] trait、
//! 单线程 in-memory 服务端与假 transport、以及客户端 [`SyncSession`] 状态机。
//!
//! 设计边界：
//! - **纯 Rust**：无网络 IO、无 GUI、无 async runtime；只用 `Rc<RefCell<...>>` 的
//!   假服务端跑通“两端 + 一服务端”协议测试。
//! - **内容帧仍是标准 Yjs v1**：复用 `mazha_crdt` 的 `BTreeMap<DocId, Vec<u8>>`；
//!   墓碑/upsert 等元数据用显式的 `MetaChange`/`RosterMeta`，不伪装进 Yjs 文本。
//! - **同步路径只走 `apply_updates_with_meta`**：坏帧/坏元数据整批拒绝零改动；
//!   不查墓碑的旧 `apply_updates` 不用于同步。
//! - **S3 字节编码**：`wire` 模块提供确定性帧编解码，仍然不引入网络/IO/async；
//!   真实 HTTP/WebSocket、认证与持久化服务端在独立上层 crate；本地/桌面集成留 S4/S5。

mod fake;
mod protocol;
mod session;
mod transport;
pub mod wire;

pub use fake::{FakeTransport, InMemorySyncServer};
pub use protocol::{
    AckPayload, BatchId, DEFAULT_MAX_FRAME_BYTES, DeviceId, DocFrameMap, HelloAck, HelloPayload,
    HelloRequest, PROTOCOL_VERSION, ProtocolError, PullPayload, PullRequest, PullResponse, PushAck,
    PushPayload, PushRequest, SnapshotPayload, SyncCapability, SyncCursor, SyncEnvelope,
    SyncErrorCode, SyncErrorPayload, SyncMessage, SyncMessageKind, WorkspaceId, error_code_for,
};
pub use session::{
    HandshakeOutcome, PendingBatch, PullOutcome, PushOutcome, SnapshotOutcome, SyncOnceOutcome,
    SyncSession, SyncSessionError,
};
pub use transport::{SyncTransport, TransportError};
