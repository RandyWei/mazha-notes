//! 同步变更订阅（Phase 5 S5b 第三批 / D108，契约见 `docs/SYNC-SPEC.md` §5.13）。
//!
//! 本 crate 是 **additive 旁路**：只负责“服务端有变更”这一轻量通知，
//! 从不参与 `SyncTransport` 的九种消息、`MZS0` wire 或 `LocalSyncEngine` 的合并决策。
//! 收到事件后由调用方自行决定发一轮非破坏性的 `SyncNow`。
//!
//! 设计约束：
//! - 同步阻塞、无 async runtime；客户端用 `std::net::TcpStream`，服务端复用 tiny_http 的 upgrade。
//! - 握手校验、URL 推导、帧编解码、事件 JSON 全部是纯函数，可在无 socket 环境下单测。
//! - 核心 crate 不依赖 GUI；本 crate 只依赖标准库与小型编解码库。

/// 轻量订阅事件与 JSON 编解码。
pub mod event;
/// 最小 RFC6455 帧解析/编码（text/分片/ping/pong/close/坏帧拒绝）。
pub mod frame;
/// RFC6455 握手与 URL 推导的纯函数。
pub mod handshake;
/// 可替换的订阅 trait 与内存假实现。
pub mod subscription;
/// 阻塞式 TCP 客户端订阅（不连真实端口也能通过上述纯函数验证协议）。
pub mod tcp;

pub use event::{SyncEvent, decode_event_json, encode_event_json};
pub use frame::{WsFrame, decode_ws_frame, encode_ws_frame};
pub use handshake::{
    SUBSCRIBE_PATH, UpgradeRequest, WsEndpoint, accept_key, parse_ws_endpoint, subscription_url,
    validate_upgrade, workspace_from_subscribe_path,
};
pub use subscription::{
    InMemorySubscription, SubscriptionConfig, SubscriptionError, SyncSubscription,
};
pub use tcp::{TcpSubscription, websocket_key};
