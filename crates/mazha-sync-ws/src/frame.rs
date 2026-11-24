//! 最小 RFC6455 帧编解码（D108）。
//!
//! 只订阅**文本事件**，因此有意不支持 binary/扩展/压缩：越小越容易在纯单测里穷尽边界。
//! `decode_ws_frame` 是纯函数（给一段字节返回一帧和消费长度），`FrameReader` 在其上
//! 缓存半包并拼装分片文本；两者都不持有 socket，便于测试 text/emoji/分片/ping/pong/坏帧。

use crate::subscription::SubscriptionError;

/// 单帧最大载荷（1 MiB）；订阅事件是元数据提示，绝不该接近同步帧上限。
pub const MAX_WS_PAYLOAD: usize = 1024 * 1024;

/// 一帧的原始语义（分片帧用 `fin` 标记是否结束）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsFrame {
    /// 文本帧（可能还有后续 continuation）。
    Text {
        /// 已解码的 UTF-8 文本。
        text: String,
        /// FIN 位：true 表示整条消息已结束。
        fin: bool,
    },
    /// 文本分片续帧。
    Continuation {
        /// 本片文本。
        data: String,
        /// FIN 位：true 表示最后一片。
        fin: bool,
    },
    /// 心跳请求；本 crate 自动回 Pong，不上抛给业务层。
    Ping(Vec<u8>),
    /// 心跳应答；静默忽略。
    Pong(Vec<u8>),
    /// 关闭帧。
    Close,
}

/// 拼装后的消息（调用方只需处理 Text / Close）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsMessage {
    /// 完整文本事件（分片已拼好）。
    Text(String),
    /// 心跳请求（服务端可据此回 Pong）。
    Ping(Vec<u8>),
    /// 心跳应答。
    Pong(Vec<u8>),
    /// 对端请求关闭。
    Close,
}

/// 增量帧读取器：缓存半包、拼文本分片，不依赖 IO。
#[derive(Debug, Default)]
pub struct FrameReader {
    /// 尚未解析完整的字节。
    buffer: Vec<u8>,
    /// 正在拼装的分片文本。
    fragmented: Option<String>,
}

impl FrameReader {
    /// 创建空读取器。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂入新字节，返回本次可交付的完整消息。
    ///
    /// `expect_masked` 为 true 表示对端是客户端（帧必须带 mask）；服务端下发帧不带 mask。
    ///
    /// # Errors
    /// 坏帧、非法 UTF-8、分片顺序错误或超限时返回 [`SubscriptionError::Protocol`]。
    pub fn push(
        &mut self,
        bytes: &[u8],
        expect_masked: bool,
    ) -> Result<Vec<WsMessage>, SubscriptionError> {
        self.buffer.extend_from_slice(bytes);
        let mut messages = Vec::new();
        // 每次解析一帧就消费对应字节，直到半包或缓冲耗尽；半包留给下次 push。
        while let Some((frame, consumed)) = decode_ws_frame(&self.buffer, expect_masked)? {
            self.buffer.drain(..consumed);
            match frame {
                WsFrame::Text { text, fin } => {
                    if self.fragmented.is_some() {
                        return Err(SubscriptionError::Protocol(
                            "上一组分片未结束时收到新文本帧".into(),
                        ));
                    }
                    if fin {
                        messages.push(WsMessage::Text(text));
                    } else {
                        self.fragmented = Some(text);
                    }
                }
                WsFrame::Continuation { data, fin } => {
                    let assembled = self.fragmented.as_mut().ok_or_else(|| {
                        SubscriptionError::Protocol("收到无起始帧的文本分片".into())
                    })?;
                    if assembled.len() + data.len() > MAX_WS_PAYLOAD {
                        return Err(SubscriptionError::Protocol("分片文本总长超限".into()));
                    }
                    assembled.push_str(&data);
                    if fin {
                        // take 后状态回到“无分片”，后续字节可重新开始一帧。
                        messages.push(WsMessage::Text(self.fragmented.take().unwrap_or_default()));
                    }
                }
                WsFrame::Ping(payload) => messages.push(WsMessage::Ping(payload)),
                WsFrame::Pong(payload) => messages.push(WsMessage::Pong(payload)),
                WsFrame::Close => messages.push(WsMessage::Close),
            }
        }
        Ok(messages)
    }

    /// 当前是否还有未交付的分片（测试/诊断用）。
    #[must_use]
    pub const fn has_pending_fragment(&self) -> bool {
        self.fragmented.is_some()
    }
}

/// 解析一帧；返回 `(帧, 消费字节数)`，字节不足时返回 `Ok(None)` 让调用方继续收。
///
/// 严格边界：RSV 位必须为 0；客户端帧必须 mask，服务端帧必须不 mask；
/// 控制帧不能分片且载荷 ≤ 125；只接受 text/continuation/ping/pong/close。
///
/// # Errors
/// 任意非法字段返回 [`SubscriptionError::Protocol`]。
// 这里把长度字段、mask、负载、opcode 的校验集中在同一纯函数里，边界需要连续阅读；
// 拆成多个小函数反而会让“消费字节数”与“半包判定”分散，故显式放宽行数限制。
#[allow(clippy::too_many_lines)]
pub fn decode_ws_frame(
    input: &[u8],
    expect_masked: bool,
) -> Result<Option<(WsFrame, usize)>, SubscriptionError> {
    if input.len() < 2 {
        return Ok(None);
    }
    let first = input[0];
    let second = input[1];
    if first & 0x70 != 0 {
        return Err(SubscriptionError::Protocol(
            "RSV 位必须为 0（未协商扩展）".into(),
        ));
    }
    let fin = first & 0x80 != 0;
    let opcode = first & 0x0f;
    let masked = second & 0x80 != 0;
    if masked != expect_masked {
        return Err(SubscriptionError::Protocol(if expect_masked {
            "客户端帧必须带 mask".into()
        } else {
            "服务端帧不得带 mask".into()
        }));
    }
    let len_marker = second & 0x7f;
    let (payload_len, header_len) = match len_marker {
        0..=125 => (usize::from(len_marker), 2),
        126 => {
            if input.len() < 4 {
                return Ok(None);
            }
            (usize::from(u16::from_be_bytes([input[2], input[3]])), 4)
        }
        127 => {
            if input.len() < 10 {
                return Ok(None);
            }
            let raw = u64::from_be_bytes(
                input[2..10]
                    .try_into()
                    .map_err(|_| SubscriptionError::Protocol("64 位帧长度字段非法".into()))?,
            );
            (
                usize::try_from(raw)
                    .map_err(|_| SubscriptionError::Protocol("帧长度溢出".into()))?,
                10,
            )
        }
        _ => unreachable!("len_marker 已限制在 0..=127"),
    };
    if payload_len > MAX_WS_PAYLOAD {
        return Err(SubscriptionError::Protocol(format!(
            "帧载荷 {payload_len} 超过上限 {MAX_WS_PAYLOAD}"
        )));
    }
    let is_control = matches!(opcode, 0x8..=0xA);
    if is_control && (!fin || payload_len > 125) {
        return Err(SubscriptionError::Protocol(
            "控制帧不得分片且载荷 ≤125".into(),
        ));
    }
    let mask_offset = header_len;
    let payload_offset = if masked {
        if input.len() < mask_offset + 4 {
            return Ok(None);
        }
        mask_offset + 4
    } else {
        mask_offset
    };
    if input.len() < payload_offset + payload_len {
        return Ok(None);
    }
    let mask = if masked {
        Some(
            <[u8; 4]>::try_from(&input[mask_offset..mask_offset + 4])
                .map_err(|_| SubscriptionError::Protocol("mask 字段长度非法".into()))?,
        )
    } else {
        None
    };
    let mut payload = input[payload_offset..payload_offset + payload_len].to_vec();
    if let Some(key) = mask {
        // RFC6455：负载与 4 字节 key 逐字节 XOR；编码与解码共用这一步。
        for (index, byte) in payload.iter_mut().enumerate() {
            *byte ^= key[index % 4];
        }
    }
    let consumed = payload_offset + payload_len;
    let frame = match opcode {
        0x0 => WsFrame::Continuation {
            data: String::from_utf8(payload)
                .map_err(|_| SubscriptionError::Protocol("分片不是合法 UTF-8".into()))?,
            fin,
        },
        0x1 => WsFrame::Text {
            text: String::from_utf8(payload)
                .map_err(|_| SubscriptionError::Protocol("文本帧不是合法 UTF-8".into()))?,
            fin,
        },
        0x8 => WsFrame::Close,
        0x9 => WsFrame::Ping(payload),
        0xA => WsFrame::Pong(payload),
        other => {
            return Err(SubscriptionError::Protocol(format!(
                "不支持的 opcode 0x{other:x}"
            )));
        }
    };
    Ok(Some((frame, consumed)))
}

/// 编码一帧；`mask` 为 `Some(key)` 时按客户端要求加掩，服务端传 `None`。
///
/// # Errors
/// 载荷超限或长度字段无法表达时返回 [`SubscriptionError::Protocol`]。
pub fn encode_ws_frame(
    frame: &WsFrame,
    mask: Option<[u8; 4]>,
) -> Result<Vec<u8>, SubscriptionError> {
    let (fin, opcode, payload) = match frame {
        WsFrame::Text { text, fin } => (*fin, 0x1, text.as_bytes().to_vec()),
        WsFrame::Continuation { data, fin } => (*fin, 0x0, data.as_bytes().to_vec()),
        WsFrame::Close => (true, 0x8, Vec::new()),
        WsFrame::Ping(payload) => (true, 0x9, payload.clone()),
        WsFrame::Pong(payload) => (true, 0xA, payload.clone()),
    };
    if payload.len() > MAX_WS_PAYLOAD {
        return Err(SubscriptionError::Protocol("帧载荷超过上限".into()));
    }
    let mut out = Vec::with_capacity(payload.len() + 14);
    // FIN 与 opcode 必须按位或到同一个字节；分两步写避免 `if` 表达式结合性歧义。
    let mut first_byte = opcode;
    if fin {
        first_byte |= 0x80;
    }
    out.push(first_byte);
    // RFC6455 的 mask 位与 7 位长度共享第二个字节，不能拆成两个字节写。
    let len = payload.len();
    let (len_marker, extended): (u8, Vec<u8>) = if len <= 125 {
        (
            u8::try_from(len).map_err(|_| SubscriptionError::Protocol("长度字段溢出".into()))?,
            Vec::new(),
        )
    } else if len <= 0xffff {
        (
            126,
            u16::try_from(len)
                .map_err(|_| SubscriptionError::Protocol("16 位长度字段溢出".into()))?
                .to_be_bytes()
                .to_vec(),
        )
    } else {
        (
            127,
            u64::try_from(len)
                .map_err(|_| SubscriptionError::Protocol("帧长度无法用 u64 表达".into()))?
                .to_be_bytes()
                .to_vec(),
        )
    };
    out.push(if mask.is_some() {
        0x80 | len_marker
    } else {
        len_marker
    });
    out.extend_from_slice(&extended);
    if let Some(key) = mask {
        out.extend_from_slice(&key);
        for (index, byte) in payload.iter().enumerate() {
            out.push(byte ^ key[index % 4]);
        }
    } else {
        out.extend_from_slice(&payload);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_and_emoji_roundtrip() {
        let cases = ["hello", "中文与 emoji 🪄", ""];
        for text in cases {
            let frame = WsFrame::Text {
                text: text.to_owned(),
                fin: true,
            };
            let bytes = encode_ws_frame(&frame, None).unwrap();
            let (decoded, consumed) = decode_ws_frame(&bytes, false).unwrap().unwrap();
            assert_eq!(decoded, frame);
            assert_eq!(consumed, bytes.len());
        }
    }

    #[test]
    fn client_frames_are_masked_and_server_frames_are_not() {
        let frame = WsFrame::Text {
            text: "secret".into(),
            fin: true,
        };
        let masked = encode_ws_frame(&frame, Some([1, 2, 3, 4])).unwrap();
        assert_eq!(decode_ws_frame(&masked, true).unwrap().unwrap().0, frame);
        assert!(decode_ws_frame(&masked, false).is_err());
        let plain = encode_ws_frame(&frame, None).unwrap();
        assert!(decode_ws_frame(&plain, true).is_err());
    }

    #[test]
    fn fragmented_text_is_assembled() {
        let mut reader = FrameReader::new();
        let first = encode_ws_frame(
            &WsFrame::Text {
                text: "中".into(),
                fin: false,
            },
            None,
        )
        .unwrap();
        let middle = encode_ws_frame(
            &WsFrame::Continuation {
                data: "文🪄".into(),
                fin: false,
            },
            None,
        )
        .unwrap();
        let last = encode_ws_frame(
            &WsFrame::Continuation {
                data: "ok".into(),
                fin: true,
            },
            None,
        )
        .unwrap();
        assert!(reader.push(&first, false).unwrap().is_empty());
        assert!(reader.has_pending_fragment());
        assert!(reader.push(&middle, false).unwrap().is_empty());
        assert_eq!(
            reader.push(&last, false).unwrap(),
            vec![WsMessage::Text("中文🪄ok".into())]
        );
        assert!(!reader.has_pending_fragment());
    }

    #[test]
    fn ping_pong_and_close_are_decoded() {
        let mut reader = FrameReader::new();
        let bytes = [
            encode_ws_frame(&WsFrame::Ping(vec![1, 2]), None).unwrap(),
            encode_ws_frame(&WsFrame::Pong(vec![3]), None).unwrap(),
            encode_ws_frame(&WsFrame::Close, None).unwrap(),
        ]
        .concat();
        assert_eq!(
            reader.push(&bytes, false).unwrap(),
            vec![
                WsMessage::Ping(vec![1, 2]),
                WsMessage::Pong(vec![3]),
                WsMessage::Close,
            ]
        );
    }

    #[test]
    fn bad_frames_are_rejected() {
        // RSV 位非零。
        assert!(decode_ws_frame(&[0xc1, 0x00], false).is_err());
        // 保留 opcode。
        assert!(decode_ws_frame(&[0x03, 0x00], false).is_err());
        // 控制帧分片。
        assert!(decode_ws_frame(&[0x09, 0x00], false).is_err());
        // 非法 UTF-8。
        assert!(decode_ws_frame(&[0x81, 0x01, 0xff], false).is_err());
        // 半包不报错，等下一批字节。
        assert!(
            decode_ws_frame(&[0x81, 0x05, b'h'], false)
                .unwrap()
                .is_none()
        );
        // 续帧没有起始帧。
        let mut reader = FrameReader::new();
        let bytes = encode_ws_frame(
            &WsFrame::Continuation {
                data: "x".into(),
                fin: true,
            },
            None,
        )
        .unwrap();
        assert!(reader.push(&bytes, false).is_err());
    }

    #[test]
    fn long_payload_uses_16_or_64_bit_length() {
        let long = "a".repeat(70_000);
        let frame = WsFrame::Text {
            text: long.clone(),
            fin: true,
        };
        let bytes = encode_ws_frame(&frame, None).unwrap();
        assert_eq!(bytes[1], 127);
        assert_eq!(decode_ws_frame(&bytes, false).unwrap().unwrap().0, frame);
        let medium = "b".repeat(200);
        let frame = WsFrame::Text {
            text: medium.clone(),
            fin: true,
        };
        let bytes = encode_ws_frame(&frame, Some([9, 8, 7, 6])).unwrap();
        assert_eq!(bytes[1] & 0x7f, 126);
        assert_eq!(decode_ws_frame(&bytes, true).unwrap().unwrap().0, frame);
    }
}
