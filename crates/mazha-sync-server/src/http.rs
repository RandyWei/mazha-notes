//! 同步 HTTP 适配层（D100，契约见 `docs/SYNC-SPEC.md` §7.4.3）。
//!
//! `handle_request` 是纯映射函数：HTTP 头/体 → `SyncService` 调用 → wire 字节响应，
//! 便于不启监听端口也能做契约测试。`run_server` 用 `tiny_http` 把它挂到真实 socket，
//! 保持阻塞式同步风格（D24），不引入 async runtime；TLS 由反向代理终结。

use std::net::TcpListener;

use mazha_sync::SyncErrorCode;
use mazha_sync::SyncMessage;
use mazha_sync::TransportError;
use mazha_sync::wire::{WireError, decode_message, decode_snapshot_request, encode_message};

use crate::service::{SyncService, error_payload};
use crate::store::SyncStore;

/// 同步帧的 Content-Type；`batch_id`/游标/身份只在 wire 信封内，不放 HTTP header。
pub const CONTENT_TYPE: &str = "application/x-mazha-sync+wire";

/// 与传输无关的 HTTP 请求视图：由具体 HTTP 库填充，再交给 `handle_request`。
#[derive(Debug, Clone, Copy)]
pub struct HttpRequest<'a> {
    /// HTTP 方法，只接受 `POST`。
    pub method: &'a str,
    /// 请求路径，四个固定端点之一。
    pub path: &'a str,
    /// `Authorization` 头原文（`Bearer <token>`）。
    pub authorization: Option<&'a str>,
    /// `Content-Type` 头原文。
    pub content_type: Option<&'a str>,
    /// 请求体：完整的 wire 帧字节。
    pub body: &'a [u8],
}

/// 最小化 HTTP 响应：状态码 + wire 字节体（Content-Type 恒为 [`CONTENT_TYPE`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    /// HTTP 状态码。
    pub status: u16,
    /// 响应体（成功为对应应答消息，失败为 `SyncMessage::Error`）。
    pub body: Vec<u8>,
}

/// 把协议错误码映射为 HTTP 状态码（§7.4.3）。
#[must_use]
pub const fn status_for(code: SyncErrorCode) -> u16 {
    match code {
        SyncErrorCode::InvalidRequest | SyncErrorCode::BadFrame | SyncErrorCode::BadMetadata => 400,
        SyncErrorCode::Unauthorized => 401,
        SyncErrorCode::CursorTooOld | SyncErrorCode::SnapshotRequired => 409,
        SyncErrorCode::Internal => 500,
    }
}

/// 构造错误响应：错误码决定状态码，响应体统一是 wire `Error` 消息。
fn error_response(error: &TransportError) -> HttpResponse {
    let payload = error_payload(error);
    let status = status_for(payload.code);
    // 错误载荷只含小字段，编码不会超过帧上限；保留兜底避免 panic。
    let body = encode_message(&SyncMessage::Error(payload)).unwrap_or_default();
    HttpResponse { status, body }
}

/// wire 解码失败统一映射为 400 / BadFrame，避免把字节错误泄漏成 500。
fn wire_error_response(error: &WireError) -> HttpResponse {
    error_response(&TransportError::server(
        SyncErrorCode::BadFrame,
        format!("wire 解码失败：{error}"),
    ))
}

/// 固定错误响应：路径/方法/Content-Type 不符时返回 400 InvalidRequest。
fn invalid_request(message: &str) -> HttpResponse {
    error_response(&TransportError::server(
        SyncErrorCode::InvalidRequest,
        message,
    ))
}

/// 编码成功应答；编码失败属于服务端内部问题（帧超大等）。
fn ok_response(message: &SyncMessage) -> HttpResponse {
    match encode_message(message) {
        Ok(body) => HttpResponse { status: 200, body },
        Err(error) => error_response(&TransportError::server(
            SyncErrorCode::Internal,
            format!("应答编码失败：{error}"),
        )),
    }
}

/// 纯函数式 HTTP 分发：不依赖监听 socket，便于契约测试。
///
/// 四个端点全部为 `POST /sync/v0/{hello,pull,push,snapshot}`；请求体是 wire 帧，
/// 只有 `Authorization: Bearer` 一个额外 header（见 §7.4.3）。
pub fn handle_request<S: SyncStore>(
    service: &SyncService<S>,
    request: &HttpRequest<'_>,
) -> HttpResponse {
    if request.method != "POST" {
        return invalid_request("同步端点只接受 POST");
    }
    if request.content_type != Some(CONTENT_TYPE) {
        return invalid_request("Content-Type 必须是 application/x-mazha-sync+wire");
    }
    let token = request.authorization.and_then(crate::auth::bearer_token);
    match request.path {
        "/sync/v0/hello" => match decode_message(request.body) {
            Ok(SyncMessage::Hello(req)) => match service.hello(&req, token) {
                Ok(ack) => ok_response(&SyncMessage::HelloAck(ack)),
                Err(error) => error_response(&error),
            },
            Ok(_) => invalid_request("hello 端点收到其它消息种类"),
            Err(error) => wire_error_response(&error),
        },
        "/sync/v0/pull" => match decode_message(request.body) {
            Ok(SyncMessage::Pull(req)) => match service.pull(&req, token) {
                Ok(response) => ok_response(&SyncMessage::PullResponse(response)),
                Err(error) => error_response(&error),
            },
            Ok(_) => invalid_request("pull 端点收到其它消息种类"),
            Err(error) => wire_error_response(&error),
        },
        "/sync/v0/push" => match decode_message(request.body) {
            Ok(SyncMessage::Push(req)) => match service.push(&req, token) {
                Ok(ack) => ok_response(&SyncMessage::PushAck(ack)),
                Err(error) => error_response(&error),
            },
            Ok(_) => invalid_request("push 端点收到其它消息种类"),
            Err(error) => wire_error_response(&error),
        },
        "/sync/v0/snapshot" => match decode_snapshot_request(request.body) {
            Ok(req) => match service.snapshot_request(&req, token) {
                Ok(payload) => ok_response(&SyncMessage::Snapshot(payload)),
                Err(error) => error_response(&error),
            },
            Err(error) => wire_error_response(&error),
        },
        _ => invalid_request("未知的同步端点"),
    }
}

/// 在给定 listener 上阻塞服务（测试可用 `127.0.0.1:0` 的临时端口）。
///
/// # Errors
/// 创建 `tiny_http::Server`、读取请求体或写回响应失败时返回错误字符串。
///
/// # Panics
/// 固定的 `Content-Type` 头是合法 ASCII，构造失败属于实现 bug，直接 panic 以便尽早暴露。
pub fn serve<S: SyncStore>(service: &SyncService<S>, listener: TcpListener) -> Result<(), String> {
    // 复用调用方已绑定的 listener：测试可先取 local_addr() 再起线程，端口不竞争。
    let server = tiny_http::Server::from_listener(listener, None)
        .map_err(|error| format!("启动 HTTP 服务失败：{error}"))?;
    for mut incoming in server.incoming_requests() {
        let method = incoming.method().to_string();
        let path = incoming.url().to_owned();
        let authorization = incoming
            .headers()
            .iter()
            .find(|header| header.field.equiv("Authorization"))
            .map(|header| header.value.as_str().to_owned());
        let content_type = incoming
            .headers()
            .iter()
            .find(|header| header.field.equiv("Content-Type"))
            .map(|header| header.value.as_str().to_owned());

        let mut body = Vec::new();
        if let Err(error) = incoming.as_reader().read_to_end(&mut body) {
            // 读不出完整请求体时无法给出可信的 wire 应答，直接关掉该连接。
            eprintln!("读取同步请求体失败：{error}");
            continue;
        }
        let response = handle_request(
            service,
            &HttpRequest {
                method: &method,
                path: &path,
                authorization: authorization.as_deref(),
                content_type: content_type.as_deref(),
                body: &body,
            },
        );
        let header = tiny_http::Header::from_bytes(&b"Content-Type"[..], CONTENT_TYPE.as_bytes())
            .expect("固定 Content-Type 是合法 ASCII");
        let outgoing = tiny_http::Response::from_data(response.body)
            .with_status_code(tiny_http::StatusCode(response.status))
            .with_header(header);
        if let Err(error) = incoming.respond(outgoing) {
            // 客户端可能已断开；服务端状态未因此改变，继续处理下一请求。
            eprintln!("写回同步响应失败：{error}");
        }
    }
    Ok(())
}

/// 绑定地址并阻塞服务；生产部署请在前面放反向代理终结 TLS。
///
/// # Errors
/// 地址绑定失败或 `serve` 出错时返回错误字符串。
pub fn run_server<S: SyncStore>(service: &SyncService<S>, addr: &str) -> Result<(), String> {
    let listener = TcpListener::bind(addr).map_err(|error| format!("绑定 {addr} 失败：{error}"))?;
    serve(service, listener)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use mazha_sync::{HelloPayload, SyncCapability};

    use super::*;
    use crate::auth::{TokenAuth, TokenRule};
    use crate::store::FileSyncStore;

    fn test_service(root: &PathBuf) -> SyncService<FileSyncStore> {
        SyncService::new(
            FileSyncStore::new(root),
            TokenAuth::new([TokenRule {
                token: "token-a".to_owned(),
                workspaces: vec![mazha_sync::WorkspaceId::new("ws-1").unwrap()],
            }]),
        )
    }

    fn hello_bytes() -> Vec<u8> {
        encode_message(&SyncMessage::Hello(mazha_sync::SyncEnvelope::new(
            mazha_sync::WorkspaceId::new("ws-1").unwrap(),
            mazha_sync::DeviceId::new("dev-a").unwrap(),
            HelloPayload {
                capabilities: vec![SyncCapability::ContentV1],
            },
        )))
        .unwrap()
    }

    #[test]
    fn http_mapping_rejects_wrong_method_and_content_type() {
        let service = test_service(&PathBuf::from("unused"));
        let body = hello_bytes();
        let wrong_method = handle_request(
            &service,
            &HttpRequest {
                method: "GET",
                path: "/sync/v0/hello",
                authorization: Some("Bearer token-a"),
                content_type: Some(CONTENT_TYPE),
                body: &body,
            },
        );
        assert_eq!(wrong_method.status, 400);
        let wrong_type = handle_request(
            &service,
            &HttpRequest {
                method: "POST",
                path: "/sync/v0/hello",
                authorization: Some("Bearer token-a"),
                content_type: Some("application/json"),
                body: &body,
            },
        );
        assert_eq!(wrong_type.status, 400);
    }

    #[test]
    fn http_mapping_hello_returns_ack_and_unauthorized_returns_401() {
        let root = std::env::temp_dir().join(format!(
            "mazha-http-mapping-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&root);
        let service = test_service(&root);
        let body = hello_bytes();
        let ok = handle_request(
            &service,
            &HttpRequest {
                method: "POST",
                path: "/sync/v0/hello",
                authorization: Some("Bearer token-a"),
                content_type: Some(CONTENT_TYPE),
                body: &body,
            },
        );
        assert_eq!(ok.status, 200);
        assert!(matches!(
            decode_message(&ok.body),
            Ok(SyncMessage::HelloAck(_))
        ));
        let denied = handle_request(
            &service,
            &HttpRequest {
                method: "POST",
                path: "/sync/v0/hello",
                authorization: Some("Bearer wrong"),
                content_type: Some(CONTENT_TYPE),
                body: &body,
            },
        );
        assert_eq!(denied.status, 401);
        let _ = std::fs::remove_dir_all(&root);
    }
}
