//! 内存服务端与假 transport（D99，契约见 `docs/SYNC-SPEC.md` §5.7/§6.4）。
//!
//! `InMemorySyncServer` 持有一个工作区的当前状态、单调游标、按 `(workspace, device, batch)`
//! 去重的表与按游标编号的变更日志；`FakeTransport` 通过 `Rc<RefCell<...>>` 共享它，
//! 因此不用网络、线程或 async 就能跑“两端 + 一服务端”的协议测试。
//! 服务端 push 不盲中继：直接复用 `CrdtRoster::apply_updates_with_meta` 的两阶段提交
//! 校验帧与元数据，失败时游标/日志/去重表全部不变。

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use mazha_crdt::{CrdtDoc, CrdtError, CrdtRoster, MetaChange, RosterMeta, Tombstone};

use crate::protocol::{
    BatchId, DEFAULT_MAX_FRAME_BYTES, DeviceId, DocFrameMap, HelloAck, HelloPayload, HelloRequest,
    ProtocolError, PullRequest, PullResponse, PushAck, PushRequest, SnapshotPayload,
    SyncCapability, SyncCursor, SyncEnvelope, SyncErrorCode, WorkspaceId, error_code_for,
};
use crate::transport::{SyncTransport, TransportError};

/// 变更日志的一项：游标 + 当时的内容帧 + 元数据变更。
#[derive(Debug, Clone)]
struct LogEntry {
    /// 提交后的服务端游标。
    cursor: SyncCursor,
    /// 该批次携带的内容帧（真实增量协议可据此回放到中间状态）。
    frames: DocFrameMap,
    /// 该批次携带的元数据变更。
    meta_changes: Vec<MetaChange>,
}

/// 批次去重键：工作区 + 设备 + 批次号（D99）。
type BatchKey = (WorkspaceId, DeviceId, BatchId);

/// 单线程 in-memory 服务端：一个工作区的持久化状态与游标语义。
#[derive(Debug)]
pub struct InMemorySyncServer {
    /// 当前活动文档与墓碑（服务端状态真相）。
    roster: CrdtRoster,
    /// 单调递增的服务端游标。
    cursor: SyncCursor,
    /// 按提交顺序排列的变更日志。
    log: Vec<LogEntry>,
    /// 日志裁剪下界：`since < log_floor` 的拉取一律要求快照。
    log_floor: SyncCursor,
    /// 已应用批次 → 首次 ack 的游标（幂等去重）。
    applied_batches: HashMap<BatchKey, SyncCursor>,
    /// 会话计数器，用于生成确定性的 `session_id`。
    session_counter: u64,
    /// 单帧大小上限（握手时告知客户端）。
    max_frame_bytes: u64,
}

impl Default for InMemorySyncServer {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemorySyncServer {
    /// 新建空服务端：游标为 0、日志为空、允许快照引导。
    #[must_use]
    pub fn new() -> Self {
        Self {
            roster: CrdtRoster::new(),
            cursor: SyncCursor::new(0),
            log: Vec::new(),
            log_floor: SyncCursor::new(0),
            applied_batches: HashMap::new(),
            session_counter: 0,
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
        }
    }

    /// 覆盖单帧大小上限（用于测试拒绝超大帧）。
    #[must_use]
    pub const fn with_max_frame_bytes(mut self, max_frame_bytes: u64) -> Self {
        self.max_frame_bytes = max_frame_bytes;
        self
    }

    /// 当前服务端游标。
    #[must_use]
    pub const fn cursor(&self) -> SyncCursor {
        self.cursor
    }

    /// 变更日志长度（测试用于断言重复批次不二次记日志）。
    #[must_use]
    pub fn log_len(&self) -> usize {
        self.log.len()
    }

    /// 已去重批次数量。
    #[must_use]
    pub fn batch_count(&self) -> usize {
        self.applied_batches.len()
    }

    /// 单帧大小上限。
    #[must_use]
    pub const fn max_frame_bytes(&self) -> u64 {
        self.max_frame_bytes
    }

    /// 当前全部存活文档的 Yjs v1 全量帧（键升序）。
    ///
    /// # Errors
    /// 下层编码失败时返回 `CrdtError`（实际只对合法内部状态发生）。
    pub fn frames(&self) -> Result<DocFrameMap, CrdtError> {
        self.roster.encode_diff(&BTreeMap::new())
    }

    /// 当前墓碑快照（克隆，调用方修改不影响服务端）。
    #[must_use]
    pub fn meta(&self) -> RosterMeta {
        self.roster.meta_snapshot()
    }

    /// 读取某文档的当前文本（测试断言用）。
    ///
    /// # Errors
    /// ID 非法或文档不存在时返回 `CrdtError`。
    pub fn text_of(&self, id: &str) -> Result<String, CrdtError> {
        self.roster.get(id).map(CrdtDoc::text)
    }

    /// 文档是否已墓碑化（测试断言删除不复活）。
    #[must_use]
    pub fn is_tombstoned(&self, id: &str) -> bool {
        self.roster.is_tombstoned(id)
    }

    /// 取某个游标之后日志中记录过的内容帧（诊断/未来精确增量用）。
    ///
    /// 注意：增量帧不能按键覆盖合并，本方法只用于观测日志，下游应用仍走全量帧。
    #[must_use]
    pub fn logged_frames_after(&self, cursor: SyncCursor) -> DocFrameMap {
        let mut out = DocFrameMap::new();
        for entry in &self.log {
            if entry.cursor > cursor {
                out.extend(
                    entry
                        .frames
                        .iter()
                        .map(|(id, frame)| (id.clone(), frame.clone())),
                );
            }
        }
        out
    }

    /// 查询某批次是否已应用过。
    #[must_use]
    pub fn is_batch_applied(
        &self,
        workspace: &WorkspaceId,
        device: &DeviceId,
        batch: &BatchId,
    ) -> bool {
        self.applied_batches
            .contains_key(&(workspace.clone(), device.clone(), batch.clone()))
    }

    /// 把 `through` 及更早的日志标记为不可用，触发 `CursorTooOld` 回退快照。
    pub fn prune_history(&mut self, through: SyncCursor) {
        // 下界取 through 的下一游标：任何 since <= through 的客户端都已落后于可用历史。
        let floor = through.next();
        if floor > self.log_floor {
            self.log_floor = floor;
        }
        self.log.retain(|entry| entry.cursor > through);
    }

    /// 测试/初始化用：直接种入一篇文本文档并推进游标。
    ///
    /// # Errors
    /// ID 非法或下层应用失败时返回协议错误；原子失败时服务端状态不变。
    pub fn seed_text(&mut self, id: &str, text: &str) -> Result<SyncCursor, ProtocolError> {
        let mut frames = DocFrameMap::new();
        frames.insert(id.to_owned(), CrdtDoc::from_text(text).encode_update());
        self.commit(&frames, &[]).map_err(ProtocolError::from)
    }

    /// 测试/初始化用：直接种入墓碑并推进游标。
    ///
    /// # Errors
    /// 墓碑非法或下层应用失败时返回协议错误；原子失败时服务端状态不变。
    pub fn seed_tombstone(&mut self, tombstone: Tombstone) -> Result<SyncCursor, ProtocolError> {
        self.commit(&BTreeMap::new(), &[MetaChange::Tombstone(tombstone)])
            .map_err(ProtocolError::from)
    }

    /// 内部提交：在 roster 上原子应用，再推进游标并记日志。
    fn commit(
        &mut self,
        frames: &DocFrameMap,
        changes: &[MetaChange],
    ) -> Result<SyncCursor, CrdtError> {
        // apply_updates_with_meta 内部两阶段提交：校验失败时活动文档与墓碑都原样不动，
        // 所以游标/日志/去重表也必须在它成功之后才更新，才能保持“失败零副作用”。
        self.roster.apply_updates_with_meta(frames, changes)?;
        self.cursor = self.cursor.next();
        self.log.push(LogEntry {
            cursor: self.cursor,
            frames: frames.clone(),
            meta_changes: changes.to_vec(),
        });
        Ok(self.cursor)
    }

    /// 服务端握手逻辑：登记设备并返回会话 ID 与当前游标。
    ///
    /// # Errors
    /// 信封版本错误或能力集缺少 `ContentV1` 时返回 `TransportError`。
    pub fn hello(&mut self, req: &HelloRequest) -> Result<HelloAck, TransportError> {
        req.validate()?;
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
        // 会话号只依赖服务端自身的提交/握手次数：无系统时间、无随机数，单测可复现。
        self.session_counter += 1;
        Ok(HelloAck {
            session_id: format!("sess-{}", self.session_counter),
            cursor: self.cursor,
            requires_snapshot: false,
            max_frame_bytes: self.max_frame_bytes,
        })
    }

    /// 服务端拉取逻辑：返回全量最新帧 + `since` 之后的元数据变更与新游标。
    ///
    /// # Errors
    /// 游标过旧/超前、信封非法或下层编码失败时返回 `TransportError`。
    pub fn pull(&mut self, req: &PullRequest) -> Result<PullResponse, TransportError> {
        req.validate()?;
        if req.payload.session_id.is_empty() {
            return Err(TransportError::server(
                SyncErrorCode::InvalidRequest,
                "缺少会话 ID",
            ));
        }
        let since = req.payload.cursor;
        if since < self.log_floor {
            return Err(TransportError::server(
                SyncErrorCode::CursorTooOld,
                format!("{since} < {}", self.log_floor),
            ));
        }
        if since > self.cursor {
            return Err(TransportError::server(
                SyncErrorCode::SnapshotRequired,
                format!("客户端游标 {since} 超前于服务端 {}", self.cursor),
            ));
        }
        let mut meta_changes = Vec::new();
        for entry in &self.log {
            if entry.cursor > since {
                // 同一文档的多次元数据变更按日志顺序回放；CrdtRoster 会按 op_id
                // 排序应用，因此顺序不影响收敛结果。
                meta_changes.extend(entry.meta_changes.iter().cloned());
            }
        }
        // MVP 简化：内容帧总是返回当前全量（幂等），真实增量顺序由日志与游标保留，
        // 因此仍可触发 CursorTooOld 并正确推进游标（S3 可改为按日志精确裁剪）。
        let frames = self.frames().map_err(|error| server_from_crdt(&error))?;
        Ok(PullResponse {
            cursor: self.cursor,
            frames,
            meta_changes,
        })
    }

    /// 服务端推送逻辑：去重、校验、提交并推进游标。
    ///
    /// # Errors
    /// 重复批次外的任何校验失败都返回对应协议错误码，且服务端零改动。
    pub fn push(&mut self, req: &PushRequest) -> Result<PushAck, TransportError> {
        req.validate()?;
        let payload = &req.payload;
        let key = (
            req.workspace_id.clone(),
            req.device_id.clone(),
            payload.batch_id.clone(),
        );
        if let Some(first_cursor) = self.applied_batches.get(&key) {
            // 重复投递：直接返回首次 ack，绝不再次写日志或推进游标。
            return Ok(PushAck {
                cursor: *first_cursor,
                duplicate: true,
            });
        }
        for frame in payload.frames.values() {
            let size = u64::try_from(frame.len()).unwrap_or(u64::MAX);
            if size > self.max_frame_bytes {
                return Err(TransportError::server(
                    SyncErrorCode::BadFrame,
                    format!("帧大小 {size} 超过 {}", self.max_frame_bytes),
                ));
            }
        }
        let cursor = self
            .commit(&payload.frames, &payload.meta_changes)
            .map_err(|error| server_from_crdt(&error))?;
        self.applied_batches.insert(key, cursor);
        Ok(PushAck {
            cursor,
            duplicate: false,
        })
    }

    /// 服务端快照逻辑：全量内容帧 + 墓碑快照 + 当前游标。
    ///
    /// # Errors
    /// 下层编码失败时返回 `BadFrame`。
    pub fn snapshot(&self) -> Result<SnapshotPayload, TransportError> {
        Ok(SnapshotPayload {
            frames: self.frames().map_err(|error| server_from_crdt(&error))?,
            meta: self.meta(),
            cursor: self.cursor,
        })
    }
}

/// 把 CRDT 错误转为传输层的服务端协议错误（错误码 + 诊断信息）。
fn server_from_crdt(error: &CrdtError) -> TransportError {
    TransportError::server(error_code_for(error), error.to_string())
}

/// 假 transport：绑定一个工作区/设备，可模拟断线；多个实例共享同一服务端。
#[derive(Debug)]
pub struct FakeTransport {
    /// 共享的单线程服务端。
    server: Rc<RefCell<InMemorySyncServer>>,
    /// 本设备所属工作区。
    workspace_id: WorkspaceId,
    /// 本设备标识。
    device_id: DeviceId,
    /// 连接状态（false 时所有方法返回 `Disconnected`）。
    connected: bool,
}

impl FakeTransport {
    /// 构造一个连接到共享服务端的假 transport。
    #[must_use]
    pub fn new(
        server: &Rc<RefCell<InMemorySyncServer>>,
        workspace_id: WorkspaceId,
        device_id: DeviceId,
    ) -> Self {
        Self {
            server: Rc::clone(server),
            workspace_id,
            device_id,
            connected: true,
        }
    }

    /// 一次构造“两端 + 一服务端”：两个设备共享同一 `Rc<RefCell<...>>`。
    #[must_use]
    pub fn pair(
        server: &Rc<RefCell<InMemorySyncServer>>,
        workspace_id: WorkspaceId,
        device_a: DeviceId,
        device_b: DeviceId,
    ) -> (Self, Self) {
        (
            Self::new(server, workspace_id.clone(), device_a),
            Self::new(server, workspace_id, device_b),
        )
    }

    /// 模拟断线/重连；断线时 outbox 由会话层保留。
    pub fn set_connected(&mut self, connected: bool) {
        self.connected = connected;
    }

    /// 当前连接状态。
    #[must_use]
    pub const fn is_connected(&self) -> bool {
        self.connected
    }

    /// 断线守卫：所有请求在断线时立即失败，不触碰服务端状态。
    fn guard(&self) -> Result<(), TransportError> {
        if self.connected {
            Ok(())
        } else {
            Err(TransportError::disconnected("假 transport 已断线"))
        }
    }

    /// 校验请求信封的工作区/设备是否与本 transport 绑定一致。
    fn check_identity<P>(&self, req: &SyncEnvelope<P>) -> Result<(), TransportError> {
        if req.workspace_id != self.workspace_id || req.device_id != self.device_id {
            return Err(TransportError::invalid_response("请求身份与通道绑定不一致"));
        }
        Ok(())
    }
}

impl SyncTransport for FakeTransport {
    fn hello(&mut self, req: HelloRequest) -> Result<HelloAck, TransportError> {
        self.guard()?;
        // 握手身份来自通道绑定；请求仍被校验版本，避免错版本信封被静默接受。
        req.validate()?;
        let req = SyncEnvelope::new(
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
        self.server.borrow_mut().hello(&req)
    }

    fn pull(&mut self, req: PullRequest) -> Result<PullResponse, TransportError> {
        self.guard()?;
        self.check_identity(&req)?;
        self.server.borrow_mut().pull(&req)
    }

    fn push(&mut self, req: PushRequest) -> Result<PushAck, TransportError> {
        self.guard()?;
        self.check_identity(&req)?;
        self.server.borrow_mut().push(&req)
    }

    fn snapshot(&mut self) -> Result<SnapshotPayload, TransportError> {
        self.guard()?;
        self.server.borrow().snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{PullPayload, PushPayload};
    use mazha_crdt::OpId;

    /// 测试用工作区与设备 ID。
    fn ids() -> (WorkspaceId, DeviceId) {
        (
            WorkspaceId::new("笔记工作区").unwrap(),
            DeviceId::new("设备甲").unwrap(),
        )
    }

    #[test]
    fn hello_reports_cursor_and_pull_returns_frames_and_tombstones() {
        let server = Rc::new(RefCell::new(InMemorySyncServer::new()));
        server.borrow_mut().seed_text("笔记/a.md", "你好").unwrap();
        let tomb = Tombstone::new("旧.md", OpId::new(4, "设备乙").unwrap()).unwrap();
        server.borrow_mut().seed_tombstone(tomb).unwrap();
        let (ws, device) = ids();
        let mut transport = FakeTransport::new(&server, ws.clone(), device.clone());

        let ack = transport
            .hello(SyncEnvelope::new(
                ws.clone(),
                device.clone(),
                HelloPayload {
                    capabilities: vec![SyncCapability::ContentV1],
                },
            ))
            .unwrap();
        assert_eq!(ack.session_id, "sess-1");
        assert_eq!(ack.cursor, SyncCursor::new(2));
        assert!(!ack.requires_snapshot);
        assert_eq!(ack.max_frame_bytes, DEFAULT_MAX_FRAME_BYTES);

        let response = transport
            .pull(SyncEnvelope::new(
                ws,
                device,
                PullPayload {
                    session_id: ack.session_id,
                    cursor: SyncCursor::new(0),
                    state_vectors: BTreeMap::new(),
                    known_metadata: RosterMeta::new(),
                },
            ))
            .unwrap();
        assert_eq!(response.cursor, SyncCursor::new(2));
        assert_eq!(response.frames.len(), 1);
        assert_eq!(response.meta_changes.len(), 1);
        assert_eq!(response.meta_changes[0].doc_id(), "旧.md");
    }

    #[test]
    fn duplicate_batch_returns_first_ack_without_new_log_entry() {
        let server = Rc::new(RefCell::new(InMemorySyncServer::new()));
        let (ws, device) = ids();
        let mut transport = FakeTransport::new(&server, ws.clone(), device.clone());
        let batch = BatchId::new(&device, 3).unwrap();
        let mut frames = DocFrameMap::new();
        frames.insert(
            "a.md".to_owned(),
            CrdtDoc::from_text("内容").encode_update(),
        );
        let request = SyncEnvelope::new(
            ws.clone(),
            device.clone(),
            PushPayload {
                batch_id: batch.clone(),
                frames,
                meta_changes: Vec::new(),
            },
        );

        let first = transport.push(request.clone()).unwrap();
        let second = transport.push(request).unwrap();
        assert!(!first.duplicate);
        assert!(second.duplicate);
        assert_eq!(first.cursor, second.cursor);
        assert_eq!(server.borrow().cursor(), SyncCursor::new(1));
        assert_eq!(server.borrow().log_len(), 1);
        assert_eq!(
            server
                .borrow()
                .logged_frames_after(SyncCursor::new(0))
                .len(),
            1
        );
        assert_eq!(server.borrow().batch_count(), 1);
        assert!(server.borrow().is_batch_applied(&ws, &device, &batch));
        assert_eq!(server.borrow().text_of("a.md").unwrap(), "内容");
    }

    #[test]
    fn bad_frame_push_is_rejected_without_cursor_or_log_change() {
        let server = Rc::new(RefCell::new(InMemorySyncServer::new()));
        server.borrow_mut().seed_text("keep.md", "原样").unwrap();
        let (ws, device) = ids();
        let mut transport = FakeTransport::new(&server, ws.clone(), device.clone());
        let mut frames = DocFrameMap::new();
        frames.insert("keep.md".to_owned(), vec![0xff, 0xff, 0xff]);
        let request = SyncEnvelope::new(
            ws,
            device,
            PushPayload {
                batch_id: BatchId::new(&DeviceId::new("设备甲").unwrap(), 9).unwrap(),
                frames,
                meta_changes: Vec::new(),
            },
        );
        let error = transport.push(request).unwrap_err();
        assert_eq!(error.code(), Some(SyncErrorCode::BadFrame));
        assert_eq!(server.borrow().cursor(), SyncCursor::new(1));
        assert_eq!(server.borrow().log_len(), 1);
        assert_eq!(server.borrow().batch_count(), 0);
        assert_eq!(server.borrow().text_of("keep.md").unwrap(), "原样");
    }

    #[test]
    fn paired_transports_share_server_and_disconnect_blocks_requests() {
        let server = Rc::new(RefCell::new(InMemorySyncServer::new()));
        let (ws, a) = ids();
        let b = DeviceId::new("设备乙").unwrap();
        let (mut left, mut right) = FakeTransport::pair(&server, ws.clone(), a.clone(), b.clone());
        assert!(left.is_connected() && right.is_connected());

        left.set_connected(false);
        let hello = SyncEnvelope::new(
            ws.clone(),
            a.clone(),
            HelloPayload {
                capabilities: vec![SyncCapability::ContentV1],
            },
        );
        assert!(matches!(
            left.hello(hello.clone()),
            Err(TransportError::Disconnected { .. })
        ));
        // 另一端仍可握手，证明共享服务端不因单端断线而失效。
        let hello_b = SyncEnvelope::new(
            ws.clone(),
            b,
            HelloPayload {
                capabilities: vec![SyncCapability::ContentV1],
            },
        );
        assert!(right.hello(hello_b).is_ok());
        assert_eq!(server.borrow().cursor(), SyncCursor::new(0));
    }
}
