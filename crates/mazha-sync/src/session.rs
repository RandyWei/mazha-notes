//! 客户端同步会话状态机（D99，契约见 `docs/SYNC-SPEC.md` §5.7）。
//!
//! `SyncSession` 拥有本地 [`CrdtRoster`]，维护握手状态、服务端游标、确定性批次号与
//! `OpId` 逻辑时钟。离线改动先落到本地 roster 再进 outbox；重连后按序重放。
//! 下游数据**只通过 `CrdtRoster::apply_updates_with_meta` 应用**（先元数据后内容、
//! 两阶段提交），绝不调用不查墓碑的旧 `apply_updates`。

use std::collections::{BTreeMap, VecDeque};

use mazha_crdt::{CrdtError, CrdtRoster, DocId, MetaChange, OpId};

use crate::protocol::{
    BatchId, DeviceId, DocFrameMap, HelloPayload, HelloRequest, ProtocolError, PullPayload,
    PullRequest, PullResponse, PushPayload, PushRequest, SnapshotPayload, SyncCapability,
    SyncCursor, SyncEnvelope, WorkspaceId,
};
use crate::transport::{SyncTransport, TransportError};

/// 会话层错误：传输错误、CRDT 错误与需要用户确认的快照冲突。
#[derive(Debug, thiserror::Error)]
pub enum SyncSessionError {
    /// 传输层失败（断线、服务端错误码、非法应答）。
    #[error("同步传输失败：{0}")]
    Transport(#[from] TransportError),

    /// CRDT 层应用失败（坏帧/坏元数据已由两阶段提交保证零改动）。
    #[error("CRDT 应用失败：{0}")]
    Crdt(#[from] CrdtError),

    /// 协议/标识校验失败。
    #[error("同步协议错误：{0}")]
    Protocol(#[from] ProtocolError),

    /// 尚未握手就尝试拉取/推送。
    #[error("尚未握手，请先调用 handshake")]
    HandshakeRequired,

    /// 快照回退需要用户确认：本地有未确认出站批次，不能静默覆盖。
    #[error("快照回退需用户确认：本地有 {pending_batches} 个未确认出站批次")]
    SnapshotNeedsConfirmation {
        /// 当前 outbox 中的批次数。
        pending_batches: usize,
    },
}

impl SyncSessionError {
    /// 是否需要用户确认后才能继续（UI 据此弹合并/覆盖选择）。
    #[must_use]
    pub const fn needs_confirmation(&self) -> bool {
        matches!(self, Self::SnapshotNeedsConfirmation { .. })
    }
}

/// 待推送批次：确定性批次号 + 内容帧 + 元数据变更。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingBatch {
    /// 批次号。
    batch_id: BatchId,
    /// 内容帧（标准 Yjs v1）。
    frames: DocFrameMap,
    /// 元数据变更。
    meta_changes: Vec<MetaChange>,
}

impl PendingBatch {
    /// 批次号。
    #[must_use]
    pub const fn batch_id(&self) -> &BatchId {
        &self.batch_id
    }

    /// 内容帧（只读）。
    #[must_use]
    pub const fn frames(&self) -> &DocFrameMap {
        &self.frames
    }

    /// 元数据变更（只读）。
    #[must_use]
    pub fn meta_changes(&self) -> &[MetaChange] {
        &self.meta_changes
    }
}

/// 握手结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandshakeOutcome {
    /// 服务端分配的会话 ID。
    pub session_id: String,
    /// 服务端当前游标（诊断，**不作为首轮拉取起点**）。
    pub server_cursor: SyncCursor,
    /// 服务端是否要求先做快照引导。
    pub requires_snapshot: bool,
}

/// 拉取结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullOutcome {
    /// 应用成功后确认的游标。
    pub cursor: SyncCursor,
    /// 实际受影响的文档 ID（升序）。
    pub applied: Vec<DocId>,
    /// 应用的元数据变更条数。
    pub meta_changes: usize,
    /// 是否通过快照回退完成（游标过旧时的自动路径）。
    pub via_snapshot: bool,
}

/// 推送结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushOutcome {
    /// 最后一个成功批次的 ack 游标；无批次时为本地游标。
    pub cursor: SyncCursor,
    /// 本轮出队的批次号（按发送顺序）。
    pub confirmed: Vec<BatchId>,
    /// 其中被服务端判定为重复投递的批次数。
    pub duplicates: usize,
}

/// 快照应用结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotOutcome {
    /// 快照游标。
    pub cursor: SyncCursor,
    /// 实际应用的文档 ID（升序）。
    pub applied: Vec<DocId>,
}

/// `sync_once` 的组合结果（每一步未发生时为 `None`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncOnceOutcome {
    /// 握手结果（仅需要握手时执行）。
    pub handshake: Option<HandshakeOutcome>,
    /// 快照结果（服务端要求或游标过旧时执行）。
    pub snapshot: Option<SnapshotOutcome>,
    /// 增量拉取结果（未走快照路径时执行）。
    pub pull: Option<PullOutcome>,
    /// 出站重放结果。
    pub push: Option<PushOutcome>,
}

/// 客户端同步会话：握 → 拉 → 推 的状态机，离线可 stage，重连可重放。
#[derive(Debug)]
pub struct SyncSession {
    /// 工作区标识（与服务端数据隔离边界一致）。
    workspace_id: WorkspaceId,
    /// 本设备标识（批次号前缀与 OpId 来源）。
    device_id: DeviceId,
    /// 本地名册（内容 + 墓碑）。
    roster: CrdtRoster,
    /// 最后一次成功应用下游数据/收到 ack 的游标。
    last_ack_cursor: SyncCursor,
    /// 服务端最近一次告知的游标（仅诊断，不参与拉取起点）。
    server_cursor: SyncCursor,
    /// 未确认出站批次（FIFO）。
    outbox: VecDeque<PendingBatch>,
    /// 会话 ID；`None` 表示未握手。
    session_id: Option<String>,
    /// 是否需要重新握手。
    needs_handshake: bool,
    /// 服务端是否要求快照引导。
    requires_snapshot: bool,
    /// 本地 OpId 逻辑时钟（`OpId(clock, device)`）。
    op_clock: u64,
    /// 本地批次计数器（与设备 ID 共同构成确定性 `BatchId`）。
    batch_counter: u64,
}

impl SyncSession {
    /// 用已有本地名册构造会话；初始状态为未握手、游标 0。
    #[must_use]
    pub fn new(workspace_id: WorkspaceId, device_id: DeviceId, roster: CrdtRoster) -> Self {
        Self {
            workspace_id,
            device_id,
            roster,
            last_ack_cursor: SyncCursor::new(0),
            server_cursor: SyncCursor::new(0),
            outbox: VecDeque::new(),
            session_id: None,
            needs_handshake: true,
            requires_snapshot: false,
            op_clock: 0,
            batch_counter: 0,
        }
    }

    /// 工作区 ID。
    #[must_use]
    pub const fn workspace_id(&self) -> &WorkspaceId {
        &self.workspace_id
    }

    /// 设备 ID。
    #[must_use]
    pub const fn device_id(&self) -> &DeviceId {
        &self.device_id
    }

    /// 只读本地名册。
    #[must_use]
    pub const fn roster(&self) -> &CrdtRoster {
        &self.roster
    }

    /// 可变本地名册。
    ///
    /// 注意：直接通过本方法改动名册不会自动进入 outbox；请改用 `stage_*` 方法
    /// 以保证离线改动可被重放（也保证快照回退时能判断是否安全）。
    pub fn roster_mut(&mut self) -> &mut CrdtRoster {
        &mut self.roster
    }

    /// 取回本地名册所有权（测试/持久化用）。
    #[must_use]
    pub fn into_roster(self) -> CrdtRoster {
        self.roster
    }

    /// 最后确认的游标。
    #[must_use]
    pub const fn last_ack_cursor(&self) -> SyncCursor {
        self.last_ack_cursor
    }

    /// 服务端最近告知的游标（诊断）。
    #[must_use]
    pub const fn server_cursor(&self) -> SyncCursor {
        self.server_cursor
    }

    /// 当前会话 ID（未握手时为 `None`）。
    #[must_use]
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// 是否尚未握手。
    #[must_use]
    pub const fn needs_handshake(&self) -> bool {
        self.needs_handshake
    }

    /// 未确认出站批次（FIFO）。
    #[must_use]
    pub const fn pending_batches(&self) -> &VecDeque<PendingBatch> {
        &self.outbox
    }

    /// 逻辑时钟的当前值（诊断/确定性测试）。
    #[must_use]
    pub const fn op_clock(&self) -> u64 {
        self.op_clock
    }

    /// 产生下一个 `OpId`：时钟单调 +1，设备固定为本会话设备（D98）。
    ///
    /// # Errors
    /// 设备标识非法（类型已保证合法，属防御性检查）时返回
    /// [`ProtocolError::InvalidId`]。
    pub fn next_op_id(&mut self) -> Result<OpId, ProtocolError> {
        self.op_clock = self.op_clock.saturating_add(1);
        Ok(OpId::new(self.op_clock, self.device_id.as_str())?)
    }

    /// 把已校验的批次放入 outbox（内部统一入口，计数器只在成功时推进）。
    fn enqueue(&mut self, frames: DocFrameMap, meta_changes: Vec<MetaChange>) -> BatchId {
        let batch_id = BatchId::new(&self.device_id, self.batch_counter)
            .expect("设备 ID 已由类型校验，批次号拼接不可能非法");
        self.batch_counter = self.batch_counter.saturating_add(1);
        self.outbox.push_back(PendingBatch {
            batch_id: batch_id.clone(),
            frames,
            meta_changes,
        });
        batch_id
    }

    /// 暂存本地改动：先原子应用到本地 roster，再按 FIFO 入 outbox；不触碰 transport。
    ///
    /// 该方法离线可用。调用方传入的帧与元数据会经 `apply_updates_with_meta` 校验，
    /// 坏帧/坏元数据整批拒绝且**不入队**，本地状态也零改动。
    ///
    /// # Errors
    /// 内容帧/元数据非法返回 `Crdt`；批次号构造失败返回 `Protocol`。
    pub fn stage_local_change(
        &mut self,
        frames: &DocFrameMap,
        meta_changes: &[MetaChange],
    ) -> Result<BatchId, SyncSessionError> {
        // 本地先应用：保证 outbox 中的内容与本地可见状态一致，且复用同一个原子边界。
        self.roster.apply_updates_with_meta(frames, meta_changes)?;
        Ok(self.enqueue(frames.clone(), meta_changes.to_vec()))
    }

    /// 便捷暂存：把文档文本整体替换，并生成携带自包含全量帧的 `Upsert`。
    ///
    /// # Errors
    /// ID 非法、编辑失败或 `OpId` 构造失败时返回错误；失败不入队。
    pub fn stage_text(&mut self, doc_id: &str, text: &str) -> Result<BatchId, SyncSessionError> {
        let len = match self.roster.get(doc_id) {
            Ok(doc) => doc.text().chars().count(),
            // 未知 ID 视为新建；ID 合法性由随后的 open_or_create 再校验。
            Err(_) => 0,
        };
        let frame = {
            let doc = self.roster.open_or_create(doc_id)?;
            doc.replace(0, len, text)?;
            doc.encode_update()
        };
        let change = MetaChange::Upsert {
            doc_id: doc_id.to_owned(),
            op_id: self.next_op_id()?,
            frame,
        };
        self.stage_local_change(&BTreeMap::new(), &[change])
    }

    /// 便捷暂存：墓碑化删除（本地立即移除，出站传播墓碑与可恢复副本）。
    ///
    /// # Errors
    /// ID 非法、文档不存在或 `OpId` 构造失败时返回错误；失败不入队。
    pub fn stage_delete(&mut self, doc_id: &str) -> Result<BatchId, SyncSessionError> {
        let op_id = self.next_op_id()?;
        let tombstone = self.roster.remove_with_op(doc_id, op_id)?;
        // remove_with_op 已经修改了本地 roster，这里只入队，避免重复归并同一墓碑。
        Ok(self.enqueue(BTreeMap::new(), vec![MetaChange::Tombstone(tombstone)]))
    }

    /// 握手：登记设备并获取会话 ID 与服务端游标。
    ///
    /// 注意：服务端游标只作诊断，**不写入 `last_ack_cursor`**，否则首轮拉取会跳过
    /// 服务端已有的变更与墓碑。
    ///
    /// # Errors
    /// 传输失败或服务端拒绝时返回 `Transport`。
    pub fn handshake(
        &mut self,
        transport: &mut dyn SyncTransport,
    ) -> Result<HandshakeOutcome, SyncSessionError> {
        let request: HelloRequest = SyncEnvelope::new(
            self.workspace_id.clone(),
            self.device_id.clone(),
            HelloPayload {
                capabilities: vec![
                    SyncCapability::ContentV1,
                    SyncCapability::TombstoneV1,
                    SyncCapability::SnapshotV1,
                ],
            },
        );
        let ack = transport.hello(request)?;
        self.session_id = Some(ack.session_id.clone());
        self.needs_handshake = false;
        self.requires_snapshot = ack.requires_snapshot;
        self.server_cursor = ack.cursor;
        Ok(HandshakeOutcome {
            session_id: ack.session_id,
            server_cursor: ack.cursor,
            requires_snapshot: ack.requires_snapshot,
        })
    }

    /// 增量拉取：应用内容帧 + 元数据变更（一次 `apply_updates_with_meta`），成功后推进游标。
    ///
    /// 遇到 `CursorTooOld` 自动改走快照；若本地有未确认批次，快照会被拒绝并返回
    /// `SnapshotNeedsConfirmation`，交由用户确认，绝不静默覆盖本地改动。
    ///
    /// # Errors
    /// 未握手、传输失败、快照需确认或下游数据非法时返回对应错误（游标原地不动）。
    pub fn pull(
        &mut self,
        transport: &mut dyn SyncTransport,
    ) -> Result<PullOutcome, SyncSessionError> {
        let session_id = self
            .session_id
            .clone()
            .ok_or(SyncSessionError::HandshakeRequired)?;
        let request: PullRequest = SyncEnvelope::new(
            self.workspace_id.clone(),
            self.device_id.clone(),
            PullPayload {
                session_id,
                cursor: self.last_ack_cursor,
                state_vectors: self.roster.state_vectors(),
                known_metadata: self.roster.meta_snapshot(),
            },
        );
        let response: PullResponse = match transport.pull(request) {
            Ok(response) => response,
            Err(error) if error.is_cursor_too_old() => {
                let snapshot = self.apply_snapshot(transport)?;
                return Ok(PullOutcome {
                    cursor: snapshot.cursor,
                    applied: snapshot.applied,
                    meta_changes: 0,
                    via_snapshot: true,
                });
            }
            Err(error) => return Err(error.into()),
        };
        // 唯一的下游应用入口：内部先元数据（按 op_id 排序、墓碑 wins）再内容帧，
        // 两阶段提交保证坏帧/坏元数据零改动；因此游标只能在 Ok 之后推进。
        let applied = self
            .roster
            .apply_updates_with_meta(&response.frames, &response.meta_changes)?;
        self.advance_cursor(response.cursor);
        Ok(PullOutcome {
            cursor: self.last_ack_cursor,
            applied,
            meta_changes: response.meta_changes.len(),
            via_snapshot: false,
        })
    }

    /// 按 FIFO 重放 outbox；重复 ack 视为成功并出队，失败批次保留原位。
    ///
    /// 已成功确认的批次会立即出队（远端已持久化，无法撤销）；当前失败的批次及其后
    /// 所有批次留在 outbox，错误原样返回，等待下次重连重试。
    ///
    /// # Errors
    /// 传输失败或服务端拒绝时返回 `Transport`，outbox 保留失败批次。
    pub fn push(
        &mut self,
        transport: &mut dyn SyncTransport,
    ) -> Result<PushOutcome, SyncSessionError> {
        let start_cursor = self.last_ack_cursor;
        let mut confirmed = Vec::new();
        let mut duplicates = 0;
        while let Some(pending) = self.outbox.front().cloned() {
            let request: PushRequest = SyncEnvelope::new(
                self.workspace_id.clone(),
                self.device_id.clone(),
                PushPayload {
                    batch_id: pending.batch_id().clone(),
                    frames: pending.frames().clone(),
                    meta_changes: pending.meta_changes().to_vec(),
                },
            );
            let ack = transport.push(request)?;
            // 先出队再推进游标：任何失败路径（上面的 ?）都不会丢掉待重放批次。
            self.outbox.pop_front();
            self.advance_cursor(ack.cursor);
            if ack.duplicate {
                duplicates += 1;
            }
            confirmed.push(pending.batch_id().clone());
        }
        Ok(PushOutcome {
            cursor: if confirmed.is_empty() {
                start_cursor
            } else {
                self.last_ack_cursor
            },
            confirmed,
            duplicates,
        })
    }

    /// 便捷同步：握手（若需要）→ 快照（若服务端要求）→ 拉取 → 推送。
    ///
    /// # Errors
    /// 任一步失败即返回；已成功的步骤不可撤销（例如已推送的批次不会回到 outbox）。
    pub fn sync_once(
        &mut self,
        transport: &mut dyn SyncTransport,
    ) -> Result<SyncOnceOutcome, SyncSessionError> {
        let handshake = if self.needs_handshake {
            Some(self.handshake(transport)?)
        } else {
            None
        };
        let snapshot = if self.requires_snapshot {
            Some(self.apply_snapshot(transport)?)
        } else {
            None
        };
        // 快照已包含全量状态，无需再用同一游标拉一次增量。
        let pull = if snapshot.is_none() {
            Some(self.pull(transport)?)
        } else {
            None
        };
        let push = self.push(transport)?;
        Ok(SyncOnceOutcome {
            handshake,
            snapshot,
            pull,
            push: Some(push),
        })
    }

    /// 应用全量快照并替换本地名册。
    ///
    /// 仅当 outbox 为空（无未确认出站批次/本地未同步改动）时允许自动 apply；
    /// 否则返回 `SnapshotNeedsConfirmation`，由上层让用户选择合并/覆盖/新建副本。
    /// 快照在新 roster 上一次原子应用，失败时本地状态与游标不变。
    ///
    /// # Errors
    /// 本地有未确认批次、传输失败或快照数据非法时返回对应错误。
    pub fn apply_snapshot(
        &mut self,
        transport: &mut dyn SyncTransport,
    ) -> Result<SnapshotOutcome, SyncSessionError> {
        if !self.outbox.is_empty() {
            return Err(SyncSessionError::SnapshotNeedsConfirmation {
                pending_batches: self.outbox.len(),
            });
        }
        let snapshot: SnapshotPayload = transport.snapshot()?;
        let changes: Vec<MetaChange> = snapshot
            .meta
            .tombstones()
            .values()
            .cloned()
            .map(MetaChange::Tombstone)
            .collect();
        let mut fresh = CrdtRoster::new();
        let applied = fresh.apply_updates_with_meta(&snapshot.frames, &changes)?;
        self.roster = fresh;
        self.requires_snapshot = false;
        self.advance_cursor(snapshot.cursor);
        Ok(SnapshotOutcome {
            cursor: self.last_ack_cursor,
            applied,
        })
    }

    /// 单调推进游标：收到旧游标也不回退（防御乱序应答）。
    fn advance_cursor(&mut self, next: SyncCursor) {
        if next > self.last_ack_cursor {
            self.last_ack_cursor = next;
        }
        if next > self.server_cursor {
            self.server_cursor = next;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeTransport, InMemorySyncServer};
    use crate::protocol::DocFrameMap;
    use crate::protocol::{HelloAck, PushAck};
    use crate::transport::TransportError;
    use mazha_crdt::Tombstone;
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    /// 测试用 ID。
    fn ids(device: &str) -> (WorkspaceId, DeviceId) {
        (
            WorkspaceId::new("笔记工作区").unwrap(),
            DeviceId::new(device).unwrap(),
        )
    }

    /// 造一个空会话。
    fn session(device: &str) -> SyncSession {
        let (ws, dev) = ids(device);
        SyncSession::new(ws, dev, CrdtRoster::new())
    }

    /// 握手包装：注入 `requires_snapshot` 标志。
    struct FlagTransport {
        /// 真实传输。
        inner: FakeTransport,
        /// 注入的快照标志。
        requires_snapshot: bool,
    }

    impl SyncTransport for FlagTransport {
        fn hello(&mut self, req: HelloRequest) -> Result<HelloAck, TransportError> {
            let mut ack = self.inner.hello(req)?;
            ack.requires_snapshot = self.requires_snapshot;
            Ok(ack)
        }

        fn pull(&mut self, req: PullRequest) -> Result<PullResponse, TransportError> {
            self.inner.pull(req)
        }

        fn push(&mut self, req: PushRequest) -> Result<PushAck, TransportError> {
            self.inner.push(req)
        }

        fn snapshot(&mut self) -> Result<SnapshotPayload, TransportError> {
            self.inner.snapshot()
        }
    }

    /// 拉取应答注入包装：验证坏帧/坏元数据的零改动边界。
    struct InjectTransport {
        /// 真实传输。
        inner: FakeTransport,
        /// 是否注入坏内容帧。
        bad_frame: bool,
        /// 是否注入坏元数据。
        bad_meta: bool,
    }

    impl SyncTransport for InjectTransport {
        fn hello(&mut self, req: HelloRequest) -> Result<HelloAck, TransportError> {
            self.inner.hello(req)
        }

        fn pull(&mut self, req: PullRequest) -> Result<PullResponse, TransportError> {
            let mut response = self.inner.pull(req)?;
            if self.bad_frame {
                response
                    .frames
                    .insert("坏帧.md".to_owned(), vec![0xff, 0xff]);
            }
            if self.bad_meta {
                response.meta_changes.push(MetaChange::Upsert {
                    doc_id: "坏元数据.md".to_owned(),
                    op_id: OpId::new(9, "设备甲").unwrap(),
                    frame: vec![0xff, 0xff],
                });
            }
            Ok(response)
        }

        fn push(&mut self, req: PushRequest) -> Result<PushAck, TransportError> {
            self.inner.push(req)
        }

        fn snapshot(&mut self) -> Result<SnapshotPayload, TransportError> {
            self.inner.snapshot()
        }
    }

    #[test]
    fn handshake_reports_server_cursor_and_snapshot_flag() {
        let server = Rc::new(RefCell::new(InMemorySyncServer::new()));
        server.borrow_mut().seed_text("a.md", "原文").unwrap();
        let (ws, dev) = ids("设备甲");
        let mut transport = FakeTransport::new(&server, ws, dev);
        let mut client = session("设备甲");
        let outcome = client.handshake(&mut transport).unwrap();
        assert_eq!(outcome.session_id, "sess-1");
        assert_eq!(outcome.server_cursor, SyncCursor::new(1));
        assert!(!outcome.requires_snapshot);
        // 握手不得推进客户端确认游标，否则首轮 pull 会跳过服务端内容。
        assert_eq!(client.last_ack_cursor(), SyncCursor::new(0));
        assert!(!client.needs_handshake());
        assert_eq!(client.session_id(), Some("sess-1"));
    }

    #[test]
    fn pull_applies_content_and_tombstone_then_advances_cursor() {
        let server = Rc::new(RefCell::new(InMemorySyncServer::new()));
        server.borrow_mut().seed_text("a.md", "你好").unwrap();
        server
            .borrow_mut()
            .seed_tombstone(Tombstone::new("旧.md", OpId::new(4, "设备乙").unwrap()).unwrap())
            .unwrap();
        let (ws, dev) = ids("设备甲");
        let mut transport = FakeTransport::new(&server, ws, dev);
        let mut client = session("设备甲");
        client.handshake(&mut transport).unwrap();
        let outcome = client.pull(&mut transport).unwrap();

        assert!(!outcome.via_snapshot);
        assert_eq!(outcome.cursor, SyncCursor::new(2));
        assert_eq!(outcome.applied, vec!["a.md", "旧.md"]);
        assert_eq!(outcome.meta_changes, 1);
        assert_eq!(client.last_ack_cursor(), SyncCursor::new(2));
        assert_eq!(client.roster().get("a.md").unwrap().text(), "你好");
        assert!(client.roster().is_tombstoned("旧.md"));
        assert!(!client.roster().contains("旧.md"));
    }

    #[test]
    fn push_persists_batch_and_duplicate_is_idempotent() {
        let server = Rc::new(RefCell::new(InMemorySyncServer::new()));
        let (ws, dev) = ids("设备甲");
        let mut transport = FakeTransport::new(&server, ws.clone(), dev.clone());
        let mut client = session("设备甲");
        client.handshake(&mut transport).unwrap();
        let batch = client.stage_text("笔记/a.md", "你好，世界").unwrap();
        assert_eq!(batch.as_str(), "设备甲-00000000000000000000");
        assert_eq!(client.pending_batches().len(), 1);
        let outcome = client.push(&mut transport).unwrap();
        assert_eq!(outcome.confirmed, vec![batch.clone()]);
        assert_eq!(outcome.cursor, SyncCursor::new(1));
        assert!(client.pending_batches().is_empty());
        assert_eq!(server.borrow().text_of("笔记/a.md").unwrap(), "你好，世界");

        // 同一个 batch_id 再投递一次：服务端去重、不记新日志、ack 游标不变。
        let pending = PendingBatch {
            batch_id: batch,
            frames: BTreeMap::new(),
            meta_changes: Vec::new(),
        };
        let request = SyncEnvelope::new(
            ws,
            dev,
            PushPayload {
                batch_id: pending.batch_id().clone(),
                frames: pending.frames().clone(),
                meta_changes: Vec::new(),
            },
        );
        let ack = transport.push(request).unwrap();
        assert!(ack.duplicate);
        assert_eq!(ack.cursor, SyncCursor::new(1));
        assert_eq!(server.borrow().log_len(), 1);
    }

    #[test]
    fn offline_staged_delete_replays_and_stale_content_does_not_revive() {
        let server = Rc::new(RefCell::new(InMemorySyncServer::new()));
        server.borrow_mut().seed_text("a.md", "共同原文").unwrap();
        let (ws, dev_a) = ids("dev-a");
        let (_, dev_b) = ids("dev-b");
        let (mut a_transport, mut b_transport) =
            FakeTransport::pair(&server, ws.clone(), dev_a.clone(), dev_b.clone());

        let mut a = SyncSession::new(ws.clone(), dev_a, CrdtRoster::new());
        a.handshake(&mut a_transport).unwrap();
        a.pull(&mut a_transport).unwrap();
        let delete_batch = a.stage_delete("a.md").unwrap();
        assert!(a.roster().is_tombstoned("a.md"));

        // A 离线：推送失败，批次仍留在 outbox。
        a_transport.set_connected(false);
        assert!(matches!(
            a.push(&mut a_transport),
            Err(SyncSessionError::Transport(
                TransportError::Disconnected { .. }
            ))
        ));
        assert_eq!(a.pending_batches().len(), 1);

        // 重连后重放成功，服务端记录墓碑。
        a_transport.set_connected(true);
        let pushed = a.push(&mut a_transport).unwrap();
        assert_eq!(pushed.confirmed, vec![delete_batch.clone()]);
        assert!(server.borrow().is_tombstoned("a.md"));
        assert!(server.borrow().text_of("a.md").is_err());

        // B 拉取后看到墓碑，并尝试用更低 op_id 推送旧内容：不得复活。
        let mut b = SyncSession::new(ws, dev_b, CrdtRoster::new());
        b.handshake(&mut b_transport).unwrap();
        b.pull(&mut b_transport).unwrap();
        assert!(b.roster().is_tombstoned("a.md"));
        let mut stale = DocFrameMap::new();
        stale.insert(
            "a.md".to_owned(),
            mazha_crdt::CrdtDoc::from_text("离线旧内容").encode_update(),
        );
        b.stage_local_change(
            &stale,
            &[MetaChange::Upsert {
                doc_id: "a.md".to_owned(),
                // 时钟 0 < 墓碑的时钟 1，按 D95 墓碑 wins。
                op_id: OpId::new(0, "dev-b").unwrap(),
                frame: stale["a.md"].clone(),
            }],
        )
        .unwrap();
        assert!(!b.roster().contains("a.md"));
        b.push(&mut b_transport).unwrap();
        assert!(server.borrow().is_tombstoned("a.md"));
        assert!(server.borrow().text_of("a.md").is_err());
        // 冲突副本被保留，未静默丢数据。
        let final_meta = server.borrow().meta();
        let frames = &final_meta.tombstones()["a.md"].conflict_frames();
        assert!(frames.len() >= 2);
    }

    #[test]
    fn bad_frame_or_metadata_in_pull_leaves_roster_and_cursor_unchanged() {
        for (bad_frame, bad_meta) in [(true, false), (false, true)] {
            let server = Rc::new(RefCell::new(InMemorySyncServer::new()));
            server.borrow_mut().seed_text("a.md", "原样").unwrap();
            let (ws, dev) = ids("设备甲");
            let inner = FakeTransport::new(&server, ws, dev);
            let mut transport = InjectTransport {
                inner,
                bad_frame,
                bad_meta,
            };
            let mut client = session("设备甲");
            client.handshake(&mut transport).unwrap();
            let before_sv = client.roster().state_vectors();
            let before_meta = client.roster().meta_snapshot();
            let error = client.pull(&mut transport).unwrap_err();
            assert!(matches!(error, SyncSessionError::Crdt(_)));
            assert_eq!(client.last_ack_cursor(), SyncCursor::new(0));
            assert_eq!(client.roster().state_vectors(), before_sv);
            assert_eq!(client.roster().meta_snapshot(), before_meta);
            assert!(client.roster().get("a.md").is_err());
        }
    }

    #[test]
    fn cursor_too_old_falls_back_to_snapshot_but_pending_outbox_blocks_it() {
        let server = Rc::new(RefCell::new(InMemorySyncServer::new()));
        server.borrow_mut().seed_text("a.md", "第一版").unwrap();
        let (ws, dev) = ids("设备甲");
        let mut transport = FakeTransport::new(&server, ws.clone(), dev.clone());
        let mut client = session("设备甲");
        client.handshake(&mut transport).unwrap();
        client.pull(&mut transport).unwrap();
        assert_eq!(client.roster().get("a.md").unwrap().text(), "第一版");

        // 服务端继续演进并裁剪历史：旧游标拉取触发快照回退。
        server.borrow_mut().seed_text("b.md", "第二版").unwrap();
        server.borrow_mut().prune_history(SyncCursor::new(1));
        let outcome = client.pull(&mut transport).unwrap();
        assert!(outcome.via_snapshot);
        assert_eq!(outcome.cursor, SyncCursor::new(2));
        assert_eq!(client.roster().get("b.md").unwrap().text(), "第二版");

        // 本地有未确认批次时，快照不得自动覆盖。
        transport.set_connected(false);
        client.stage_text("a.md", "本地未同步").unwrap();
        transport.set_connected(true);
        server.borrow_mut().seed_text("c.md", "第三版").unwrap();
        server.borrow_mut().prune_history(SyncCursor::new(2));
        let error = client.pull(&mut transport).unwrap_err();
        assert!(matches!(
            error,
            SyncSessionError::SnapshotNeedsConfirmation { pending_batches: 1 }
        ));
        assert!(error.needs_confirmation());
        assert_eq!(client.last_ack_cursor(), SyncCursor::new(2));
        assert_eq!(client.pending_batches().len(), 1);
        assert_eq!(client.roster().get("a.md").unwrap().text(), "本地未同步");
    }

    #[test]
    fn deterministic_op_ids_batches_and_cjk_ids() {
        let mut client = session("设备甲");
        assert_eq!(client.next_op_id().unwrap().clock(), 1);
        assert_eq!(client.next_op_id().unwrap().clock(), 2);
        assert_eq!(client.next_op_id().unwrap().device(), "设备甲");
        let first = client.stage_text("笔记/甲.md", "你好").unwrap();
        let second = client.stage_text("笔记/乙.md", "世界").unwrap();
        assert_eq!(first.as_str(), "设备甲-00000000000000000000");
        assert_eq!(second.as_str(), "设备甲-00000000000000000001");
        // batch_counter 与 op_clock 相互独立但都确定性推进。
        assert_eq!(client.batch_counter, 2);
        assert_eq!(client.op_clock(), 5);
        assert_eq!(client.roster().ids(), vec!["笔记/乙.md", "笔记/甲.md"]);
    }

    #[test]
    fn two_sessions_converge_with_one_shared_server() {
        let server = Rc::new(RefCell::new(InMemorySyncServer::new()));
        let (ws, dev_a) = ids("dev-a");
        let b = DeviceId::new("dev-b").unwrap();
        let (mut a_transport, mut b_transport) =
            FakeTransport::pair(&server, ws.clone(), dev_a.clone(), b.clone());

        // A 先建内容并推送；B 首轮拉取应得到完全一致的状态。
        let mut a = SyncSession::new(ws.clone(), dev_a, CrdtRoster::new());
        a.roster_mut()
            .create_from_text("shared.md", "初稿")
            .unwrap();
        a.stage_text("shared.md", "初稿").unwrap();
        a.sync_once(&mut a_transport).unwrap();

        let mut b = SyncSession::new(ws.clone(), b, CrdtRoster::new());
        b.sync_once(&mut b_transport).unwrap();
        assert_eq!(
            b.roster().get("shared.md").unwrap().text(),
            a.roster().get("shared.md").unwrap().text()
        );

        // B 补充内容并推送，A 再拉：文本收敛。
        b.stage_text("shared.md", "初稿\n乙补充").unwrap();
        b.push(&mut b_transport).unwrap();
        a.pull(&mut a_transport).unwrap();
        assert_eq!(a.roster().get("shared.md").unwrap().text(), "初稿\n乙补充");
        assert_eq!(
            b.roster().get("shared.md").unwrap().text(),
            a.roster().get("shared.md").unwrap().text()
        );

        // A 删除并推送，B 拉取后删除不复活。
        a.stage_delete("shared.md").unwrap();
        a.push(&mut a_transport).unwrap();
        b.pull(&mut b_transport).unwrap();
        assert!(a.roster().is_tombstoned("shared.md"));
        assert!(b.roster().is_tombstoned("shared.md"));
        assert!(!b.roster().contains("shared.md"));
    }

    #[test]
    fn pull_before_handshake_and_flag_driven_snapshot() {
        let server = Rc::new(RefCell::new(InMemorySyncServer::new()));
        server.borrow_mut().seed_text("a.md", "快照内容").unwrap();
        let (ws, dev) = ids("设备甲");
        let mut plain = FakeTransport::new(&server, ws.clone(), dev.clone());
        assert!(matches!(
            session("设备甲").pull(&mut plain),
            Err(SyncSessionError::HandshakeRequired)
        ));

        let inner = FakeTransport::new(&server, ws, dev);
        let mut flagged = FlagTransport {
            inner,
            requires_snapshot: true,
        };
        let mut client = session("设备甲");
        let outcome = client.sync_once(&mut flagged).unwrap();
        assert!(outcome.handshake.unwrap().requires_snapshot);
        assert!(outcome.snapshot.is_some());
        // 快照路径已包含全量状态，不再重复拉增量。
        assert!(outcome.pull.is_none());
        assert_eq!(client.roster().get("a.md").unwrap().text(), "快照内容");
        assert_eq!(client.last_ack_cursor(), SyncCursor::new(1));
    }
}
