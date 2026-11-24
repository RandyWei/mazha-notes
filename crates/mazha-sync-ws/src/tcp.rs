//! 阻塞式 TCP 订阅客户端（D108）。
//!
//! 只在专用订阅线程创建/调用：一次 `connect` 完成 HTTP Upgrade，之后循环读帧；
//! 超时用 `TcpStream::set_read_timeout` 实现，不引入 async runtime。握手响应校验、
//! 帧解析、事件 JSON 都复用本 crate 的纯函数，因此“不连真实端口”也能验证协议正确性。

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;

use crate::event::SyncEvent;
use crate::event::decode_event_json;
use crate::frame::{FrameReader, WsFrame, encode_ws_frame};
use crate::handshake::{accept_key, parse_ws_endpoint, percent_encode_component};
use crate::subscription::{SubscriptionConfig, SubscriptionError, SyncSubscription};

/// 帧读写缓冲大小；订阅事件很小，4 KiB 足够，避免大块分配。
const IO_CHUNK: usize = 4096;
/// 握手头上限，超过视为协议错误（防止无界读取）。
const MAX_HANDSHAKE_BYTES: usize = 16 * 1024;
/// 客户端 mask 序列号；同一进程多连接不冲突即可，不用于安全用途。
static MASK_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 生成 16 字节随机样式的 `Sec-WebSocket-Key`（base64）。
///
/// 使用时间 + 进程内计数器派生，**不用于加密**；RFC6455 只要求每连接不可预测。
#[must_use]
pub fn websocket_key() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        // 时间 nanos 超过 u64 的概率仅在极远未来；饱和到 MAX 保持确定性且不 panic。
        .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or_default();
    let counter = MASK_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = u64::from(std::process::id());
    let mut raw = [0u8; 16];
    for (index, byte) in raw.iter_mut().enumerate() {
        let index = u64::try_from(index).unwrap_or_default();
        let word = now
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(counter)
            .wrapping_add(pid)
            .wrapping_add(index.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        // 取中间字节：先 mask 再 try_from，保证不依赖截断转换。
        *byte = u8::try_from((word >> 24) & 0xff).unwrap_or_default();
    }
    BASE64.encode(raw)
}

/// 生成客户端帧 mask；与 key 同理只需每帧不同，不用于安全。
fn next_mask() -> [u8; 4] {
    let word = MASK_COUNTER.fetch_add(1, Ordering::Relaxed);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        // 同上：饱和处理避免 u128 → u64 的静默截断。
        .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or_default();
    let mixed = word ^ now ^ u64::from(std::process::id());
    let bytes = mixed.to_ne_bytes();
    // 只取 4 字节作为 mask；其余高位不参与，避免截断告警。
    <[u8; 4]>::try_from(&bytes[..4]).unwrap_or([0, 0, 0, 0])
}

/// 校验 101 握手响应头（纯函数，便于无 socket 单测）。
///
/// # Errors
/// 状态行不是 101、Accept 不匹配或缺少 Connection: Upgrade 时返回握手错误。
pub fn validate_handshake_response(
    headers: &str,
    expected_accept: &str,
) -> Result<(), SubscriptionError> {
    let mut lines = headers.lines();
    let status = lines.next().unwrap_or_default();
    if !status.starts_with("HTTP/1.1 101") && !status.starts_with("HTTP/1.0 101") {
        return Err(SubscriptionError::Handshake(format!(
            "握手状态不是 101：{status}"
        )));
    }
    let mut accept = None;
    let mut connection_upgrade = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("sec-websocket-accept") {
            accept = Some(value.trim());
        }
        if name.trim().eq_ignore_ascii_case("connection")
            && value
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
        {
            connection_upgrade = true;
        }
    }
    if !connection_upgrade {
        return Err(SubscriptionError::Handshake(
            "握手响应缺少 Connection: Upgrade".into(),
        ));
    }
    if accept != Some(expected_accept) {
        return Err(SubscriptionError::Handshake(
            "Sec-WebSocket-Accept 不匹配".into(),
        ));
    }
    Ok(())
}

/// 阻塞式 WebSocket 订阅连接。
pub struct TcpSubscription {
    /// 已升级的 TCP 连接。
    stream: TcpStream,
    /// 帧缓存与分片拼装器。
    reader: FrameReader,
    /// 建连配置（token 仅在内存）。
    config: SubscriptionConfig,
    /// 本地已关闭标志。
    closed: bool,
}

impl TcpSubscription {
    /// 建立连接并完成 RFC6455 握手。
    ///
    /// # Errors
    /// URL 非法、host 解析失败、连接失败、握手超时或校验失败时返回对应错误。
    pub fn connect(config: SubscriptionConfig) -> Result<Self, SubscriptionError> {
        let endpoint = parse_ws_endpoint(&config.url)?;
        let address = (endpoint.host.as_str(), endpoint.port)
            .to_socket_addrs()
            .map_err(|error| SubscriptionError::Io(format!("订阅地址解析失败：{error}")))?
            .next()
            .ok_or_else(|| SubscriptionError::Io("订阅地址没有可用地址".into()))?;
        let mut stream = TcpStream::connect_timeout(&address, config.read_timeout)
            .map_err(|error| SubscriptionError::Io(format!("订阅连接失败：{error}")))?;
        stream
            .set_read_timeout(Some(config.read_timeout))
            .map_err(|error| io_error(&error))?;
        stream
            .set_write_timeout(Some(config.read_timeout))
            .map_err(|error| io_error(&error))?;
        let key = websocket_key();
        let expected = accept_key(&key)?;
        let path = format!(
            "{}?workspace={}",
            endpoint.path,
            percent_encode_component(&config.workspace)
        );
        // token 只走 Authorization 头；不进 URL、不写日志、不落盘。
        let request = format!(
            "GET {path} HTTP/1.1\r\n\
             Host: {}:{}\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Key: {key}\r\n\
             Sec-WebSocket-Version: 13\r\n\
             Authorization: Bearer {}\r\n\
             \r\n",
            endpoint.host, endpoint.port, config.token
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|error| io_error(&error))?;
        stream.flush().map_err(|error| io_error(&error))?;
        let mut head = Vec::new();
        let mut chunk = [0u8; IO_CHUNK];
        loop {
            if head.len() > MAX_HANDSHAKE_BYTES {
                return Err(SubscriptionError::Handshake("握手响应头过大".into()));
            }
            let read = stream.read(&mut chunk).map_err(|error| io_error(&error))?;
            if read == 0 {
                return Err(SubscriptionError::Handshake("握手响应提前结束".into()));
            }
            head.extend_from_slice(&chunk[..read]);
            if let Some(position) = find_header_end(&head) {
                // 理论上 Upgrade 响应无 body；若是对端误带 body，只消费到头部边界即可。
                let headers = String::from_utf8_lossy(&head[..position]).into_owned();
                validate_handshake_response(&headers, &expected)?;
                break;
            }
        }
        Ok(Self {
            stream,
            reader: FrameReader::new(),
            config,
            closed: false,
        })
    }

    /// 读取下一条事件（带配置里的默认超时）。
    fn read_one(&mut self, timeout: Duration) -> Result<Option<SyncEvent>, SubscriptionError> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.closed {
                return Err(SubscriptionError::Closed);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(SubscriptionError::Timeout);
            }
            let wait = remaining.min(self.config.read_timeout);
            self.stream
                .set_read_timeout(Some(wait))
                .map_err(|error| io_error(&error))?;
            let mut chunk = [0u8; IO_CHUNK];
            match self.stream.read(&mut chunk) {
                Ok(0) => {
                    self.closed = true;
                    return Err(SubscriptionError::Closed);
                }
                Ok(read) => {
                    let messages = self.reader.push(&chunk[..read], false)?;
                    for message in messages {
                        match message {
                            crate::frame::WsMessage::Text(text) => {
                                return decode_event_json(&text).map(Some);
                            }
                            crate::frame::WsMessage::Ping(payload) => {
                                // 客户端帧必须 mask；Pong 回同一载荷即可（纯异步：先写后继续读）。
                                let pong =
                                    encode_ws_frame(&WsFrame::Pong(payload), Some(next_mask()))?;
                                self.stream
                                    .write_all(&pong)
                                    .map_err(|error| io_error(&error))?;
                            }
                            crate::frame::WsMessage::Pong(_) => {}
                            crate::frame::WsMessage::Close => {
                                self.closed = true;
                                return Err(SubscriptionError::Closed);
                            }
                        }
                    }
                }
                // 读超时不是错误：回到循环开头继续等事件，并检查停止条件。
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) => {}
                Err(error) => return Err(io_error(&error)),
            }
        }
    }
}

impl SyncSubscription for TcpSubscription {
    fn next_event(&mut self) -> Result<Option<SyncEvent>, SubscriptionError> {
        self.read_one(self.config.read_timeout)
    }

    fn next_event_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<SyncEvent>, SubscriptionError> {
        self.read_one(timeout)
    }

    fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        if let Ok(frame) = encode_ws_frame(&WsFrame::Close, Some(next_mask())) {
            let _ = self.stream.write_all(&frame);
        }
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

impl Drop for TcpSubscription {
    fn drop(&mut self) {
        self.close();
    }
}

/// 把 IO 错误转成统一文案；不输出 token（token 从未进入 IO 错误上下文）。
fn io_error(error: &std::io::Error) -> SubscriptionError {
    SubscriptionError::Io(error.to_string())
}

/// 找 HTTP 头结束位置（纯函数）；返回 `\r\n\r\n` 之后的下标。
fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|pos| pos + 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_key_is_16_bytes_base64() {
        let key = websocket_key();
        let raw = BASE64.decode(&key).unwrap();
        assert_eq!(raw.len(), 16);
        assert_ne!(key, websocket_key());
    }

    #[test]
    fn handshake_response_validation_is_pure() {
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        let accept = accept_key(key).unwrap();
        let good = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
        );
        assert!(validate_handshake_response(&good, &accept).is_ok());
        assert!(validate_handshake_response(&good, "wrong").is_err());
        assert!(
            validate_handshake_response(&good.replace("HTTP/1.1 101", "HTTP/1.1 200"), &accept)
                .is_err()
        );
    }

    #[test]
    fn header_end_detection_handles_partial_input() {
        assert_eq!(find_header_end(b"a\r\n\r\nbody"), Some(5));
        assert_eq!(find_header_end(b"a\r\n\r"), None);
    }
}
