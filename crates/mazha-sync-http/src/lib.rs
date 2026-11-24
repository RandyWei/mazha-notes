//! # mazha-sync-http
//!
//! Phase 5 S3 的真实 HTTP client transport（D100，契约见 `docs/SYNC-SPEC.md` §7.4.3）：
//! 用同步 HTTP（`ureq`）把 [`SyncTransport`] 的四个方法映射到
//! `POST /sync/v0/{hello,pull,push,snapshot}`，请求/响应体都是 wire 帧字节。
//!
//! 设计边界：
//! - 不引入 async runtime，沿用 D24 同步风格；网络错误映射为
//!   [`TransportError::Disconnected`]，让 `SyncSession` 保留 outbox 等待重放。
//! - `batch_id`、游标、身份只出现在 wire 信封内；HTTP header 仅承载
//!   `Content-Type` 与 `Authorization: Bearer`。
//! - TLS 由反向代理终结；base_url 可以传 `http://127.0.0.1:port` 做本地测试。

use std::io::Read;

use mazha_sync::wire::WireError;
use mazha_sync::{
    DeviceId, HelloAck, HelloRequest, PullRequest, PullResponse, PushAck, PushRequest,
    SnapshotPayload, SyncEnvelope, SyncMessage, SyncMessageKind, SyncTransport, TransportError,
    WorkspaceId,
    wire::{decode_message, encode_message, encode_snapshot_request},
};

/// HTTP 传输错误沿用 `mazha-sync` 的分层错误，避免第二套错误模型。
pub type HttpTransportError = TransportError;

/// 同步帧的 Content-Type（与服务端一致）。
const CONTENT_TYPE: &str = "application/x-mazha-sync+wire";

/// 基于 ureq 的同步 HTTP 传输实现。
#[derive(Debug, Clone)]
pub struct HttpTransport {
    /// 服务端基础 URL（不含尾部 `/`）。
    base_url: String,
    /// 绑定的工作区（供无参 `snapshot()` 携带身份）。
    workspace: WorkspaceId,
    /// 绑定的设备（供无参 `snapshot()` 携带身份）。
    device: DeviceId,
    /// Bearer 凭证原文。
    token: String,
}

impl HttpTransport {
    /// 构造 HTTP 传输；`base_url` 尾部斜杠会被去掉，避免拼出双斜杠路径。
    #[must_use]
    pub fn new(
        base_url: impl Into<String>,
        workspace: WorkspaceId,
        device: DeviceId,
        token: impl Into<String>,
    ) -> Self {
        let base_url = base_url.into();
        let base_url = base_url.trim_end_matches('/').to_owned();
        Self {
            base_url,
            workspace,
            device,
            token: token.into(),
        }
    }

    /// 绑定的工作区 ID（诊断/测试用）。
    #[must_use]
    pub const fn workspace(&self) -> &WorkspaceId {
        &self.workspace
    }

    /// 发送一个 wire 帧并读取应答字节。
    fn send(&self, path: &str, body: &[u8]) -> Result<Vec<u8>, TransportError> {
        let url = format!("{}{path}", self.base_url);
        // 凭证只在 HTTP header 出现一次；wire 信封内不放 token。
        let authorization = format!("Bearer {}", self.token);
        let response = ureq::post(&url)
            .set("Content-Type", CONTENT_TYPE)
            .set("Authorization", &authorization)
            .send_bytes(body);
        match response {
            Ok(response) => read_body(response).map_err(|error| {
                TransportError::disconnected(format!("读取 HTTP 应答失败：{error}"))
            }),
            Err(ureq::Error::Status(status, response)) => {
                let bytes = read_body(response).unwrap_or_default();
                match decode_message(&bytes) {
                    Ok(SyncMessage::Error(payload)) => Err(TransportError::Server {
                        code: payload.code,
                        message: payload.message,
                    }),
                    // 网关返回的非 wire 错误体（如纯文本 502）按断线处理，outbox 保留。
                    Ok(_) | Err(_) => Err(TransportError::invalid_response(format!(
                        "服务端返回 HTTP {status} 但错误体不是 wire Error"
                    ))),
                }
            }
            Err(ureq::Error::Transport(error)) => {
                Err(TransportError::disconnected(error.to_string()))
            }
        }
    }

    /// 解码应答并校验消息种类，避免把 HelloAck 当成 PullResponse 使用。
    fn expect(
        message: SyncMessage,
        expected: SyncMessageKind,
    ) -> Result<SyncMessage, TransportError> {
        if message.kind() == expected {
            Ok(message)
        } else {
            Err(TransportError::invalid_response(format!(
                "期望 {expected:?} 应答，实际是 {:?}",
                message.kind()
            )))
        }
    }
}

/// 读取 ureq 应答体到字节缓冲。
fn read_body(response: ureq::Response) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    response.into_reader().read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// wire 编解码错误映射到分层错误：这是“应答不合法”，不是 CRDT 错误。
fn wire_error(error: &WireError) -> TransportError {
    TransportError::invalid_response(format!("wire 编解码失败：{error}"))
}

impl SyncTransport for HttpTransport {
    fn hello(&mut self, request: HelloRequest) -> Result<HelloAck, TransportError> {
        let body =
            encode_message(&SyncMessage::Hello(request)).map_err(|error| wire_error(&error))?;
        let response = self.send("/sync/v0/hello", &body)?;
        let message = decode_message(&response).map_err(|error| wire_error(&error))?;
        match Self::expect(message, SyncMessageKind::HelloAck)? {
            SyncMessage::HelloAck(ack) => Ok(ack),
            _ => unreachable!("expect 已校验消息种类"),
        }
    }

    fn pull(&mut self, request: PullRequest) -> Result<PullResponse, TransportError> {
        let body =
            encode_message(&SyncMessage::Pull(request)).map_err(|error| wire_error(&error))?;
        let response = self.send("/sync/v0/pull", &body)?;
        let message = decode_message(&response).map_err(|error| wire_error(&error))?;
        match Self::expect(message, SyncMessageKind::PullResponse)? {
            SyncMessage::PullResponse(response) => Ok(response),
            _ => unreachable!("expect 已校验消息种类"),
        }
    }

    fn push(&mut self, request: PushRequest) -> Result<PushAck, TransportError> {
        let body =
            encode_message(&SyncMessage::Push(request)).map_err(|error| wire_error(&error))?;
        let response = self.send("/sync/v0/push", &body)?;
        let message = decode_message(&response).map_err(|error| wire_error(&error))?;
        match Self::expect(message, SyncMessageKind::PushAck)? {
            SyncMessage::PushAck(ack) => Ok(ack),
            _ => unreachable!("expect 已校验消息种类"),
        }
    }

    fn snapshot(&mut self) -> Result<SnapshotPayload, TransportError> {
        // trait 签名无参数，用构造时绑定的 workspace/device 组装方向 0 的快照请求。
        let request = SyncEnvelope::new(self.workspace.clone(), self.device.clone(), ());
        let body = encode_snapshot_request(&request).map_err(|error| wire_error(&error))?;
        let response = self.send("/sync/v0/snapshot", &body)?;
        let message = decode_message(&response).map_err(|error| wire_error(&error))?;
        match Self::expect(message, SyncMessageKind::Snapshot)? {
            SyncMessage::Snapshot(payload) => Ok(payload),
            _ => unreachable!("expect 已校验消息种类"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_trims_trailing_slashes() {
        let transport = HttpTransport::new(
            "http://127.0.0.1:8787///",
            WorkspaceId::new("工作区甲").unwrap(),
            DeviceId::new("设备乙").unwrap(),
            "token",
        );
        assert_eq!(transport.base_url, "http://127.0.0.1:8787");
        assert_eq!(transport.workspace().as_str(), "工作区甲");
    }
}
