//! S3 线上字节编码（D100，契约见 `docs/SYNC-SPEC.md` §5.8）。
//!
//! 本模块只做「`SyncMessage` ⇄ 字节」的确定性转换：解释帧头、CRC、长度前缀与字段顺序，
//! 不解释游标语义、不做 CRDT 合并、不触碰网络或磁盘。这样 `mazha-sync` 仍保持纯 Rust，
//! 只依赖 `mazha-crdt` + `thiserror`，可被核心 crate 无 UI 单测。
//!
//! 所有变长字段用 `u32` 小端长度前缀，所有整数小端；`BTreeMap` 按键升序写出，
//! 保证同一逻辑消息的字节输出唯一（golden vector 可锁定，见模块测试）。

use std::collections::BTreeMap;

use mazha_crdt::{MetaChange, OpId, RosterMeta, Tombstone, TombstoneReason};

use crate::protocol::{
    AckPayload, BatchId, DeviceId, DocFrameMap, HelloAck, HelloPayload, PROTOCOL_VERSION,
    PullPayload, PullResponse, PushAck, PushPayload, SnapshotPayload, SyncCapability, SyncCursor,
    SyncEnvelope, SyncErrorCode, SyncErrorPayload, SyncMessage, WorkspaceId,
};

/// 帧魔数：用于快速识别码札同步帧，值固定为 ASCII `MZS0`。
pub const MAGIC: [u8; 4] = *b"MZS0";

/// 线上帧版本：只接受该值，未知版本一律拒绝（不做向前兼容猜测）。
pub const WIRE_VERSION: u8 = 1;

/// 帧总长上限（含 14 字节头尾）：与 `DEFAULT_MAX_FRAME_BYTES` 的 8 MiB 默认值一致。
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// 解码/编码失败的分类：只描述字节层问题，不泄漏 IO 或 CRDT 细节给上层。
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, thiserror::Error)]
pub enum WireError {
    /// 帧版本字节不被支持。
    #[error("不支持的线上帧版本：{0}")]
    UnsupportedVersion(u8),
    /// kind 不在 `1..=9`。
    #[error("未知消息 kind：{0}")]
    UnknownKind(u8),
    /// 魔数不匹配（不是码札同步帧）。
    #[error("帧 magic 不匹配")]
    BadMagic,
    /// CRC32 校验失败（帧被篡改或截断）。
    #[error("CRC32 校验失败")]
    BadCrc,
    /// 帧体长度超过上限。
    #[error("帧体过大：{0} 字节")]
    BodyTooLarge(usize),
    /// 编码后的整帧超过上限。
    #[error("帧超过 {MAX_FRAME_BYTES} 字节上限")]
    FrameTooLarge,
    /// 剩余字节不足，帧被截断。
    #[error("帧被截断")]
    Truncated,
    /// 字符串字段不是合法 UTF-8。
    #[error("字符串不是合法 UTF-8")]
    InvalidUtf8,
    /// 未知枚举判别值（能力/错误码/墓碑原因/变更类型）。
    #[error("未知枚举判别值：{0}")]
    InvalidEnum(&'static str),
    /// Snapshot 方向字节非法或请求被当作消息解码。
    #[error("未知快照方向：{0}")]
    UnknownSnapshotDirection(u8),
    /// 信封协议版本字符串不匹配。
    #[error("不支持的协议版本：{0}")]
    UnsupportedProtocol(String),
    /// 帧内元数据自相矛盾或无法重建（如重复 doc_id 不一致）。
    #[error("帧内元数据非法：{0}")]
    InvalidFrame(String),
    /// 帧内标识非法（工作区/设备/批次/OpId 设备）。
    #[error("帧内标识非法：{field}")]
    InvalidId {
        /// 字段名，便于定位坏字段。
        field: &'static str,
    },
    /// 内存 writer 的 IO 错误（当前 `Vec<u8>` 实现不会产生，预留给未来流式编码）。
    #[error("IO 错误：{0}")]
    Io(#[from] std::io::Error),
}

/// 追加一个 `u8`（小端单字节）。
pub fn put_u8(buf: &mut Vec<u8>, value: u8) {
    buf.push(value);
}

/// 追加一个 `u32`（小端 4 字节）。
pub fn put_u32(buf: &mut Vec<u8>, value: u32) {
    buf.extend_from_slice(&value.to_le_bytes());
}

/// 追加一个 `u64`（小端 8 字节）。
pub fn put_u64(buf: &mut Vec<u8>, value: u64) {
    buf.extend_from_slice(&value.to_le_bytes());
}

/// 追加一个 `i64`（小端 8 字节补码，不读系统时钟）。
pub fn put_i64(buf: &mut Vec<u8>, value: i64) {
    buf.extend_from_slice(&value.to_le_bytes());
}

/// 追加一段字节：`u32 LE 长度 + 原始字节`。
///
/// 调用方需保证最终帧不超过 [`MAX_FRAME_BYTES`]；超长时长度会被饱和为 `u32::MAX`，
/// 由 `encode_frame` 的统一上限检查拒绝，不会产生半截可用帧。
pub fn put_bytes(buf: &mut Vec<u8>, bytes: &[u8]) {
    put_u32(buf, u32::try_from(bytes.len()).unwrap_or(u32::MAX));
    buf.extend_from_slice(bytes);
}

/// 追加一个 UTF-8 字符串：长度按字节数计，与 [`put_bytes`] 同规则。
pub fn put_string(buf: &mut Vec<u8>, value: &str) {
    put_bytes(buf, value.as_bytes());
}

/// 计算 IEEE CRC-32（初值 `0xFFFF_FFFF`、反射多项式 `0xEDB8_8320`、末异或 `0xFFFF_FFFF`）。
#[must_use]
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        // 逐位反射实现：不引入 crc 依赖，byte 序与 zlib CRC-32 兼容。
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// 只读字节游标：按 declared 顺序读小端字段与长度前缀，所有越界访问返回错误而非 panic。
///
/// 供服务端持久化层复用同一套长度前缀规则（见 `docs/SYNC-SPEC.md` §7.4.1）。
#[allow(clippy::module_name_repetitions)]
#[derive(Debug)]
pub struct WireReader<'a> {
    /// 待读取的完整字节切片。
    bytes: &'a [u8],
    /// 当前读位置，恒满足 `pos <= bytes.len()`。
    pos: usize,
}

impl<'a> WireReader<'a> {
    /// 从字节切片构造游标。
    #[must_use]
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    /// 剩余未读字节数。
    #[must_use]
    pub const fn remaining(&self) -> usize {
        // new/每次读取都保证 pos 不越界，因此这里不会下溢。
        self.bytes.len() - self.pos
    }

    /// 读取一个 `u8`。
    ///
    /// # Errors
    /// 剩余字节不足时返回 [`WireError::Truncated`]。
    pub fn read_u8(&mut self) -> Result<u8, WireError> {
        let byte = *self.bytes.get(self.pos).ok_or(WireError::Truncated)?;
        self.pos += 1;
        Ok(byte)
    }

    /// 读取一个小端 `u32`。
    ///
    /// # Errors
    /// 剩余字节不足时返回 [`WireError::Truncated`]。
    pub fn read_u32(&mut self) -> Result<u32, WireError> {
        let slice = self.take(4)?;
        let array: [u8; 4] = slice.try_into().map_err(|_| WireError::Truncated)?;
        Ok(u32::from_le_bytes(array))
    }

    /// 读取一个小端 `u64`。
    ///
    /// # Errors
    /// 剩余字节不足时返回 [`WireError::Truncated`]。
    pub fn read_u64(&mut self) -> Result<u64, WireError> {
        let slice = self.take(8)?;
        let array: [u8; 8] = slice.try_into().map_err(|_| WireError::Truncated)?;
        Ok(u64::from_le_bytes(array))
    }

    /// 读取一个小端补码 `i64`。
    ///
    /// # Errors
    /// 剩余字节不足时返回 [`WireError::Truncated`]。
    pub fn read_i64(&mut self) -> Result<i64, WireError> {
        Ok(i64::from_le_bytes(self.read_u64()?.to_le_bytes()))
    }

    /// 读取一段 `u32 LE` 长度前缀的字节（返回借用，不分配）。
    ///
    /// # Errors
    /// 长度前缀缺失、声明长度超过剩余字节时返回 [`WireError::Truncated`]；
    /// 长度无法转为 `usize` 时返回 [`WireError::BodyTooLarge`]。
    pub fn read_bytes(&mut self) -> Result<&'a [u8], WireError> {
        let declared = self.read_u32()?;
        let len = usize::try_from(declared).map_err(|_| WireError::BodyTooLarge(0))?;
        self.take(len)
    }

    /// 读取一段 UTF-8 字符串。
    ///
    /// # Errors
    /// 除 [`WireError::Truncated`] 外，非法 UTF-8 返回 [`WireError::InvalidUtf8`]。
    pub fn read_string(&mut self) -> Result<String, WireError> {
        let bytes = self.read_bytes()?;
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| WireError::InvalidUtf8)
    }

    /// 断言已经读到末尾，拒绝尾部夹带额外字节。
    ///
    /// # Errors
    /// 还有剩余字节时返回 [`WireError::InvalidFrame`]。
    pub fn ensure_finished(&self) -> Result<(), WireError> {
        if self.remaining() == 0 {
            Ok(())
        } else {
            Err(WireError::InvalidFrame("尾部有多余字节".to_owned()))
        }
    }

    /// 取出 `len` 字节并推进游标（内部统一越界检查点）。
    fn take(&mut self, len: usize) -> Result<&'a [u8], WireError> {
        if len > self.remaining() {
            return Err(WireError::Truncated);
        }
        let start = self.pos;
        self.pos += len;
        Ok(&self.bytes[start..self.pos])
    }
}

/// 把 `SyncMessage` 编码为完整帧（含 14 字节头尾）。
///
/// # Errors
/// 帧超过 [`MAX_FRAME_BYTES`] 时返回 [`WireError::FrameTooLarge`]。
/// 标识在类型构造期已校验，编码路径不做重复防御；非法字节只在解码侧拒绝。
pub fn encode_message(message: &SyncMessage) -> Result<Vec<u8>, WireError> {
    match message {
        SyncMessage::Hello(request) => {
            let mut body = Vec::new();
            encode_envelope(request, &mut body, encode_hello_payload);
            encode_frame(1, &body)
        }
        SyncMessage::HelloAck(ack) => {
            let mut body = Vec::new();
            encode_hello_ack(ack, &mut body);
            encode_frame(2, &body)
        }
        SyncMessage::Pull(request) => {
            let mut body = Vec::new();
            encode_envelope(request, &mut body, encode_pull_payload);
            encode_frame(3, &body)
        }
        SyncMessage::PullResponse(response) => {
            let mut body = Vec::new();
            encode_pull_response(response, &mut body);
            encode_frame(4, &body)
        }
        SyncMessage::Push(request) => {
            let mut body = Vec::new();
            encode_envelope(request, &mut body, encode_push_payload);
            encode_frame(5, &body)
        }
        SyncMessage::PushAck(ack) => {
            let mut body = Vec::new();
            encode_push_ack(ack, &mut body);
            encode_frame(6, &body)
        }
        SyncMessage::Snapshot(payload) => {
            let mut body = vec![1];
            encode_snapshot_payload(payload, &mut body);
            encode_frame(7, &body)
        }
        // Ack 只有一个 u64 游标，直接写小端字节，省去单独的编码器函数。
        SyncMessage::Ack(payload) => encode_frame(8, &payload.cursor.get().to_le_bytes()),
        SyncMessage::Error(payload) => {
            let mut body = Vec::new();
            encode_error_payload(payload, &mut body);
            encode_frame(9, &body)
        }
    }
}

/// 把完整帧解码为 `SyncMessage`。
///
/// # Errors
/// 魔数/版本/kind/长度/CRC/UTF-8/枚举任一项非法都返回对应 [`WireError`]；
/// `Snapshot` 方向字节 0（请求）不会被当作消息，返回 [`WireError::UnknownSnapshotDirection`]。
pub fn decode_message(bytes: &[u8]) -> Result<SyncMessage, WireError> {
    let (kind, body) = decode_frame(bytes)?;
    let mut reader = WireReader::new(body);
    let message = match kind {
        1 => SyncMessage::Hello(decode_envelope(&mut reader, decode_hello_payload)?),
        2 => SyncMessage::HelloAck(decode_hello_ack(&mut reader)?),
        3 => SyncMessage::Pull(decode_envelope(&mut reader, decode_pull_payload)?),
        4 => SyncMessage::PullResponse(decode_pull_response(&mut reader)?),
        5 => SyncMessage::Push(decode_envelope(&mut reader, decode_push_payload)?),
        6 => SyncMessage::PushAck(decode_push_ack(&mut reader)?),
        7 => {
            // Snapshot 的 body 首字节是方向：只有 1（Payload）属于 SyncMessage 联合。
            match reader.read_u8()? {
                1 => SyncMessage::Snapshot(decode_snapshot_payload(&mut reader)?),
                0 => {
                    return Err(WireError::InvalidFrame(
                        "快照请求不能解码为 SyncMessage".to_owned(),
                    ));
                }
                other => return Err(WireError::UnknownSnapshotDirection(other)),
            }
        }
        8 => SyncMessage::Ack(decode_ack_payload(&mut reader)?),
        9 => SyncMessage::Error(decode_error_payload(&mut reader)?),
        other => return Err(WireError::UnknownKind(other)),
    };
    reader.ensure_finished()?;
    Ok(message)
}

/// 编码快照请求（方向 0）：让无参 `SyncTransport::snapshot()` 携带工作区/设备身份。
///
/// # Errors
/// 帧超过上限时返回 [`WireError::FrameTooLarge`]。
pub fn encode_snapshot_request(request: &SyncEnvelope<()>) -> Result<Vec<u8>, WireError> {
    let mut body = vec![0];
    encode_envelope_header(request, &mut body);
    encode_frame(7, &body)
}

/// 解码快照请求（方向 0），供服务端在路由层消费，不进入 `SyncMessage` 联合。
///
/// # Errors
/// 帧头/方向/信封非法时返回对应 [`WireError`]；方向 1（Payload）被拒绝。
pub fn decode_snapshot_request(bytes: &[u8]) -> Result<SyncEnvelope<()>, WireError> {
    let (kind, body) = decode_frame(bytes)?;
    if kind != 7 {
        return Err(WireError::InvalidEnum("snapshot_request_kind"));
    }
    let mut reader = WireReader::new(body);
    let direction = reader.read_u8()?;
    if direction != 0 {
        return Err(WireError::UnknownSnapshotDirection(direction));
    }
    let (protocol, workspace_id, device_id, sent_at_ms) = decode_envelope_header(&mut reader)?;
    reader.ensure_finished()?;
    Ok(SyncEnvelope {
        protocol,
        workspace_id,
        device_id,
        sent_at_ms,
        payload: (),
    })
}

// ---------- 帧头与 CRC ----------

/// 组装完整帧：头 + body + CRC32，统一执行总长上限检查。
fn encode_frame(kind: u8, body: &[u8]) -> Result<Vec<u8>, WireError> {
    let header_len = MAGIC.len() + 1 + 1 + 4;
    let total = header_len
        .checked_add(body.len())
        .and_then(|value| value.checked_add(4))
        .ok_or(WireError::FrameTooLarge)?;
    if total > MAX_FRAME_BYTES {
        return Err(WireError::FrameTooLarge);
    }
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&MAGIC);
    out.push(WIRE_VERSION);
    out.push(kind);
    put_u32(&mut out, u32::try_from(body.len()).unwrap_or(u32::MAX));
    out.extend_from_slice(body);
    // CRC 覆盖「头 + body」，与解码端使用同一段字节；CRC 自身不参与计算。
    let checksum = crc32(&out);
    put_u32(&mut out, checksum);
    Ok(out)
}

/// 拆解完整帧：校验魔数/版本/kind/长度/CRC，返回 kind 与 body 切片。
fn decode_frame(bytes: &[u8]) -> Result<(u8, &[u8]), WireError> {
    let header_len = MAGIC.len() + 1 + 1 + 4;
    if bytes.len() < header_len + 4 {
        return Err(WireError::Truncated);
    }
    if &bytes[..MAGIC.len()] != MAGIC.as_slice() {
        return Err(WireError::BadMagic);
    }
    if bytes[MAGIC.len()] != WIRE_VERSION {
        return Err(WireError::UnsupportedVersion(bytes[MAGIC.len()]));
    }
    let kind = bytes[MAGIC.len() + 1];
    if !(1..=9).contains(&kind) {
        return Err(WireError::UnknownKind(kind));
    }
    let raw_len = [bytes[6], bytes[7], bytes[8], bytes[9]];
    let body_len = usize::try_from(u32::from_le_bytes(raw_len)).unwrap_or(usize::MAX);
    let total = header_len
        .checked_add(body_len)
        .and_then(|value| value.checked_add(4))
        .ok_or(WireError::BodyTooLarge(body_len))?;
    if total > MAX_FRAME_BYTES {
        return Err(WireError::BodyTooLarge(body_len));
    }
    if bytes.len() != total {
        return Err(WireError::Truncated);
    }
    let body_end = header_len + body_len;
    let raw_crc: [u8; 4] = bytes[body_end..total]
        .try_into()
        .map_err(|_| WireError::Truncated)?;
    if crc32(&bytes[..body_end]) != u32::from_le_bytes(raw_crc) {
        return Err(WireError::BadCrc);
    }
    Ok((kind, &bytes[header_len..body_end]))
}

// ---------- 信封 ----------

/// 写 `SyncEnvelope` 的公共头（不含 payload）。
fn encode_envelope_header<P>(envelope: &SyncEnvelope<P>, buf: &mut Vec<u8>) {
    put_string(buf, &envelope.protocol);
    put_string(buf, envelope.workspace_id.as_str());
    put_string(buf, envelope.device_id.as_str());
    put_i64(buf, envelope.sent_at_ms);
}

/// 写完整信封：公共头后委托具体 payload 编码器（编码器只做 infallible 写入）。
fn encode_envelope<P>(
    envelope: &SyncEnvelope<P>,
    buf: &mut Vec<u8>,
    encode_payload: fn(&P, &mut Vec<u8>),
) {
    encode_envelope_header(envelope, buf);
    encode_payload(&envelope.payload, buf);
}

/// 读 `SyncEnvelope` 公共头，并校验协议版本字符串、类型化 ID。
fn decode_envelope_header(
    reader: &mut WireReader<'_>,
) -> Result<(String, WorkspaceId, DeviceId, i64), WireError> {
    let protocol = reader.read_string()?;
    if protocol != PROTOCOL_VERSION {
        return Err(WireError::UnsupportedProtocol(protocol));
    }
    let workspace_id =
        WorkspaceId::new(reader.read_string()?).map_err(|_| WireError::InvalidId {
            field: "workspace_id",
        })?;
    let device_id = DeviceId::new(reader.read_string()?)
        .map_err(|_| WireError::InvalidId { field: "device_id" })?;
    let sent_at_ms = reader.read_i64()?;
    Ok((protocol, workspace_id, device_id, sent_at_ms))
}

/// 读完整信封：公共头后委托具体 payload 解码器。
fn decode_envelope<P>(
    reader: &mut WireReader<'_>,
    decode_payload: fn(&mut WireReader<'_>) -> Result<P, WireError>,
) -> Result<SyncEnvelope<P>, WireError> {
    let (protocol, workspace_id, device_id, sent_at_ms) = decode_envelope_header(reader)?;
    let payload = decode_payload(reader)?;
    Ok(SyncEnvelope {
        protocol,
        workspace_id,
        device_id,
        sent_at_ms,
        payload,
    })
}

// ---------- 各消息 payload ----------

/// 编码 Hello 载荷：能力集合按声明顺序写出（集合本身顺序无关）。
fn encode_hello_payload(payload: &HelloPayload, buf: &mut Vec<u8>) {
    put_u32(
        buf,
        u32::try_from(payload.capabilities.len()).unwrap_or(u32::MAX),
    );
    for capability in &payload.capabilities {
        let code = match capability {
            SyncCapability::ContentV1 => 0,
            SyncCapability::TombstoneV1 => 1,
            SyncCapability::SnapshotV1 => 2,
        };
        put_u8(buf, code);
    }
}

/// 解码 Hello 载荷，未知能力判别值直接拒绝。
fn decode_hello_payload(reader: &mut WireReader<'_>) -> Result<HelloPayload, WireError> {
    let count = read_count(reader)?;
    let mut capabilities = Vec::with_capacity(count);
    for _ in 0..count {
        let capability = match reader.read_u8()? {
            0 => SyncCapability::ContentV1,
            1 => SyncCapability::TombstoneV1,
            2 => SyncCapability::SnapshotV1,
            _ => return Err(WireError::InvalidEnum("sync_capability")),
        };
        capabilities.push(capability);
    }
    Ok(HelloPayload { capabilities })
}

/// 编码 HelloAck：`session_id`、游标、快照标志与帧上限提示。
fn encode_hello_ack(ack: &HelloAck, buf: &mut Vec<u8>) {
    put_string(buf, &ack.session_id);
    put_u64(buf, ack.cursor.get());
    put_u8(buf, u8::from(ack.requires_snapshot));
    put_u64(buf, ack.max_frame_bytes);
}

/// 解码 HelloAck。
fn decode_hello_ack(reader: &mut WireReader<'_>) -> Result<HelloAck, WireError> {
    Ok(HelloAck {
        session_id: reader.read_string()?,
        cursor: read_cursor(reader)?,
        requires_snapshot: match reader.read_u8()? {
            0 => false,
            1 => true,
            _ => return Err(WireError::InvalidEnum("requires_snapshot")),
        },
        max_frame_bytes: reader.read_u64()?,
    })
}

/// 编码 Pull 载荷：会话 ID、since 游标、状态向量与已知墓碑。
fn encode_pull_payload(payload: &PullPayload, buf: &mut Vec<u8>) {
    put_string(buf, &payload.session_id);
    put_u64(buf, payload.cursor.get());
    put_frame_map(buf, &payload.state_vectors);
    put_meta(buf, &payload.known_metadata);
}

/// 解码 Pull 载荷。
fn decode_pull_payload(reader: &mut WireReader<'_>) -> Result<PullPayload, WireError> {
    Ok(PullPayload {
        session_id: reader.read_string()?,
        cursor: read_cursor(reader)?,
        state_vectors: read_frame_map(reader)?,
        known_metadata: read_meta(reader)?,
    })
}

/// 编码 PullResponse：游标、内容帧与元数据变更。
fn encode_pull_response(response: &PullResponse, buf: &mut Vec<u8>) {
    put_u64(buf, response.cursor.get());
    put_frame_map(buf, &response.frames);
    put_meta_changes(buf, &response.meta_changes);
}

/// 解码 PullResponse。
fn decode_pull_response(reader: &mut WireReader<'_>) -> Result<PullResponse, WireError> {
    Ok(PullResponse {
        cursor: read_cursor(reader)?,
        frames: read_frame_map(reader)?,
        meta_changes: read_meta_changes(reader)?,
    })
}

/// 编码 Push 载荷：批次号、内容帧与元数据变更。
fn encode_push_payload(payload: &PushPayload, buf: &mut Vec<u8>) {
    put_string(buf, payload.batch_id.as_str());
    put_frame_map(buf, &payload.frames);
    put_meta_changes(buf, &payload.meta_changes);
}

/// 解码 Push 载荷。
fn decode_push_payload(reader: &mut WireReader<'_>) -> Result<PushPayload, WireError> {
    Ok(PushPayload {
        batch_id: BatchId::parse(reader.read_string()?)
            .map_err(|_| WireError::InvalidId { field: "batch_id" })?,
        frames: read_frame_map(reader)?,
        meta_changes: read_meta_changes(reader)?,
    })
}

/// 编码 PushAck：新游标与去重标志。
fn encode_push_ack(ack: &PushAck, buf: &mut Vec<u8>) {
    put_u64(buf, ack.cursor.get());
    put_u8(buf, u8::from(ack.duplicate));
}

/// 解码 PushAck。
fn decode_push_ack(reader: &mut WireReader<'_>) -> Result<PushAck, WireError> {
    Ok(PushAck {
        cursor: read_cursor(reader)?,
        duplicate: match reader.read_u8()? {
            0 => false,
            1 => true,
            _ => return Err(WireError::InvalidEnum("duplicate")),
        },
    })
}

/// 编码快照载荷：内容帧、墓碑快照与游标（方向字节由调用方写入）。
fn encode_snapshot_payload(payload: &SnapshotPayload, buf: &mut Vec<u8>) {
    put_frame_map(buf, &payload.frames);
    put_meta(buf, &payload.meta);
    put_u64(buf, payload.cursor.get());
}

/// 解码快照载荷。
fn decode_snapshot_payload(reader: &mut WireReader<'_>) -> Result<SnapshotPayload, WireError> {
    Ok(SnapshotPayload {
        frames: read_frame_map(reader)?,
        meta: read_meta(reader)?,
        cursor: read_cursor(reader)?,
    })
}

/// 解码通用 Ack 载荷。
fn decode_ack_payload(reader: &mut WireReader<'_>) -> Result<AckPayload, WireError> {
    Ok(AckPayload {
        cursor: read_cursor(reader)?,
    })
}

/// 编码错误载荷：错误码、诊断文本与可选游标。
fn encode_error_payload(payload: &SyncErrorPayload, buf: &mut Vec<u8>) {
    put_u8(buf, error_code_value(payload.code));
    put_string(buf, &payload.message);
    match payload.cursor {
        Some(cursor) => {
            put_u8(buf, 1);
            put_u64(buf, cursor.get());
        }
        None => put_u8(buf, 0),
    }
}

/// 解码错误载荷。
fn decode_error_payload(reader: &mut WireReader<'_>) -> Result<SyncErrorPayload, WireError> {
    let code = read_error_code(reader)?;
    let message = reader.read_string()?;
    let cursor = match reader.read_u8()? {
        0 => None,
        1 => Some(read_cursor(reader)?),
        _ => return Err(WireError::InvalidEnum("error_cursor_present")),
    };
    Ok(SyncErrorPayload {
        code,
        message,
        cursor,
    })
}

// ---------- 公共字段助手 ----------

/// 读取一个带上限的长度前缀，防止恶意计数导致超大预分配。
fn read_count(reader: &mut WireReader<'_>) -> Result<usize, WireError> {
    let declared = reader.read_u32()?;
    let count = usize::try_from(declared).unwrap_or(usize::MAX);
    if count > MAX_FRAME_BYTES {
        return Err(WireError::BodyTooLarge(count));
    }
    Ok(count)
}

/// 读游标（小端 u64）。
fn read_cursor(reader: &mut WireReader<'_>) -> Result<SyncCursor, WireError> {
    Ok(SyncCursor::new(reader.read_u64()?))
}

/// 写内容帧映射：`u32 数量 + (DocId 字符串, 字节帧)*`，`BTreeMap` 自身保证键升序。
///
/// 供持久化层复用，避免磁盘格式与线上格式各自实现一套长度前缀规则。
pub fn put_frame_map(buf: &mut Vec<u8>, frames: &DocFrameMap) {
    put_u32(buf, u32::try_from(frames.len()).unwrap_or(u32::MAX));
    for (id, frame) in frames {
        put_string(buf, id);
        put_bytes(buf, frame);
    }
}

/// 读内容帧映射。
///
/// # Errors
/// 数量越界、字符串非 UTF-8 或字节被截断时返回对应 [`WireError`]。
pub fn read_frame_map(reader: &mut WireReader<'_>) -> Result<DocFrameMap, WireError> {
    let count = read_count(reader)?;
    let mut frames = BTreeMap::new();
    for _ in 0..count {
        let id = reader.read_string()?;
        let frame = reader.read_bytes()?.to_vec();
        frames.insert(id, frame);
    }
    Ok(frames)
}

/// 写 `OpId`：逻辑时钟 + 设备字符串。
pub fn put_op_id(buf: &mut Vec<u8>, op_id: &OpId) {
    put_u64(buf, op_id.clock());
    put_string(buf, op_id.device());
}

/// 读 `OpId`，设备非法（空/控制字符）时拒绝整个帧。
///
/// # Errors
/// 字节截断或设备 ID 非法时返回对应 [`WireError`]。
pub fn read_op_id(reader: &mut WireReader<'_>) -> Result<OpId, WireError> {
    let clock = reader.read_u64()?;
    let device = reader.read_string()?;
    OpId::new(clock, device).map_err(|_| WireError::InvalidId {
        field: "op_id.device",
    })
}

/// 写墓碑：doc_id、op_id、原因与可恢复副本帧列表。
pub fn put_tombstone(buf: &mut Vec<u8>, tombstone: &Tombstone) {
    put_string(buf, tombstone.doc_id());
    put_op_id(buf, tombstone.op_id());
    let reason = match tombstone.reason() {
        TombstoneReason::Delete => 0,
        TombstoneReason::RenameSource => 1,
        TombstoneReason::Replace => 2,
    };
    put_u8(buf, reason);
    put_u32(
        buf,
        u32::try_from(tombstone.conflict_frames().len()).unwrap_or(u32::MAX),
    );
    // 副本帧在 CrdtRoster 内已按字节排序去重，这里原样写出即可保持确定性。
    for frame in tombstone.conflict_frames() {
        put_bytes(buf, frame);
    }
}

/// 读墓碑，逐帧校验可解码（坏副本帧使整帧拒绝）。
///
/// # Errors
/// 判别值/ID/副本帧非法时返回对应 [`WireError`]。
pub fn read_tombstone(reader: &mut WireReader<'_>) -> Result<Tombstone, WireError> {
    let doc_id = reader.read_string()?;
    let op_id = read_op_id(reader)?;
    let reason = match reader.read_u8()? {
        0 => TombstoneReason::Delete,
        1 => TombstoneReason::RenameSource,
        2 => TombstoneReason::Replace,
        _ => return Err(WireError::InvalidEnum("tombstone_reason")),
    };
    let count = read_count(reader)?;
    let mut tombstone = Tombstone::new(&doc_id, op_id)
        .map_err(|_| WireError::InvalidId {
            field: "tombstone.doc_id",
        })?
        .with_reason(reason);
    for _ in 0..count {
        let frame = reader.read_bytes()?.to_vec();
        tombstone = tombstone
            .with_conflict_frame(frame)
            .map_err(|error| WireError::InvalidFrame(error.to_string()))?;
    }
    Ok(tombstone)
}

/// 写元数据快照：按 `BTreeMap` 键升序写 `(doc_id, tombstone)`。
pub fn put_meta(buf: &mut Vec<u8>, meta: &RosterMeta) {
    put_u32(
        buf,
        u32::try_from(meta.tombstones().len()).unwrap_or(u32::MAX),
    );
    for (id, tombstone) in meta.tombstones() {
        put_string(buf, id);
        put_tombstone(buf, tombstone);
    }
}

/// 读元数据快照，校验键与墓碑内 doc_id 一致。
///
/// # Errors
/// 墓碑非法、键不一致或字节截断时返回对应 [`WireError`]。
pub fn read_meta(reader: &mut WireReader<'_>) -> Result<RosterMeta, WireError> {
    let count = read_count(reader)?;
    let mut meta = RosterMeta::new();
    for _ in 0..count {
        let id = reader.read_string()?;
        let tombstone = read_tombstone(reader)?;
        if tombstone.doc_id() != id {
            return Err(WireError::InvalidFrame(
                "元数据键与墓碑 doc_id 不一致".to_owned(),
            ));
        }
        meta.merge_tombstone(tombstone)
            .map_err(|error| WireError::InvalidFrame(error.to_string()))?;
    }
    Ok(meta)
}

/// 写元数据变更列表：`u8 类型（0 墓碑 / 1 upsert）` + 各自字段。
pub fn put_meta_changes(buf: &mut Vec<u8>, changes: &[MetaChange]) {
    put_u32(buf, u32::try_from(changes.len()).unwrap_or(u32::MAX));
    for change in changes {
        match change {
            MetaChange::Tombstone(tombstone) => {
                put_u8(buf, 0);
                put_tombstone(buf, tombstone);
            }
            MetaChange::Upsert {
                doc_id,
                op_id,
                frame,
            } => {
                put_u8(buf, 1);
                put_string(buf, doc_id);
                put_op_id(buf, op_id);
                put_bytes(buf, frame);
            }
        }
    }
}

/// 读元数据变更列表（upsert 帧的 Yjs 可解码性由后续 `apply_updates_with_meta` 校验）。
///
/// # Errors
/// 类型判别值非法、ID/帧字节截断时返回对应 [`WireError`]。
pub fn read_meta_changes(reader: &mut WireReader<'_>) -> Result<Vec<MetaChange>, WireError> {
    let count = read_count(reader)?;
    let mut changes = Vec::with_capacity(count);
    for _ in 0..count {
        let change = match reader.read_u8()? {
            0 => MetaChange::Tombstone(read_tombstone(reader)?),
            1 => {
                let doc_id = reader.read_string()?;
                let op_id = read_op_id(reader)?;
                let frame = reader.read_bytes()?.to_vec();
                MetaChange::Upsert {
                    doc_id,
                    op_id,
                    frame,
                }
            }
            _ => return Err(WireError::InvalidEnum("meta_change")),
        };
        changes.push(change);
    }
    Ok(changes)
}

/// 错误码稳定判别值（与 `SyncErrorCode` 声明顺序一致，禁止后续重排）。
fn error_code_value(code: SyncErrorCode) -> u8 {
    match code {
        SyncErrorCode::CursorTooOld => 0,
        SyncErrorCode::BadFrame => 1,
        SyncErrorCode::BadMetadata => 2,
        SyncErrorCode::SnapshotRequired => 3,
        SyncErrorCode::Unauthorized => 4,
        SyncErrorCode::InvalidRequest => 5,
        SyncErrorCode::Internal => 6,
    }
}

/// 读取错误码判别值，未知值拒绝。
fn read_error_code(reader: &mut WireReader<'_>) -> Result<SyncErrorCode, WireError> {
    match reader.read_u8()? {
        0 => Ok(SyncErrorCode::CursorTooOld),
        1 => Ok(SyncErrorCode::BadFrame),
        2 => Ok(SyncErrorCode::BadMetadata),
        3 => Ok(SyncErrorCode::SnapshotRequired),
        4 => Ok(SyncErrorCode::Unauthorized),
        5 => Ok(SyncErrorCode::InvalidRequest),
        6 => Ok(SyncErrorCode::Internal),
        _ => Err(WireError::InvalidEnum("sync_error_code")),
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;
    use mazha_crdt::CrdtDoc;

    /// 测试用合法 Yjs v1 全量帧。
    fn frame(text: &str) -> Vec<u8> {
        CrdtDoc::from_text(text).encode_update()
    }

    /// 构造带身份的信封，避免测试里重复写类型化 ID。
    fn envelope<P>(payload: P) -> SyncEnvelope<P> {
        SyncEnvelope::new(
            WorkspaceId::new("工作区-甲").unwrap(),
            DeviceId::new("设备-A").unwrap(),
            payload,
        )
    }

    ///  roundtrip 断言：编码后解码必须逐字段一致，且编码确定性。
    fn roundtrip(message: &SyncMessage) {
        let bytes = encode_message(message).unwrap();
        assert_eq!(decode_message(&bytes).unwrap(), *message);
        assert_eq!(encode_message(message).unwrap(), bytes);
    }

    #[test]
    fn ack_golden_vector_is_stable() {
        let bytes = encode_message(&SyncMessage::Ack(AckPayload {
            cursor: SyncCursor::new(7),
        }))
        .unwrap();
        let hex = bytes
            .iter()
            .fold(String::with_capacity(bytes.len() * 2), |mut acc, byte| {
                let _ = write!(acc, "{byte:02x}");
                acc
            });
        assert_eq!(hex, "4d5a53300108080000000700000000000000adc3f5b0");
        assert_eq!(bytes.len(), 22);
    }

    #[test]
    fn all_nine_messages_roundtrip() {
        let mut frames = BTreeMap::new();
        frames.insert("notes/a.md".to_owned(), frame("甲内容"));
        frames.insert("笔记/乙.md".to_owned(), frame("乙内容"));

        let tombstone = Tombstone::new("notes/old.md", OpId::new(3, "设备-B").unwrap())
            .unwrap()
            .with_reason(TombstoneReason::RenameSource)
            .with_conflict_frame(frame("旧版内容"))
            .unwrap();
        let mut meta = RosterMeta::new();
        meta.merge_tombstone(tombstone.clone()).unwrap();
        let changes = vec![
            MetaChange::Tombstone(tombstone),
            MetaChange::Upsert {
                doc_id: "notes/new.md".to_owned(),
                op_id: OpId::new(9, "设备-A").unwrap(),
                frame: frame("重建内容"),
            },
        ];

        let messages = vec![
            SyncMessage::Hello(envelope(HelloPayload {
                capabilities: vec![
                    SyncCapability::ContentV1,
                    SyncCapability::TombstoneV1,
                    SyncCapability::SnapshotV1,
                ],
            })),
            SyncMessage::HelloAck(HelloAck {
                session_id: "sess-1".to_owned(),
                cursor: SyncCursor::new(4),
                requires_snapshot: true,
                max_frame_bytes: u64::try_from(MAX_FRAME_BYTES).unwrap_or(u64::MAX),
            }),
            SyncMessage::Pull(envelope(PullPayload {
                session_id: "sess-1".to_owned(),
                cursor: SyncCursor::new(4),
                state_vectors: frames.clone(),
                known_metadata: meta.clone(),
            })),
            SyncMessage::PullResponse(PullResponse {
                cursor: SyncCursor::new(5),
                frames: frames.clone(),
                meta_changes: changes.clone(),
            }),
            SyncMessage::Push(envelope(PushPayload {
                batch_id: BatchId::new(&DeviceId::new("设备-A").unwrap(), 12).unwrap(),
                frames: frames.clone(),
                meta_changes: changes.clone(),
            })),
            SyncMessage::PushAck(PushAck {
                cursor: SyncCursor::new(5),
                duplicate: true,
            }),
            SyncMessage::Snapshot(SnapshotPayload {
                frames: frames.clone(),
                meta: meta.clone(),
                cursor: SyncCursor::new(5),
            }),
            SyncMessage::Ack(AckPayload {
                cursor: SyncCursor::new(5),
            }),
            SyncMessage::Error(SyncErrorPayload {
                code: SyncErrorCode::CursorTooOld,
                message: "游标过旧".to_owned(),
                cursor: Some(SyncCursor::new(5)),
            }),
        ];
        for message in &messages {
            roundtrip(message);
        }
        assert_eq!(messages.iter().map(SyncMessage::kind).count(), 9);
    }

    #[test]
    fn snapshot_request_carries_envelope_identity() {
        let request = envelope(());
        let bytes = encode_snapshot_request(&request).unwrap();
        assert_eq!(decode_snapshot_request(&bytes).unwrap(), request);
        // 快照请求不得被当作 SyncMessage::Snapshot 解码。
        assert!(decode_message(&bytes).is_err());
        // 快照载荷也不得被当作快照请求解码。
        let payload = encode_message(&SyncMessage::Snapshot(SnapshotPayload {
            frames: BTreeMap::new(),
            meta: RosterMeta::new(),
            cursor: SyncCursor::new(1),
        }))
        .unwrap();
        assert!(decode_snapshot_request(&payload).is_err());
    }

    #[test]
    fn reject_bad_magic_version_kind_crc_and_truncation() {
        let good = encode_message(&SyncMessage::Ack(AckPayload {
            cursor: SyncCursor::new(7),
        }))
        .unwrap();

        let mut bad_magic = good.clone();
        bad_magic[0] = b'X';
        assert!(matches!(
            decode_message(&bad_magic),
            Err(WireError::BadMagic)
        ));

        let mut bad_version = good.clone();
        bad_version[4] = 2;
        assert!(matches!(
            decode_message(&bad_version),
            Err(WireError::UnsupportedVersion(2))
        ));

        let mut bad_kind = good.clone();
        bad_kind[5] = 10;
        assert!(matches!(
            decode_message(&bad_kind),
            Err(WireError::UnknownKind(10))
        ));

        let mut bad_crc = good.clone();
        let last = bad_crc.len() - 1;
        bad_crc[last] ^= 0x01;
        assert!(matches!(decode_message(&bad_crc), Err(WireError::BadCrc)));

        let truncated = &good[..good.len() - 1];
        assert!(matches!(
            decode_message(truncated),
            Err(WireError::Truncated)
        ));
        // 尾部夹带额外字节使实际长度与帧头声明不一致，按截断/长度不符拒绝。
        let mut extra = good;
        extra.push(0);
        assert!(matches!(decode_message(&extra), Err(WireError::Truncated)));
    }

    #[test]
    fn reject_bad_utf8_and_invalid_identifiers_in_body() {
        // 手工构造畸形 body，验证解码侧防御（类型化 ID 无法在编码侧构造出非法值）。
        let mut bad_utf8_body = Vec::new();
        put_string(&mut bad_utf8_body, PROTOCOL_VERSION);
        put_string(&mut bad_utf8_body, "ws");
        put_bytes(&mut bad_utf8_body, &[0xff, 0xfe]);
        put_i64(&mut bad_utf8_body, 0);
        put_u32(&mut bad_utf8_body, 0);
        let bad_utf8_frame = encode_frame(1, &bad_utf8_body).unwrap();
        assert!(matches!(
            decode_message(&bad_utf8_frame),
            Err(WireError::InvalidUtf8)
        ));

        let mut bad_id_body = Vec::new();
        put_string(&mut bad_id_body, PROTOCOL_VERSION);
        put_string(&mut bad_id_body, "ws");
        put_string(&mut bad_id_body, "坏\n设备");
        put_i64(&mut bad_id_body, 0);
        put_u32(&mut bad_id_body, 0);
        let bad_id_frame = encode_frame(1, &bad_id_body).unwrap();
        assert!(matches!(
            decode_message(&bad_id_frame),
            Err(WireError::InvalidId { .. })
        ));
    }

    #[test]
    fn enforce_frame_size_limit_on_both_sides() {
        let huge_body = vec![0u8; MAX_FRAME_BYTES];
        assert!(matches!(
            encode_frame(8, &huge_body),
            Err(WireError::FrameTooLarge)
        ));

        // 帧头声明超大 body 但只给 14 字节：应在长度检查阶段拒绝，而非等待读满。
        let mut fake_header = Vec::new();
        fake_header.extend_from_slice(&MAGIC);
        fake_header.push(WIRE_VERSION);
        fake_header.push(8);
        put_u32(
            &mut fake_header,
            u32::try_from(MAX_FRAME_BYTES).unwrap_or(u32::MAX),
        );
        // 先补足最小帧长（含 4 字节 CRC 占位），让解码器走到长度上限检查。
        fake_header.extend_from_slice(&[0; 4]);
        assert!(matches!(
            decode_message(&fake_header),
            Err(WireError::BodyTooLarge(_))
        ));
    }
}
