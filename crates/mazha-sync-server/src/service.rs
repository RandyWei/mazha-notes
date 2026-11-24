//! 同步服务层：鉴权 + 校验 + 去重 + 游标 + 持久化（D100，契约见 `docs/SYNC-SPEC.md` §7.4.2）。
//!
//! 本模块不依赖 HTTP，只处理协议载荷。push 在**克隆的工作区状态**上重建 `CrdtRoster`
//! 并调用 `apply_updates_with_meta`：校验或落盘失败时服务端状态零改动（与 S2 假服务端同语义）。

use std::collections::BTreeMap;

use mazha_crdt::{CrdtRoster, MetaChange};
use mazha_sync::TransportError;
use mazha_sync::wire::MAX_FRAME_BYTES;
use mazha_sync::{
    BatchId, DeviceId, DocFrameMap, HelloAck, HelloRequest, ProtocolError, PullRequest,
    PullResponse, PushAck, PushRequest, SnapshotPayload, SyncCapability, SyncCursor, SyncEnvelope,
    SyncErrorCode, SyncErrorPayload, WorkspaceId, error_code_for,
};

use crate::auth::{AuthError, TokenAuth};
use crate::store::{LogEntry, StoreError, SyncStore, WorkspaceState};

/// 与 HTTP 层无关的同步服务：组合存储与鉴权，复用 S2 的协议类型。
#[derive(Debug)]
pub struct SyncService<S> {
    /// 工作区状态存储。
    store: S,
    /// 设备 token → 工作区授权表。
    auth: TokenAuth,
    /// 单帧大小上限（握手时回报给客户端）。
    max_frame_bytes: u64,
}

impl<S: SyncStore> SyncService<S> {
    /// 构造服务，帧上限取 wire 默认值。
    #[must_use]
    pub fn new(store: S, auth: TokenAuth) -> Self {
        Self {
            store,
            auth,
            max_frame_bytes: u64::try_from(MAX_FRAME_BYTES).unwrap_or(u64::MAX),
        }
    }

    /// 覆盖单帧上限（用于测试拒绝超大帧）。
    #[must_use]
    pub const fn with_max_frame_bytes(mut self, max_frame_bytes: u64) -> Self {
        self.max_frame_bytes = max_frame_bytes;
        self
    }

    /// 只读访问底层存储（诊断/测试用）。
    #[must_use]
    pub const fn store(&self) -> &S {
        &self.store
    }

    /// 握手：校验能力、登记会话并回报当前游标。
    ///
    /// # Errors
    /// 信封非法/未授权/能力与协议不符/存储失败时返回对应 [`TransportError`]。
    pub fn hello(
        &self,
        req: &HelloRequest,
        token: Option<&str>,
    ) -> Result<HelloAck, TransportError> {
        req.validate()?;
        self.authorize(token, &req.workspace_id)?;
        if !req
            .payload
            .capabilities
            .contains(&SyncCapability::ContentV1)
        {
            return Err(TransportError::server(
                SyncErrorCode::InvalidRequest,
                "客户端未声明 content_v1 能力",
            ));
        }

        let mut state = self.load(&req.workspace_id)?;
        // 会话号只依赖服务端的持久化计数器：多设备握手互不覆盖，重启后继续递增。
        state.session_counter = state.session_counter.saturating_add(1);
        let ack = HelloAck {
            session_id: format!("sess-{}", state.session_counter),
            cursor: state.cursor,
            requires_snapshot: false,
            max_frame_bytes: self.max_frame_bytes,
        };
        self.store
            .save(&req.workspace_id, &state)
            .map_err(|error| server_from_store(&error))?;
        Ok(ack)
    }

    /// 增量拉取：按 `since` 回放元数据变更，并返回当前全量内容帧。
    ///
    /// # Errors
    /// 游标过旧/超前、未授权、信封非法或存储失败时返回对应 [`TransportError`]。
    pub fn pull(
        &self,
        req: &PullRequest,
        token: Option<&str>,
    ) -> Result<PullResponse, TransportError> {
        req.validate()?;
        self.authorize(token, &req.workspace_id)?;
        if req.payload.session_id.is_empty() {
            return Err(TransportError::server(
                SyncErrorCode::InvalidRequest,
                "缺少会话 ID",
            ));
        }
        let state = self.load(&req.workspace_id)?;
        let since = req.payload.cursor;
        if since < state.log_floor {
            return Err(TransportError::server(
                SyncErrorCode::CursorTooOld,
                format!("{since} < {}", state.log_floor),
            ));
        }
        if since > state.cursor {
            return Err(TransportError::server(
                SyncErrorCode::SnapshotRequired,
                format!("客户端游标 {since} 超前于服务端 {}", state.cursor),
            ));
        }
        // 元数据变更按日志顺序回放；CrdtRoster 内部按 op_id 排序应用，顺序不影响收敛。
        let meta_changes = state
            .log
            .iter()
            .filter(|entry| entry.cursor > since)
            .flat_map(|entry| entry.meta_changes.iter().cloned())
            .collect();
        Ok(PullResponse {
            cursor: state.cursor,
            frames: state.frames.clone(),
            meta_changes,
        })
    }

    /// 幂等推送：先按 `(device, batch)` 去重，再原子校验并提交。
    ///
    /// # Errors
    /// 坏帧/坏元数据/超大帧/未授权/存储失败时返回对应 [`TransportError`]，服务端状态零改动。
    pub fn push(&self, req: &PushRequest, token: Option<&str>) -> Result<PushAck, TransportError> {
        req.validate()?;
        self.authorize(token, &req.workspace_id)?;
        let state = self.load(&req.workspace_id)?;
        let key = (req.device_id.clone(), req.payload.batch_id.clone());
        if let Some(first_cursor) = state.dedup.get(&key) {
            // 重复批次：返回首次 ack，绝不再次写日志或推进游标。
            return Ok(PushAck {
                cursor: *first_cursor,
                duplicate: true,
            });
        }
        for frame in req.payload.frames.values() {
            let size = u64::try_from(frame.len()).unwrap_or(u64::MAX);
            if size > self.max_frame_bytes {
                return Err(TransportError::server(
                    SyncErrorCode::BadFrame,
                    format!("帧大小 {size} 超过 {}", self.max_frame_bytes),
                ));
            }
        }
        let (next, cursor) = commit(
            &state,
            &req.payload.frames,
            &req.payload.meta_changes,
            Some(key),
        )?;
        self.store
            .save(&req.workspace_id, &next)
            .map_err(|error| server_from_store(&error))?;
        Ok(PushAck {
            cursor,
            duplicate: false,
        })
    }

    /// 全量快照请求：方向 0 的信封只携带身份，返回当前全量状态。
    ///
    /// # Errors
    /// 未授权、信封非法或存储失败时返回对应 [`TransportError`]。
    pub fn snapshot_request(
        &self,
        req: &SyncEnvelope<()>,
        token: Option<&str>,
    ) -> Result<SnapshotPayload, TransportError> {
        req.validate()?;
        self.authorize(token, &req.workspace_id)?;
        let state = self.load(&req.workspace_id)?;
        Ok(SnapshotPayload {
            frames: state.frames,
            meta: state.meta,
            cursor: state.cursor,
        })
    }

    /// 只读读取工作区状态（测试断言游标/墓碑/日志用）。
    ///
    /// # Errors
    /// 存储失败时返回 `Internal`。
    pub fn state(&self, workspace: &WorkspaceId) -> Result<WorkspaceState, TransportError> {
        self.load(workspace)
    }

    /// 初始化/测试用：直接种入文本文档并推进游标。
    ///
    /// # Errors
    /// 坏帧或存储失败时返回对应 [`TransportError`]。
    pub fn seed_text(
        &self,
        workspace: &WorkspaceId,
        id: &str,
        text: &str,
    ) -> Result<SyncCursor, TransportError> {
        let state = self.load(workspace)?;
        let mut frames = DocFrameMap::new();
        frames.insert(
            id.to_owned(),
            mazha_crdt::CrdtDoc::from_text(text).encode_update(),
        );
        let (next, cursor) = commit(&state, &frames, &[], None)?;
        self.store
            .save(workspace, &next)
            .map_err(|error| server_from_store(&error))?;
        Ok(cursor)
    }

    /// 鉴权入口：把 `AuthError` 映射为协议层 `Unauthorized`。
    fn authorize(
        &self,
        token: Option<&str>,
        workspace: &WorkspaceId,
    ) -> Result<(), TransportError> {
        self.auth
            .authorize(token, workspace)
            .map_err(|error| match error {
                AuthError::MissingToken | AuthError::Forbidden => TransportError::server(
                    SyncErrorCode::Unauthorized,
                    format!("工作区 {workspace} 鉴权失败：{error}"),
                ),
            })
    }

    /// 加载状态并把存储错误映射为 `Internal`。
    fn load(&self, workspace: &WorkspaceId) -> Result<WorkspaceState, TransportError> {
        self.store
            .load(workspace)
            .map_err(|error| server_from_store(&error))
    }
}

/// 生成错误载荷：服务端各处统一用它编码 wire `Error` 消息。
#[must_use]
pub fn error_payload(error: &TransportError) -> SyncErrorPayload {
    match error {
        TransportError::Server { code, message } => SyncErrorPayload {
            code: *code,
            message: message.clone(),
            cursor: None,
        },
        TransportError::Protocol(protocol) => SyncErrorPayload {
            code: SyncErrorCode::InvalidRequest,
            message: protocol.to_string(),
            cursor: None,
        },
        TransportError::Disconnected { reason } => SyncErrorPayload {
            code: SyncErrorCode::Internal,
            message: reason.clone(),
            cursor: None,
        },
        TransportError::InvalidResponse { reason } => SyncErrorPayload {
            code: SyncErrorCode::InvalidRequest,
            message: reason.clone(),
            cursor: None,
        },
    }
}

/// 在状态副本上提交一批变更，返回「新状态 + 新游标」；失败时调用方的状态不变。
fn commit(
    state: &WorkspaceState,
    frames: &DocFrameMap,
    changes: &[MetaChange],
    dedup_key: Option<(DeviceId, BatchId)>,
) -> Result<(WorkspaceState, SyncCursor), TransportError> {
    // 先在副本上重建 roster：坏帧/坏元数据只影响副本，服务端真相保持不变。
    let mut roster = roster_from_state(state).map_err(|error| server_from_crdt(&error))?;
    roster
        .apply_updates_with_meta(frames, changes)
        .map_err(|error| server_from_crdt(&error))?;

    let mut next = state.clone();
    // 用 roster 的最新全量帧覆盖缓存，保证后续 pull/snapshot 返回的是收敛后的状态。
    next.frames = roster
        .encode_diff(&BTreeMap::new())
        .map_err(|error| server_from_crdt(&error))?;
    next.meta = roster.meta_snapshot();
    next.cursor = next.cursor.next();
    next.log.push(LogEntry {
        cursor: next.cursor,
        frames: frames.clone(),
        meta_changes: changes.to_vec(),
    });
    if let Some(key) = dedup_key {
        next.dedup.insert(key, next.cursor);
    }
    // 只返回新状态：落盘由调用方在拿到新状态后执行，失败则内存状态也保持不变。
    // 先复制游标再移动 next，避免同一表达式里的移动与借用冲突。
    let cursor = next.cursor;
    Ok((next, cursor))
}

/// 从持久化状态重建 `CrdtRoster`：先应用全量帧，再以墓碑变更覆盖（墓碑 wins）。
fn roster_from_state(state: &WorkspaceState) -> Result<CrdtRoster, mazha_crdt::CrdtError> {
    let mut roster = CrdtRoster::new();
    let meta_changes: Vec<MetaChange> = state
        .meta
        .tombstones()
        .values()
        .cloned()
        .map(MetaChange::Tombstone)
        .collect();
    roster.apply_updates_with_meta(&state.frames, &meta_changes)?;
    Ok(roster)
}

/// 把 CRDT 错误映射为协议错误码（坏帧/坏元数据分层）。
fn server_from_crdt(error: &mazha_crdt::CrdtError) -> TransportError {
    TransportError::server(error_code_for(error), error.to_string())
}

/// 把存储错误映射为 `Internal`（不向客户端泄漏磁盘路径）。
fn server_from_store(error: &StoreError) -> TransportError {
    TransportError::server(SyncErrorCode::Internal, error.to_string())
}

/// 把协议错误映射为 `InvalidRequest`（解码失败时的兜底）。
#[allow(dead_code)]
fn server_from_protocol(error: &ProtocolError) -> TransportError {
    TransportError::server(SyncErrorCode::InvalidRequest, error.to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;

    use mazha_crdt::{CrdtDoc, OpId, RosterMeta, Tombstone, TombstoneReason};
    use mazha_sync::{HelloPayload, PROTOCOL_VERSION, PullPayload, PushPayload, SyncCapability};

    use super::*;
    use crate::auth::TokenRule;
    use crate::store::FileSyncStore;

    fn temp_root(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("mazha-service-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn service(root: &PathBuf) -> SyncService<FileSyncStore> {
        let auth = TokenAuth::new([TokenRule {
            token: "dev-token".to_owned(),
            workspaces: vec![
                WorkspaceId::new("ws-1").unwrap(),
                WorkspaceId::new("ws-2").unwrap(),
            ],
        }]);
        SyncService::new(FileSyncStore::new(root), auth)
    }

    fn envelope<P>(payload: P) -> SyncEnvelope<P> {
        SyncEnvelope::new(
            WorkspaceId::new("ws-1").unwrap(),
            DeviceId::new("dev-a").unwrap(),
            payload,
        )
    }

    fn hello_request() -> HelloRequest {
        envelope(HelloPayload {
            capabilities: vec![SyncCapability::ContentV1, SyncCapability::TombstoneV1],
        })
    }

    fn pull_request(session: &str, cursor: u64) -> PullRequest {
        envelope(PullPayload {
            session_id: session.to_owned(),
            cursor: SyncCursor::new(cursor),
            state_vectors: BTreeMap::new(),
            known_metadata: RosterMeta::new(),
        })
    }

    #[test]
    fn hello_requires_capability_and_workspace_authorization() {
        let root = temp_root("hello");
        let service = service(&root);
        let ws1 = WorkspaceId::new("ws-1").unwrap();
        let ws2 = WorkspaceId::new("ws-2").unwrap();
        let ack = service.hello(&hello_request(), Some("dev-token")).unwrap();
        assert_eq!(ack.session_id, "sess-1");
        assert_eq!(ack.cursor, SyncCursor::new(0));
        assert_eq!(
            ack.max_frame_bytes,
            u64::from(u32::try_from(MAX_FRAME_BYTES).unwrap())
        );
        assert_eq!(
            service
                .hello(&hello_request(), Some("dev-token"))
                .unwrap()
                .session_id,
            "sess-2"
        );

        let mut req = hello_request();
        req.payload.capabilities.clear();
        assert_eq!(
            service.hello(&req, Some("dev-token")).unwrap_err().code(),
            Some(SyncErrorCode::InvalidRequest)
        );
        assert_eq!(
            service.hello(&hello_request(), None).unwrap_err().code(),
            Some(SyncErrorCode::Unauthorized)
        );
        let mut cross = hello_request();
        cross.workspace_id = ws2.clone();
        assert!(service.hello(&cross, Some("dev-token")).is_ok());
        // ws-1 与 ws-2 的会话计数互不影响：按工作区独立持久化。
        assert_eq!(service.state(&ws1).unwrap().session_counter, 2);
        assert_eq!(service.state(&ws2).unwrap().session_counter, 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn push_pull_and_duplicate_keep_cursor_and_log_consistent() {
        let root = temp_root("push");
        let service = service(&root);
        let ws = WorkspaceId::new("ws-1").unwrap();
        let session = service
            .hello(&hello_request(), Some("dev-token"))
            .unwrap()
            .session_id;
        let batch = BatchId::new(&DeviceId::new("dev-a").unwrap(), 1).unwrap();
        let mut frames = DocFrameMap::new();
        frames.insert(
            "笔记/甲.md".to_owned(),
            CrdtDoc::from_text("甲内容").encode_update(),
        );
        let request = envelope(PushPayload {
            batch_id: batch.clone(),
            frames,
            meta_changes: Vec::new(),
        });
        let ack = service.push(&request, Some("dev-token")).unwrap();
        assert_eq!(ack.cursor, SyncCursor::new(1));
        assert!(!ack.duplicate);
        let duplicate = service.push(&request, Some("dev-token")).unwrap();
        assert!(duplicate.duplicate);
        assert_eq!(duplicate.cursor, ack.cursor);

        let response = service
            .pull(&pull_request(&session, 0), Some("dev-token"))
            .unwrap();
        assert_eq!(response.cursor, SyncCursor::new(1));
        assert_eq!(response.frames.len(), 1);
        let state = service.state(&ws).unwrap();
        assert_eq!(state.log.len(), 1);
        assert_eq!(state.dedup.len(), 1);
        assert_eq!(
            state.dedup.values().next().copied(),
            Some(SyncCursor::new(1))
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn bad_frame_push_leaves_state_untouched() {
        let root = temp_root("bad-frame");
        let service = service(&root);
        let ws = WorkspaceId::new("ws-1").unwrap();
        let before = service.state(&ws).unwrap();
        let mut frames = DocFrameMap::new();
        frames.insert("bad.md".to_owned(), vec![0xff, 0xff, 0xff]);
        let request = envelope(PushPayload {
            batch_id: BatchId::new(&DeviceId::new("dev-a").unwrap(), 1).unwrap(),
            frames,
            meta_changes: Vec::new(),
        });
        let error = service.push(&request, Some("dev-token")).unwrap_err();
        assert_eq!(error.code(), Some(SyncErrorCode::BadFrame));
        assert_eq!(service.state(&ws).unwrap(), before);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn tombstone_does_not_revive_after_later_content_frame() {
        let root = temp_root("tombstone");
        let service = service(&root);
        let ws = WorkspaceId::new("ws-1").unwrap();
        service.seed_text(&ws, "notes/deleted.md", "原文").unwrap();
        let tombstone = Tombstone::new("notes/deleted.md", OpId::new(9, "dev-b").unwrap())
            .unwrap()
            .with_reason(TombstoneReason::Delete)
            .with_conflict_frame(CrdtDoc::from_text("被删前内容").encode_update())
            .unwrap();
        let request = envelope(PushPayload {
            batch_id: BatchId::new(&DeviceId::new("dev-a").unwrap(), 1).unwrap(),
            frames: BTreeMap::new(),
            meta_changes: vec![MetaChange::Tombstone(tombstone)],
        });
        service.push(&request, Some("dev-token")).unwrap();

        // 随后离线设备推送同一路径的低 op_id upsert，不得复活活动文档。
        let revive = envelope(PushPayload {
            batch_id: BatchId::new(&DeviceId::new("dev-a").unwrap(), 2).unwrap(),
            frames: BTreeMap::new(),
            meta_changes: vec![MetaChange::Upsert {
                doc_id: "notes/deleted.md".to_owned(),
                op_id: OpId::new(3, "dev-a").unwrap(),
                frame: CrdtDoc::from_text("离线编辑").encode_update(),
            }],
        });
        service.push(&revive, Some("dev-token")).unwrap();
        let state = service.state(&ws).unwrap();
        assert!(!state.frames.contains_key("notes/deleted.md"));
        assert!(state.meta.is_tombstoned("notes/deleted.md"));
        // 三份副本分别来自：seed 的「原文」、墓碑自带的「被删前内容」、
        // 以及被墓碑挡下的低 op_id upsert「离线编辑」；D95 禁止静默丢数据，全部保留。
        assert_eq!(
            state
                .meta
                .get("notes/deleted.md")
                .unwrap()
                .conflict_frames()
                .len(),
            3
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cursor_too_old_and_snapshot_required_are_distinguished() {
        let root = temp_root("cursor");
        let service = service(&root);
        let ws = WorkspaceId::new("ws-1").unwrap();
        service.seed_text(&ws, "a.md", "甲").unwrap();
        service.seed_text(&ws, "b.md", "乙").unwrap();

        // 先抬高 log_floor 再验证过旧游标；用状态副本直接落盘以模拟历史裁剪。
        let mut state = service.state(&ws).unwrap();
        state.log_floor = SyncCursor::new(2);
        service.store().save(&ws, &state).unwrap();
        let session = service
            .hello(&hello_request(), Some("dev-token"))
            .unwrap()
            .session_id;
        assert_eq!(
            service
                .pull(&pull_request(&session, 1), Some("dev-token"))
                .unwrap_err()
                .code(),
            Some(SyncErrorCode::CursorTooOld)
        );
        assert_eq!(
            service
                .pull(&pull_request(&session, 99), Some("dev-token"))
                .unwrap_err()
                .code(),
            Some(SyncErrorCode::SnapshotRequired)
        );
        let snapshot = service
            .snapshot_request(&envelope(()), Some("dev-token"))
            .unwrap();
        assert_eq!(snapshot.frames.len(), 2);
        assert_eq!(snapshot.cursor, SyncCursor::new(2));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn envelope_protocol_must_match() {
        let mut request = hello_request();
        request.protocol = PROTOCOL_VERSION.to_owned() + "-bad";
        let root = temp_root("protocol");
        let service = service(&root);
        assert!(matches!(
            service.hello(&request, Some("dev-token")),
            Err(TransportError::Protocol(
                ProtocolError::UnsupportedProtocol(_)
            ))
        ));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn error_payload_keeps_code_and_message() {
        let error = TransportError::server(SyncErrorCode::BadMetadata, "坏元数据");
        let payload = error_payload(&error);
        assert_eq!(payload.code, SyncErrorCode::BadMetadata);
        assert_eq!(payload.message, "坏元数据");
        assert_eq!(payload.cursor, None);
    }
}
