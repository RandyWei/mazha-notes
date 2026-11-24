//! 轻量订阅事件与 JSON 编解码（D108）。
//!
//! 事件只表达“哪几个工作区、哪些文档、游标提示是多少”，不含文档正文、不含 CRDT 操作，
//! 也不复用九种同步消息。收到事件的一方仍然要走既有 `SyncNow` 拉取合并流程。

use serde::{Deserialize, Serialize};

use crate::subscription::SubscriptionError;

/// JSON 事件的固定种类字段；未知种类一律拒绝，避免前后端版本漂移时误动作。
const KIND_WORKSPACE_CHANGED: &str = "workspace_changed";

/// 订阅事件：工作区有变更的提示（纯数据）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncEvent {
    /// 服务端接受了推送，订阅者可据此发起一轮非破坏性同步。
    WorkspaceChanged {
        /// 受影响的工作区 ID（服务端按订阅者授权过滤后下发）。
        workspaces: Vec<String>,
        /// 受影响的文档 ID（工作区相对路径），仅作 UI 提示。
        doc_ids: Vec<String>,
        /// 服务端最新游标提示；客户端不得据此跳过拉取值。
        cursor_hint: Option<u64>,
    },
}

/// 线上 JSON 结构；字段保持稳定，新增字段必须 additive 且旧端可忽略。
#[derive(Debug, Serialize, Deserialize)]
struct EventWire {
    /// 事件种类。
    kind: String,
    /// 工作区 ID 列表。
    workspaces: Vec<String>,
    /// 文档 ID 列表。
    doc_ids: Vec<String>,
    /// 游标提示。
    cursor_hint: Option<u64>,
}

/// 把订阅事件编码为 JSON 文本帧载荷。
///
/// # Errors
/// 序列化失败（理论上不会发生）时返回 [`SubscriptionError::Protocol`]。
pub fn encode_event_json(event: &SyncEvent) -> Result<String, SubscriptionError> {
    let SyncEvent::WorkspaceChanged {
        workspaces,
        doc_ids,
        cursor_hint,
    } = event;
    let wire = EventWire {
        kind: KIND_WORKSPACE_CHANGED.to_owned(),
        workspaces: workspaces.clone(),
        doc_ids: doc_ids.clone(),
        cursor_hint: *cursor_hint,
    };
    serde_json::to_string(&wire)
        .map_err(|error| SubscriptionError::Protocol(format!("订阅事件编码失败：{error}")))
}

/// 解析 JSON 文本帧载荷为订阅事件。
///
/// 空白/控制字符的工作区与文档 ID 直接拒绝：它们会污染日志与 UI，且无法定位文档。
///
/// # Errors
/// JSON 非法、种类未知或字段内容非法时返回 [`SubscriptionError::Protocol`]。
pub fn decode_event_json(raw: &str) -> Result<SyncEvent, SubscriptionError> {
    let wire: EventWire = serde_json::from_str(raw)
        .map_err(|error| SubscriptionError::Protocol(format!("订阅事件 JSON 非法：{error}")))?;
    match wire.kind.as_str() {
        KIND_WORKSPACE_CHANGED => {
            if wire.workspaces.is_empty()
                || wire.workspaces.iter().any(|id| invalid_id(id))
                || wire.doc_ids.iter().any(|id| invalid_id(id))
            {
                return Err(SubscriptionError::Protocol("订阅事件含非法 ID".into()));
            }
            Ok(SyncEvent::WorkspaceChanged {
                workspaces: wire.workspaces,
                doc_ids: wire.doc_ids,
                cursor_hint: wire.cursor_hint,
            })
        }
        other => Err(SubscriptionError::Protocol(format!(
            "未知订阅事件种类：{other}"
        ))),
    }
}

/// ID 合法性：非空、无控制字符、无空白（路径里的普通空格由调用方决定是否允许）。
fn invalid_id(raw: &str) -> bool {
    raw.is_empty() || raw.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_json_roundtrip() {
        let event = SyncEvent::WorkspaceChanged {
            workspaces: vec!["ws-1".into(), "中文工作区".into()],
            doc_ids: vec!["notes/🪄.md".into()],
            cursor_hint: Some(42),
        };
        let json = encode_event_json(&event).unwrap();
        assert_eq!(decode_event_json(&json).unwrap(), event);
    }

    #[test]
    fn bad_event_json_is_rejected() {
        for raw in [
            "not json",
            r#"{"kind":"unknown","workspaces":["ws-1"],"doc_ids":[],"cursor_hint":null}"#,
            r#"{"kind":"workspace_changed","workspaces":[],"doc_ids":[],"cursor_hint":null}"#,
            r#"{"kind":"workspace_changed","workspaces":["a\nb"],"doc_ids":[],"cursor_hint":null}"#,
        ] {
            assert!(decode_event_json(raw).is_err(), "{raw}");
        }
    }
}
