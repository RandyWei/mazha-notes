//! S2 内存协议模型（D99，契约见 `docs/SYNC-SPEC.md` §5.7）。
//!
//! 本模块只描述**消息类型与字段语义**，不定义线上字节编码或信封字段名：
//! 内容帧仍是 `BTreeMap<DocId, Vec<u8>>` 的标准 Yjs v1 帧，元数据用
//! `Vec<MetaChange>` / `RosterMeta` 显式承载，绝不伪装进 Yjs 文本。
//! 所有标识在构造期校验（非空、无控制字符），非法输入返回错误而不 panic。

use std::collections::BTreeMap;
use std::fmt;

use mazha_crdt::{CrdtError, DocId, MetaChange, RosterMeta};

/// 当前内存协议版本；仅作版本标识，不参与收敛判断。
pub const PROTOCOL_VERSION: &str = "mazha-sync/0";

/// 默认最大内容帧字节数（服务端 `HelloAck` 的限流提示）。
pub const DEFAULT_MAX_FRAME_BYTES: u64 = 8 * 1024 * 1024;

/// 协议层错误：标识/信封校验与 CRDT 下层错误的统一入口。
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    /// 类型化标识非法（工作区、设备、批次）。
    #[error("同步标识非法（{field}）：{value}")]
    InvalidId {
        /// 字段名，便于定位是哪个信封字段坏了。
        field: &'static str,
        /// 非法原值，仅用于诊断。
        value: String,
    },

    /// 信封协议版本不匹配（收到其它版本或篡改过的信封）。
    #[error("不支持的同步协议版本：{0}")]
    UnsupportedProtocol(String),

    /// 下层 CRDT 校验/应用失败（只做透传，不把网络错误塞进 CRDT）。
    #[error("同步数据处理失败：{0}")]
    Crdt(#[from] CrdtError),
}

/// 校验同步标识：必须非空且不含控制字符（与 `DocId` 同一规则）。
fn validate_id(field: &'static str, value: &str) -> Result<(), ProtocolError> {
    // 空串无法做去重键或确定性批次前缀；控制字符会破坏日志与未来字节信封的边界。
    // 校验放在构造期，后续所有消息都可假定 ID 已合法，避免各处重复防御。
    if value.is_empty() || value.chars().any(char::is_control) {
        Err(ProtocolError::InvalidId {
            field,
            value: value.to_owned(),
        })
    } else {
        Ok(())
    }
}

/// 工作区稳定标识（D95：不等于本地绝对路径）。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkspaceId(String);

impl WorkspaceId {
    /// 构造工作区 ID。
    ///
    /// # Errors
    /// 空串或含控制字符时返回 [`ProtocolError::InvalidId`]，不 panic。
    pub fn new(value: impl Into<String>) -> Result<Self, ProtocolError> {
        let value = value.into();
        validate_id("workspace_id", &value)?;
        Ok(Self(value))
    }

    /// 只读字符串视图。
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WorkspaceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// 设备稳定标识：同时是服务端去重键与 `OpId` 的 tie-break 来源。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceId(String);

impl DeviceId {
    /// 构造设备 ID。
    ///
    /// # Errors
    /// 空串或含控制字符时返回 [`ProtocolError::InvalidId`]，不 panic。
    pub fn new(value: impl Into<String>) -> Result<Self, ProtocolError> {
        let value = value.into();
        validate_id("device_id", &value)?;
        Ok(Self(value))
    }

    /// 只读字符串视图。
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// 出站批次标识：`{device_id}-{monotonic_counter:020}`（D99）。
///
/// 刻意不用 ULID/随机数：同一设备同一计数器的批次号必须逐字节可复现，
/// 这样单测可断言幂等去重，离线重放也不依赖系统熵或墙钟。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BatchId(String);

impl BatchId {
    /// 由设备 ID 与会话内单调计数器生成确定性批次号。
    ///
    /// # Errors
    /// 设备 ID 已由类型校验；若拼接结果仍非法（防御性检查）返回
    /// [`ProtocolError::InvalidId`]。
    pub fn new(device: &DeviceId, counter: u64) -> Result<Self, ProtocolError> {
        // 定宽 20 位使字典序与数值序一致，便于日志排查与确定性断言。
        Self::parse(format!("{}-{counter:020}", device.as_str()))
    }

    /// 解析既有批次号（用于回放历史批次），只做格式合法性校验。
    ///
    /// # Errors
    /// 空串或含控制字符时返回 [`ProtocolError::InvalidId`]。
    pub fn parse(value: impl Into<String>) -> Result<Self, ProtocolError> {
        let value = value.into();
        validate_id("batch_id", &value)?;
        Ok(Self(value))
    }

    /// 只读字符串视图。
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BatchId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// 服务端同步游标：单调 u64，只有服务端解释其含义（D99）。
///
/// 客户端只负责保存与回放；任何错误路径都不得推进它。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SyncCursor(u64);

impl SyncCursor {
    /// 由数值构造游标。
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// 数值视图。
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// 下一个游标（`u64::MAX` 处饱和，避免溢出回绕破坏单调性）。
    #[must_use]
    pub fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

impl fmt::Display for SyncCursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 内容帧映射：值为标准 Yjs v1 更新帧（复用 D94/D98 的数据形状）。
pub type DocFrameMap = BTreeMap<DocId, Vec<u8>>;

/// 客户端声明的协议能力（认证凭证留 S3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SyncCapability {
    /// 标准 Yjs v1 内容帧。
    ContentV1,
    /// S1 墓碑/元数据变更。
    TombstoneV1,
    /// 全量快照引导。
    SnapshotV1,
}

/// Hello 载荷：握手时声明能力。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelloPayload {
    /// 客户端支持的协议能力集合（顺序不影响语义）。
    pub capabilities: Vec<SyncCapability>,
}

/// Hello 请求（自带工作区与设备身份的信封）。
pub type HelloRequest = SyncEnvelope<HelloPayload>;

/// 服务端握手应答。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelloAck {
    /// 会话 ID；后续 Pull 请求需回放。
    pub session_id: String,
    /// 服务端当前游标（诊断用，**不是**首轮拉取的 `since`）。
    pub cursor: SyncCursor,
    /// 是否必须先做快照引导。
    pub requires_snapshot: bool,
    /// 服务端最大帧大小提示。
    pub max_frame_bytes: u64,
}

/// Pull 载荷：按本地状态向量与游标请求增量。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullPayload {
    /// 握手得到的会话 ID。
    pub session_id: String,
    /// 客户端最后确认的游标，作为服务端的 `since`。
    pub cursor: SyncCursor,
    /// 整册状态向量（标准 Yjs v1），供服务端计算缺失内容。
    pub state_vectors: DocFrameMap,
    /// 本地已知墓碑快照，供服务端计算缺失元数据。
    pub known_metadata: RosterMeta,
}

/// Pull 请求。
pub type PullRequest = SyncEnvelope<PullPayload>;

/// Pull 应答：内容帧与元数据变更**同时**携带（D99）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullResponse {
    /// 本轮应答对应的服务端游标；应用成功后客户端才推进。
    pub cursor: SyncCursor,
    /// 内容帧（Yjs v1）。
    pub frames: DocFrameMap,
    /// 元数据变更（墓碑/upsert）。
    pub meta_changes: Vec<MetaChange>,
}

/// Push 载荷：一批出站内容帧 + 元数据变更。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushPayload {
    /// 确定性批次号，服务端按 `(workspace, device, batch)` 去重。
    pub batch_id: BatchId,
    /// 内容帧（Yjs v1）。
    pub frames: DocFrameMap,
    /// 元数据变更。
    pub meta_changes: Vec<MetaChange>,
}

/// Push 请求。
pub type PushRequest = SyncEnvelope<PushPayload>;

/// Push 应答。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushAck {
    /// 服务端提交该批次后的新游标；重复批次返回首次 ack 的游标。
    pub cursor: SyncCursor,
    /// 是否为重复批次（客户端可直接出队，不再改动状态）。
    pub duplicate: bool,
}

/// 全量快照载荷：新设备引导与游标过旧回退用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotPayload {
    /// 全部存活文档的 Yjs v1 全量帧。
    pub frames: DocFrameMap,
    /// 全量墓碑快照。
    pub meta: RosterMeta,
    /// 快照对应的服务端游标。
    pub cursor: SyncCursor,
}

/// 通用确认载荷（预留给未来服务端主动推送；MVP transport 不产出）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AckPayload {
    /// 已确认的游标。
    pub cursor: SyncCursor,
}

/// 服务端协议错误码（与传输层 `TransportError` 分层）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SyncErrorCode {
    /// 客户端游标早于服务端日志下界，需快照引导。
    #[error("游标过旧，服务端历史已裁剪")]
    CursorTooOld,
    /// 内容帧不是合法 Yjs v1 帧。
    #[error("内容帧损坏")]
    BadFrame,
    /// 元数据变更非法（ID/OpId/墓碑/副本帧）。
    #[error("元数据损坏")]
    BadMetadata,
    /// 服务端要求先引导快照（如客户端游标超前）。
    #[error("需要先引导快照")]
    SnapshotRequired,
    /// 鉴权失败（真实服务端 S3 使用）。
    #[error("未授权")]
    Unauthorized,
    /// 请求字段自相矛盾（如会话 ID 不匹配）。
    #[error("请求非法")]
    InvalidRequest,
    /// 服务端内部错误。
    #[error("服务端内部错误")]
    Internal,
}

/// 服务端错误载荷：错误码 + 诊断信息 + 可选游标提示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncErrorPayload {
    /// 稳定错误码，客户端据其决定重试/回退策略。
    pub code: SyncErrorCode,
    /// 人类可读诊断信息（不写明文文档内容）。
    pub message: String,
    /// 服务端当前游标提示（可选）。
    pub cursor: Option<SyncCursor>,
}

/// 把 CRDT 下层错误映射为协议错误码：坏帧与坏元数据分层，便于客户端决策。
#[must_use]
pub const fn error_code_for(error: &CrdtError) -> SyncErrorCode {
    match error {
        CrdtError::Decode(_)
        | CrdtError::Apply(_)
        | CrdtError::InvalidConflictFrame { .. }
        | CrdtError::InvalidMetaFrame { .. } => SyncErrorCode::BadFrame,
        CrdtError::InvalidDocumentId(_)
        | CrdtError::InvalidOpId(_)
        | CrdtError::TombstoneConflict(_)
        | CrdtError::DocumentAlreadyExists(_)
        | CrdtError::DocumentNotFound(_) => SyncErrorCode::BadMetadata,
        CrdtError::OutOfBounds { .. } | CrdtError::TooLarge(_) => SyncErrorCode::InvalidRequest,
    }
}

/// 统一内存信封：请求自带身份，响应由 transport 的会话上下文定位。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncEnvelope<P> {
    /// 协议版本，构造时固定为 [`PROTOCOL_VERSION`]。
    pub protocol: String,
    /// 工作区标识。
    pub workspace_id: WorkspaceId,
    /// 发送方设备标识。
    pub device_id: DeviceId,
    /// 仅诊断/审计的墙钟毫秒；默认 0，不参与任何比较，便于单测复现。
    pub sent_at_ms: i64,
    /// 具体消息载荷。
    pub payload: P,
}

impl<P> SyncEnvelope<P> {
    /// 构造信封；`sent_at_ms` 默认 0（确定性），需要审计时由 transport 填。
    #[must_use]
    pub fn new(workspace_id: WorkspaceId, device_id: DeviceId, payload: P) -> Self {
        Self {
            protocol: PROTOCOL_VERSION.to_owned(),
            workspace_id,
            device_id,
            sent_at_ms: 0,
            payload,
        }
    }

    /// 设置诊断时间戳（不读取系统时钟，交由调用方注入）。
    #[must_use]
    pub fn with_sent_at_ms(mut self, sent_at_ms: i64) -> Self {
        self.sent_at_ms = sent_at_ms;
        self
    }

    /// 校验信封版本；篡改或跨版本信封不得静默接受。
    ///
    /// # Errors
    /// 协议版本不是 [`PROTOCOL_VERSION`] 时返回
    /// [`ProtocolError::UnsupportedProtocol`]。
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.protocol != PROTOCOL_VERSION {
            return Err(ProtocolError::UnsupportedProtocol(self.protocol.clone()));
        }
        Ok(())
    }
}

/// 九种同步消息的顺序标识（顺序固定，便于确定性测试与未来字节编码）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SyncMessageKind {
    /// 握手请求。
    Hello,
    /// 握手应答。
    HelloAck,
    /// 增量拉取请求。
    Pull,
    /// 增量拉取应答。
    PullResponse,
    /// 批次推送请求。
    Push,
    /// 批次推送应答。
    PushAck,
    /// 全量快照。
    Snapshot,
    /// 通用确认。
    Ack,
    /// 错误。
    Error,
}

/// S2 消息标记联合：所有在会话层流动的消息都可用该类型统一观测/测试。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncMessage {
    /// 握手请求。
    Hello(HelloRequest),
    /// 握手应答。
    HelloAck(HelloAck),
    /// 增量拉取请求。
    Pull(PullRequest),
    /// 增量拉取应答。
    PullResponse(PullResponse),
    /// 批次推送请求。
    Push(PushRequest),
    /// 批次推送应答。
    PushAck(PushAck),
    /// 全量快照。
    Snapshot(SnapshotPayload),
    /// 通用确认。
    Ack(AckPayload),
    /// 错误。
    Error(SyncErrorPayload),
}

impl SyncMessage {
    /// 返回消息种类（不改变消息内容）。
    #[must_use]
    pub const fn kind(&self) -> SyncMessageKind {
        match self {
            Self::Hello(_) => SyncMessageKind::Hello,
            Self::HelloAck(_) => SyncMessageKind::HelloAck,
            Self::Pull(_) => SyncMessageKind::Pull,
            Self::PullResponse(_) => SyncMessageKind::PullResponse,
            Self::Push(_) => SyncMessageKind::Push,
            Self::PushAck(_) => SyncMessageKind::PushAck,
            Self::Snapshot(_) => SyncMessageKind::Snapshot,
            Self::Ack(_) => SyncMessageKind::Ack,
            Self::Error(_) => SyncMessageKind::Error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_ids_reject_empty_and_control_characters() {
        for bad in ["", "a\nb", "a\u{0000}b", "\u{001f}", "设备\u{0085}"] {
            assert!(matches!(
                WorkspaceId::new(bad),
                Err(ProtocolError::InvalidId { .. })
            ));
            assert!(matches!(
                DeviceId::new(bad),
                Err(ProtocolError::InvalidId { .. })
            ));
        }
        // CJK 标识合法，且按字符串字节序排序确定。
        let cjk = DeviceId::new("设备甲").unwrap();
        assert_eq!(cjk.as_str(), "设备甲");
        // 乙(U+4E59) < 甲(U+7532)：排序按字符串字节序，与直觉中的甲乙顺序无关。
        assert!(DeviceId::new("设备乙").unwrap() < DeviceId::new("设备甲").unwrap());
    }

    #[test]
    fn batch_id_is_deterministic_and_validated() {
        let device = DeviceId::new("dev-a").unwrap();
        let first = BatchId::new(&device, 7).unwrap();
        assert_eq!(first.as_str(), "dev-a-00000000000000000007");
        assert_eq!(first, BatchId::new(&device, 7).unwrap());
        assert_ne!(first, BatchId::new(&device, 8).unwrap());
        assert!(matches!(
            BatchId::parse("bad\nbatch"),
            Err(ProtocolError::InvalidId { .. })
        ));
    }

    #[test]
    fn cursor_is_monotonic_and_saturating() {
        let start = SyncCursor::new(5);
        assert_eq!(start.next(), SyncCursor::new(6));
        assert_eq!(SyncCursor::default(), SyncCursor::new(0));
        assert_eq!(SyncCursor::new(u64::MAX).next(), SyncCursor::new(u64::MAX));
        assert_eq!(start.to_string(), "5");
    }

    #[test]
    fn envelope_keeps_version_and_message_kind_order() {
        let hello = SyncMessage::Hello(SyncEnvelope::new(
            WorkspaceId::new("ws").unwrap(),
            DeviceId::new("dev").unwrap(),
            HelloPayload {
                capabilities: vec![SyncCapability::ContentV1, SyncCapability::TombstoneV1],
            },
        ));
        match &hello {
            SyncMessage::Hello(req) => {
                assert_eq!(req.protocol, PROTOCOL_VERSION);
                assert_eq!(req.sent_at_ms, 0);
                assert!(req.validate().is_ok());
                let mut tampered = req.clone();
                tampered.protocol = "other/9".to_owned();
                assert!(matches!(
                    tampered.validate(),
                    Err(ProtocolError::UnsupportedProtocol(_))
                ));
            }
            other => panic!("期望 Hello，实际是 {:?}", other.kind()),
        }
        // 消息种类顺序固定：排序后仍与声明顺序一致，便于未来字节编码对齐。
        let mut kinds = vec![
            SyncMessageKind::Error,
            SyncMessageKind::Hello,
            SyncMessageKind::Pull,
        ];
        kinds.sort_unstable();
        assert_eq!(
            kinds,
            vec![
                SyncMessageKind::Hello,
                SyncMessageKind::Pull,
                SyncMessageKind::Error
            ]
        );
    }
}
