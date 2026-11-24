//! 本地同步引擎错误分类（D101，契约见 `docs/SYNC-SPEC.md` §5.9）。
//!
//! 只区分「上游错误透传」与「本地状态文件损坏」两类语义：坏状态文件必须让调用方
//! 明确感知，绝不静默回退成空状态（那会丢掉游标/批次号并触发服务端去重吞编辑）。

use mazha_crdt::CrdtError;
use mazha_store::StoreError;
use mazha_sync::wire::WireError;
use mazha_sync::{ProtocolError, SyncSessionError};

/// 本地同步错误。
#[derive(Debug, thiserror::Error)]
pub enum LocalSyncError {
    /// 存储层错误（读写工作区文件或状态文件）。
    #[error("本地存储失败：{0}")]
    Store(#[from] StoreError),

    /// 同步会话错误（传输、快照确认门槛等）。
    #[error("同步会话失败：{0}")]
    Session(#[from] SyncSessionError),

    /// 协议错误（检查点身份不匹配等）。
    #[error("同步协议失败：{0}")]
    Protocol(#[from] ProtocolError),

    /// CRDT 层错误（帧非法、名册还原失败）。
    #[error("CRDT 处理失败：{0}")]
    Crdt(#[from] CrdtError),

    /// 状态文件损坏或版本不支持：必须提示用户，不回退空状态。
    #[error("本地同步状态文件损坏：{0}")]
    CorruptState(String),

    /// 磁盘文件不是合法 UTF-8 文本：Markdown 源真相要求字节可无损转字符串。
    #[error("文档不是合法 UTF-8 文本：{0}")]
    InvalidText(String),
}

impl From<WireError> for LocalSyncError {
    /// wire 助手只可能在解析损坏字节时失败，统一归类为状态文件损坏。
    fn from(error: WireError) -> Self {
        Self::CorruptState(error.to_string())
    }
}
