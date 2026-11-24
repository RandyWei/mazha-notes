//! # mazha-sync-server
//!
//! Phase 5 S3 的最小同步服务端（D100，契约见 `docs/SYNC-SPEC.md` §7.4）：
//! 工作区持久化（`SyncStore`/`FileSyncStore`）、最小鉴权（`TokenAuth`）、
//! 与传输无关的同步服务（`SyncService`）以及同步 HTTP 路由（`http`）。
//!
//! 设计边界：
//! - 服务端是**带持久化的同步中继**：校验、去重、记日志、落盘、鉴权；
//!   不理解 Markdown 语义、不改写用户文本（§7.3）。
//! - push 在克隆状态上复用 `CrdtRoster::apply_updates_with_meta` 校验，
//!   失败时游标、日志、去重表与磁盘文件零改动。
//! - 沿用 D24 的同步风格：阻塞式 HTTP（`tiny_http`），**不引入 async runtime**；
//!   TLS 由反向代理终结，代码不绑定 HTTPS。

pub mod auth;
pub mod http;
pub mod service;
pub mod store;

pub use auth::{AuthError, TokenAuth, TokenRule, bearer_token};
pub use http::{
    CONTENT_TYPE, HttpRequest, HttpResponse, handle_request, run_server, serve, status_for,
};
pub use service::{SyncService, error_payload};
pub use store::{FileSyncStore, LogEntry, StoreError, SyncStore, WorkspaceState};
