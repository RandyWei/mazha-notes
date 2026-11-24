//! 同步传输 trait 与分层错误（D99，契约见 `docs/SYNC-SPEC.md` §6.4）。
//!
//! MVP 只有四个同步请求-响应方法：没有 `subscribe`、没有事件流、没有 async runtime。
//! 传输实现可以内部持有 IO，但对会话层只暴露本模块定义的 `TransportError`，
//! 不把 `io::Error` 或 yrs 解码错误泄漏到 CRDT 层。

use crate::protocol::{
    HelloAck, HelloRequest, ProtocolError, PullRequest, PullResponse, PushAck, PushRequest,
    SnapshotPayload, SyncErrorCode,
};

/// 传输/协议层错误：与 CRDT 的 `CrdtError` 严格分层。
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// 连接不可用；会话层必须保留 outbox 等待重连重放。
    #[error("传输已断开：{reason}")]
    Disconnected {
        /// 断线原因（诊断用）。
        reason: String,
    },

    /// 服务端返回协议错误码（游标过旧/坏帧/未授权等）。
    #[error("服务端错误 {code}：{message}")]
    Server {
        /// 稳定错误码，决定重试还是快照回退。
        code: SyncErrorCode,
        /// 诊断信息。
        message: String,
    },

    /// 对端应答自相矛盾（如缺少会话标识、游标倒退）。
    #[error("应答非法：{reason}")]
    InvalidResponse {
        /// 具体原因。
        reason: String,
    },

    /// 信封/标识校验失败（下层协议错误）。
    #[error("协议错误：{0}")]
    Protocol(#[from] ProtocolError),
}

impl TransportError {
    /// 构造服务端错误（减少重复的字段字面量）。
    #[must_use]
    pub fn server(code: SyncErrorCode, message: impl Into<String>) -> Self {
        Self::Server {
            code,
            message: message.into(),
        }
    }

    /// 构造断线错误。
    #[must_use]
    pub fn disconnected(reason: impl Into<String>) -> Self {
        Self::Disconnected {
            reason: reason.into(),
        }
    }

    /// 构造非法应答错误。
    #[must_use]
    pub fn invalid_response(reason: impl Into<String>) -> Self {
        Self::InvalidResponse {
            reason: reason.into(),
        }
    }

    /// 是否为“游标过旧”（会话层据此自动回退快照）。
    #[must_use]
    pub const fn is_cursor_too_old(&self) -> bool {
        matches!(
            self,
            Self::Server {
                code: SyncErrorCode::CursorTooOld,
                ..
            }
        )
    }

    /// 取出服务端错误码（非服务端错误返回 `None`）。
    #[must_use]
    pub const fn code(&self) -> Option<SyncErrorCode> {
        match self {
            Self::Server { code, .. } => Some(*code),
            _ => None,
        }
    }
}

/// 最小同步传输：同步请求-响应，所有方法拿 `&mut self`。
///
/// 约束（D96/D99）：不提供订阅能力；实现不得要求调用方引入 async runtime；
/// 假 transport 与未来 HTTP transport 共用同一组类型，协议规则不因传输而变。
pub trait SyncTransport {
    /// 握手：登记设备并获取会话 ID 与服务端游标。
    ///
    /// # Errors
    /// 断线返回 `Disconnected`；信封/鉴权失败返回 `Protocol`/`Server`。
    fn hello(&mut self, req: HelloRequest) -> Result<HelloAck, TransportError>;

    /// 增量拉取：按 `since` 与状态向量取内容帧 + 元数据变更。
    ///
    /// # Errors
    /// 游标过旧返回 `Server(CursorTooOld)`；断线与非法应答同上。
    fn pull(&mut self, req: PullRequest) -> Result<PullResponse, TransportError>;

    /// 幂等推送：按 `(workspace, device, batch_id)` 去重。
    ///
    /// # Errors
    /// 坏帧/坏元数据返回 `Server(BadFrame|BadMetadata)`；断线同上。
    fn push(&mut self, req: PushRequest) -> Result<PushAck, TransportError>;

    /// 全量快照：新设备引导或游标过旧回退。
    ///
    /// # Errors
    /// 未授权/断线/服务端错误按分层返回。
    fn snapshot(&mut self) -> Result<SnapshotPayload, TransportError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_error_helpers_expose_cursor_too_old() {
        let old = TransportError::server(SyncErrorCode::CursorTooOld, "裁剪");
        assert!(old.is_cursor_too_old());
        assert_eq!(old.code(), Some(SyncErrorCode::CursorTooOld));
        let bad = TransportError::disconnected("离线");
        assert!(!bad.is_cursor_too_old());
        assert_eq!(bad.code(), None);
        assert!(
            TransportError::invalid_response("游标倒退")
                .to_string()
                .contains("应答非法")
        );
    }
}
