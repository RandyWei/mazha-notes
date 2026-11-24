//! RFC6455 握手与订阅 URL 的纯函数（D108）。
//!
//! 所有函数都不碰 socket：服务端用 [`validate_upgrade`] 校验请求头并计算 Accept，
//! 客户端用 [`subscription_url`] 从既有 http(s) 同步地址推导 ws(s) 订阅地址，
//! 再用 [`parse_ws_endpoint`] 拆出 TCP 连接参数。这样“不启端口也能测握手”成立。

use std::fmt::Write as _;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use sha1::{Digest, Sha1};

use crate::subscription::SubscriptionError;

/// RFC6455 固定的 magic GUID；不得修改，否则客户端校验必然失败。
const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// 订阅端点固定路径（第 5 条路由）。
pub const SUBSCRIBE_PATH: &str = "/sync/v0/subscribe";

/// 与传输无关的 Upgrade 请求视图；`tiny_http` 的 header 列表由服务端适配层填入。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpgradeRequest<'a> {
    /// HTTP 方法，必须是 GET。
    pub method: &'a str,
    /// 请求路径（可含 query）。
    pub path: &'a str,
    /// `Upgrade` 头原文。
    pub upgrade: Option<&'a str>,
    /// `Connection` 头原文（可能含多个逗号分隔的 token）。
    pub connection: Option<&'a str>,
    /// `Sec-WebSocket-Key` 头原文。
    pub key: Option<&'a str>,
    /// `Sec-WebSocket-Version` 头原文。
    pub version: Option<&'a str>,
}

/// 校验结果：只保留后续握手需要的字段，避免把完整 header 带进业务层。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidUpgrade {
    /// 客户端请求路径（含 workspace query）。
    pub path: String,
    /// 原始 Sec-WebSocket-Key（用于回算 Accept）。
    pub key: String,
}

/// 计算 `Sec-WebSocket-Accept`：`base64(SHA1(key + GUID))`。
///
/// 客户端 key 必须是标准 base64 编码的 16 字节随机值；空白/控制字符直接拒绝，
/// 防止畸形头进入 SHA1 或日志。
///
/// # Errors
/// key 为空、含控制字符或不是 16 字节 base64 时返回 [`SubscriptionError::Handshake`]。
pub fn accept_key(client_key: &str) -> Result<String, SubscriptionError> {
    if client_key.is_empty() || client_key.chars().any(char::is_control) {
        return Err(SubscriptionError::Handshake(
            "Sec-WebSocket-Key 为空或含控制字符".into(),
        ));
    }
    let decoded = BASE64.decode(client_key.trim()).map_err(|error| {
        SubscriptionError::Handshake(format!("Sec-WebSocket-Key 非法 base64：{error}"))
    })?;
    if decoded.len() != 16 {
        return Err(SubscriptionError::Handshake(format!(
            "Sec-WebSocket-Key 必须解码为 16 字节，实际 {} 字节",
            decoded.len()
        )));
    }
    let mut sha = Sha1::new();
    sha.update(client_key.trim().as_bytes());
    sha.update(WS_GUID.as_bytes());
    Ok(BASE64.encode(sha.finalize()))
}

/// 校验一次 WebSocket Upgrade 请求（D108 服务端入口的纯函数部分）。
///
/// `Connection` 允许 `keep-alive, Upgrade` 这类多值写法；`Upgrade` 只允许 websocket。
///
/// # Errors
/// 方法/路径/头不满足 RFC6455 最小子集时返回 [`SubscriptionError::Handshake`]。
pub fn validate_upgrade(request: &UpgradeRequest<'_>) -> Result<ValidUpgrade, SubscriptionError> {
    let reject = |reason: String| SubscriptionError::Handshake(reason);
    if request.method != "GET" {
        return Err(reject("订阅端点只接受 GET Upgrade".into()));
    }
    if request.path.split('?').next().unwrap_or_default() != SUBSCRIBE_PATH {
        return Err(reject(format!("订阅路径必须是 {SUBSCRIBE_PATH}")));
    }
    if !request
        .upgrade
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("websocket"))
    {
        return Err(reject("Upgrade 头必须是 websocket".into()));
    }
    let has_upgrade = request.connection.is_some_and(|value| {
        value
            .split(',')
            .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
    });
    if !has_upgrade {
        return Err(reject("Connection 头必须包含 Upgrade".into()));
    }
    if request.version.is_none_or(|v| v.trim() != "13") {
        return Err(reject("Sec-WebSocket-Version 必须是 13".into()));
    }
    let key = request
        .key
        .ok_or_else(|| reject("缺少 Sec-WebSocket-Key".into()))?;
    accept_key(key)?;
    Ok(ValidUpgrade {
        path: request.path.to_owned(),
        key: key.trim().to_owned(),
    })
}

/// 从既有同步根 URL 推导订阅 URL：`http→ws`、`https→wss` + 固定路径。
///
/// 原 URL 可带路径前缀（反向代理场景），保留前缀再追加 `/sync/v0/subscribe`；
/// 控制字符、空白、缺 host 一律拒绝，绝不拼接出非法 URL。
///
/// # Errors
/// 非 http(s) 或格式非法时返回 [`SubscriptionError::InvalidUrl`]。
pub fn subscription_url(server_url: &str) -> Result<String, SubscriptionError> {
    let invalid = |reason: String| SubscriptionError::InvalidUrl(reason);
    if server_url.chars().any(char::is_control) {
        return Err(invalid("同步地址含控制字符".into()));
    }
    let url = server_url.trim().trim_end_matches('/');
    let (scheme, rest) = if let Some(rest) = url.strip_prefix("https://") {
        ("wss", rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        ("ws", rest)
    } else {
        return Err(invalid("同步地址只支持 http:// 或 https://".into()));
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if host.is_empty() || url.chars().any(char::is_whitespace) {
        return Err(invalid("同步地址缺少 host 或含空白".into()));
    }
    Ok(format!("{scheme}://{rest}{SUBSCRIBE_PATH}"))
}

/// 拆分后的订阅连接参数（纯数据，不含 token）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WsEndpoint {
    /// TCP 连接主机名（IPv6 字面量也保留原样）。
    pub host: String,
    /// TCP 端口；URL 未写时按 scheme 取 80/443。
    pub port: u16,
    /// 请求路径（不含 query）。
    pub path: String,
    /// 是否走 TLS（由反向代理终结，本 crate 仅记录）。
    pub secure: bool,
}

/// 解析 `ws://` / `wss://` 订阅 URL 为连接参数。
///
/// 不解析 userinfo/fragment：这类写法对订阅无意义，直接拒绝以免歧义。
///
/// # Errors
/// scheme 非法、缺 host、端口越界或含控制字符时返回 [`SubscriptionError::InvalidUrl`]。
pub fn parse_ws_endpoint(url: &str) -> Result<WsEndpoint, SubscriptionError> {
    let invalid = |reason: &str| SubscriptionError::InvalidUrl(reason.to_owned());
    if url.chars().any(char::is_control) {
        return Err(invalid("订阅地址含控制字符"));
    }
    let (scheme, rest) = if let Some(rest) = url.strip_prefix("wss://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("ws://") {
        (false, rest)
    } else {
        return Err(invalid("订阅地址只支持 ws:// 或 wss://"));
    };
    let authority_and_path = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority_and_path.is_empty() {
        return Err(invalid("订阅地址缺少 host"));
    }
    if authority_and_path.contains('@') {
        return Err(invalid("订阅地址不允许 userinfo"));
    }
    let (host, port) = match authority_and_path.rsplit_once(':') {
        // IPv6 字面量用方括号包裹，冒号属于地址本身而不是端口分隔符。
        Some((h, p)) if h.starts_with('[') && h.ends_with(']') => {
            (h.to_owned(), parse_port(p, scheme)?)
        }
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => {
            (h.to_owned(), parse_port(p, scheme)?)
        }
        Some(_) => return Err(invalid("订阅地址端口非法")),
        None => (authority_and_path.to_owned(), if scheme { 443 } else { 80 }),
    };
    if host.is_empty() || host.chars().any(char::is_whitespace) {
        return Err(invalid("订阅地址 host 非法"));
    }
    let path = rest
        .strip_prefix(authority_and_path)
        .unwrap_or_default()
        .split(['?', '#'])
        .next()
        .unwrap_or_default();
    let path = if path.is_empty() {
        SUBSCRIBE_PATH.to_owned()
    } else {
        path.to_owned()
    };
    Ok(WsEndpoint {
        host: host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned(),
        port,
        path,
        secure: scheme,
    })
}

/// 端口解析：空串取 scheme 默认端口，其他必须是 1..=65535。
fn parse_port(raw: &str, secure: bool) -> Result<u16, SubscriptionError> {
    if raw.is_empty() {
        return Ok(if secure { 443 } else { 80 });
    }
    raw.parse::<u16>()
        .ok()
        .filter(|port| *port > 0)
        .ok_or_else(|| SubscriptionError::InvalidUrl(format!("订阅地址端口非法：{raw}")))
}

/// 从订阅路径的 query 中取 `workspace` 参数并解码（纯函数）。
///
/// 只接受单个 `workspace`；重复参数或空值一律拒绝，避免鉴权目标歧义。
///
/// # Errors
/// 缺少参数、重复参数、百分号编码非法或含控制字符时返回 [`SubscriptionError::InvalidUrl`]。
pub fn workspace_from_subscribe_path(path: &str) -> Result<String, SubscriptionError> {
    let invalid = |reason: &str| SubscriptionError::InvalidUrl(reason.to_owned());
    let query = path
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or_default();
    let mut workspace = None;
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        if key == "workspace" {
            if workspace.is_some() {
                return Err(invalid("订阅路径含重复 workspace 参数"));
            }
            workspace = Some(percent_decode_component(value)?);
        }
    }
    let workspace = workspace.ok_or_else(|| invalid("订阅路径缺少 workspace 参数"))?;
    if workspace.is_empty() || workspace.chars().any(char::is_control) {
        return Err(invalid("订阅 workspace 参数非法"));
    }
    Ok(workspace)
}

/// 百分号解码 query 组件；非法 `%` 序列直接拒绝（不猜恢复）。
///
/// # Errors
/// `%` 后不是两个十六进制字符或 UTF-8 非法时返回 [`SubscriptionError::InvalidUrl`]。
pub fn percent_decode_component(raw: &str) -> Result<String, SubscriptionError> {
    let invalid = |reason: &str| SubscriptionError::InvalidUrl(reason.to_owned());
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = bytes
                .get(index + 1..index + 3)
                .ok_or_else(|| invalid("百分号编码截断"))?;
            let pair = std::str::from_utf8(hex).map_err(|_| invalid("百分号编码非法"))?;
            let byte = u8::from_str_radix(pair, 16).map_err(|_| invalid("百分号编码非法"))?;
            decoded.push(byte);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| invalid("百分号解码结果不是合法 UTF-8"))
}

/// 对 workspace ID 做 query 组件的最小百分号编码（不依赖 url crate）。
#[must_use]
pub fn percent_encode_component(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            // 直接写进 String，避免每个百分号编码字节都多分配一个临时 String。
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_key_matches_rfc_example() {
        // RFC6455 示例：key dGhlIHNhbXBsZSBub25jZQ== → s3pPLMBiTxaQ9kYGzzhZRbK+xOo=
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ==").unwrap(),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
        assert!(accept_key("").is_err());
        assert!(accept_key("not-base64!").is_err());
        assert!(accept_key("YWJj").is_err()); // 长度不是 16 字节
    }

    #[test]
    fn upgrade_validation_accepts_minimal_request() {
        let request = UpgradeRequest {
            method: "GET",
            path: "/sync/v0/subscribe?workspace=ws-1",
            upgrade: Some("websocket"),
            connection: Some("keep-alive, Upgrade"),
            key: Some("dGhlIHNhbXBsZSBub25jZQ=="),
            version: Some("13"),
        };
        let valid = validate_upgrade(&request).unwrap();
        assert_eq!(valid.key, "dGhlIHNhbXBsZSBub25jZQ==");
        assert!(valid.path.contains("workspace=ws-1"));

        for mutate in [
            |r: &mut UpgradeRequest<'_>| r.method = "POST",
            |r: &mut UpgradeRequest<'_>| r.upgrade = Some("h2c"),
            |r: &mut UpgradeRequest<'_>| r.connection = Some("keep-alive"),
            |r: &mut UpgradeRequest<'_>| r.version = Some("12"),
            |r: &mut UpgradeRequest<'_>| r.key = None,
            |r: &mut UpgradeRequest<'_>| r.path = "/sync/v0/hello",
        ] {
            let mut bad = request;
            mutate(&mut bad);
            assert!(validate_upgrade(&bad).is_err());
        }
    }

    #[test]
    fn subscription_url_maps_schemes_and_keeps_prefix() {
        assert_eq!(
            subscription_url("http://127.0.0.1:9123").unwrap(),
            "ws://127.0.0.1:9123/sync/v0/subscribe"
        );
        assert_eq!(
            subscription_url("https://sync.example.com/mz").unwrap(),
            "wss://sync.example.com/mz/sync/v0/subscribe"
        );
        for bad in [
            "",
            "ftp://host",
            "http://",
            "http://a b.com",
            "http://host\n",
        ] {
            assert!(subscription_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn parse_endpoint_defaults_ports_and_rejects_garbage() {
        let endpoint = parse_ws_endpoint("ws://localhost/sync/v0/subscribe").unwrap();
        assert_eq!(endpoint.host, "localhost");
        assert_eq!(endpoint.port, 80);
        assert!(!endpoint.secure);
        let tls = parse_ws_endpoint("wss://[::1]:8443/prefix/sync/v0/subscribe").unwrap();
        assert_eq!(tls.host, "::1");
        assert_eq!(tls.port, 8443);
        assert!(tls.secure);
        for bad in [
            "http://host",
            "ws://",
            "ws://user@host",
            "ws://host:0/path",
            "ws://host:65536/path",
            "ws://host\n/path",
        ] {
            assert!(parse_ws_endpoint(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn percent_encode_keeps_utf8_bytes() {
        assert_eq!(
            percent_encode_component("ws-1/中文"),
            "ws-1%2F%E4%B8%AD%E6%96%87"
        );
        assert_eq!(
            workspace_from_subscribe_path("/sync/v0/subscribe?workspace=ws-1%2F%E4%B8%AD%E6%96%87")
                .unwrap(),
            "ws-1/中文"
        );
        assert!(workspace_from_subscribe_path("/sync/v0/subscribe").is_err());
        assert!(
            workspace_from_subscribe_path("/sync/v0/subscribe?workspace=a&workspace=b").is_err()
        );
        assert!(workspace_from_subscribe_path("/sync/v0/subscribe?workspace=%ZZ").is_err());
    }
}
