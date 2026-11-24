//! 本地同步引擎（D101，契约见 `docs/SYNC-SPEC.md` §5.9）。
//!
//! 把 `SyncSession` 接到工作区文件：watch 事件只标脏，`sync_once` 统一读盘暂存、
//! 网络同步、写回远端变更并持久化状态。同步阻塞、无后台线程/async，传输由调用方
//! 以 `&mut dyn SyncTransport` 注入（FakeTransport / HttpTransport 均可）。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use mazha_crdt::{CrdtDoc, CrdtRoster, DocId, MetaChange};
use mazha_store::{COALESCE_WINDOW, DocPath, EntryKind, StorageProvider, StoreError, WatchEvent};
use mazha_sync::{
    DeviceId, SessionCheckpoint, SyncCursor, SyncOnceOutcome, SyncSession, SyncTransport,
    WorkspaceId,
};

use crate::error::LocalSyncError;
use crate::mapping::{
    LocalWriteAction, ObservedBytes, is_syncable_md, local_text_to_frames, merge_watch_events,
    remote_frames_to_local_writes, should_ignore_echo,
};
use crate::resolve::{
    ConflictChoice, ResolveOutcome, SnapshotChoice, conflict_copy_text, merged_conflict_text,
};
use crate::state::{LocalSyncState, LocalSyncStore};

/// 引擎对外状态快照（供 S5 UI 展示，本 crate 不碰 UI）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSyncStatus {
    /// 已标脏、等待 `sync_once` 检查的文档 ID（升序）。
    pub dirty_docs: Vec<DocId>,
    /// 快照回退因本地未确认批次被拒绝，需用户确认。
    pub needs_confirmation: bool,
    /// 墓碑中保留了被取代的冲突副本，数据未静默丢失。
    pub conflict_docs: Vec<DocId>,
    /// 最后确认的服务端游标。
    pub last_cursor: SyncCursor,
}

impl Default for LocalSyncStatus {
    fn default() -> Self {
        Self {
            dirty_docs: Vec::new(),
            needs_confirmation: false,
            conflict_docs: Vec::new(),
            last_cursor: SyncCursor::new(0),
        }
    }
}

/// 一次 `sync_once` 的组合结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSyncOutcome {
    /// 本轮从本地文件暂存进会话的文档数（去抖后）。
    pub staged_local: usize,
    /// 本轮从远端写回本地文件的动作（按文档 ID 升序）。
    pub writes: Vec<LocalWriteAction>,
    /// 会话层原始结果（握手/拉取/推送）。
    pub sync: SyncOnceOutcome,
}

/// 本地同步引擎：监听标脏 → 暂存 → 同步 → 写回 → 持久化。
pub struct LocalSyncEngine {
    /// 工作区存储 Provider（文件读写与状态文件共用）。
    provider: Arc<dyn StorageProvider>,
    /// 状态文件访问器。
    store: LocalSyncStore,
    /// 同步会话（拥有名册）。
    session: SyncSession,
    /// 上游合并窗口（store 已按此窗口去抖，本字段仅暴露给 S5 诊断）。
    coalesce_window: Duration,
    /// 待检查的文档 ID 集合（只标脏，不读盘）。
    dirty: BTreeSet<DocId>,
    /// 自己写回文件后的字节标记：`Some` 为刚写入的字节，`None` 为刚删除。
    write_marks: BTreeMap<DocId, Option<Vec<u8>>>,
    /// 已显式确认过的冲突副本指纹：`DocId` → 冲突帧字节列表。
    /// 只有当前墓碑的冲突帧与之一致时才抑制横幅；同一会话内新冲突仍会提示。
    /// 仅内存有效（重启后指纹丢失，横幅可能重现），不写状态文件。
    resolved_conflict_frames: BTreeMap<DocId, Vec<Vec<u8>>>,
    /// 已显式确认过的快照门槛 outbox 指纹（批次 ID 升序集合）。
    /// 只有当前 outbox 与之一致时才抑制；新批次仍会提示。仅内存有效。
    resolved_snapshot_batches: Option<BTreeSet<String>>,
    /// 对外状态快照。
    status: LocalSyncStatus,
}

impl LocalSyncEngine {
    /// 打开工作区并建立引擎；状态文件不存在时扫描 Markdown 建初始名册（不自动 stage 删除）。
    ///
    /// # Errors
    /// 状态文件损坏、扫描/读取失败或检查点身份不匹配时返回错误；坏状态绝不回退空状态。
    pub fn open(
        provider: Arc<dyn StorageProvider>,
        workspace_id: WorkspaceId,
        device_id: DeviceId,
    ) -> Result<Self, LocalSyncError> {
        Self::open_with_coalesce(provider, workspace_id, device_id, COALESCE_WINDOW)
    }

    /// 与 [`Self::open`] 相同，但允许覆盖合并窗口（仅影响上游标脏阶段的诊断信息）。
    ///
    /// # Errors
    /// 同 [`Self::open`]。
    pub fn open_with_coalesce(
        provider: Arc<dyn StorageProvider>,
        workspace_id: WorkspaceId,
        device_id: DeviceId,
        coalesce_window: Duration,
    ) -> Result<Self, LocalSyncError> {
        let store = LocalSyncStore::new(Arc::clone(&provider));
        let loaded = store.load()?;
        let roster = if let Some(state) = &loaded {
            restore_roster(&state.frames, &state.meta)?
        } else {
            let mut texts = BTreeMap::new();
            scan_markdown(provider.as_ref(), &DocPath::root(), &mut texts)?;
            let mut fresh = CrdtRoster::new();
            for (doc_id, text) in texts {
                fresh.create_from_text(&doc_id, &text)?;
            }
            fresh
        };
        // 身份类型在此被会话消费；检查点身份从会话自身读取，避免额外克隆。
        let mut session = SyncSession::new(workspace_id, device_id, roster);
        if let Some(state) = loaded {
            // 重启后强制重新握手；会话 ID 只保留诊断，不拿旧会话 ID 去拉增量。
            let checkpoint = SessionCheckpoint {
                workspace_id: session.workspace_id().as_str().to_owned(),
                device_id: session.device_id().as_str().to_owned(),
                last_ack_cursor: state.last_cursor,
                server_cursor: state.last_cursor,
                outbox: state.outbox,
                session_id: state.session_id,
                needs_handshake: true,
                requires_snapshot: false,
                op_clock: state.op_clock,
                batch_counter: state.batch_counter,
            };
            session.restore_checkpoint(&checkpoint)?;
        }
        let status = LocalSyncStatus {
            conflict_docs: visible_conflict_docs(&session, &BTreeMap::new()),
            ..LocalSyncStatus::default()
        };
        Ok(Self {
            provider,
            store,
            session,
            coalesce_window,
            dirty: BTreeSet::new(),
            write_marks: BTreeMap::new(),
            resolved_conflict_frames: BTreeMap::new(),
            resolved_snapshot_batches: None,
            status,
        })
    }

    /// 上游合并窗口（与 `mazha-store` 的 `COALESCE_WINDOW` 对齐）。
    #[must_use]
    pub const fn coalesce_window(&self) -> Duration {
        self.coalesce_window
    }

    /// 只读会话（测试/诊断用；改动名册请走 `drain_dirty` 以保持 outbox 一致）。
    #[must_use]
    pub const fn session(&self) -> &SyncSession {
        &self.session
    }

    /// 当前对外状态。
    #[must_use]
    pub const fn status(&self) -> &LocalSyncStatus {
        &self.status
    }

    /// 消费一批监听事件：过滤 + 合并 + 标脏，**不读文件**。
    ///
    /// 返回本次新增的脏文档数（同一路径多次事件只计一次）。
    pub fn handle_watch_events(&mut self, events: &[WatchEvent]) -> usize {
        let merged = merge_watch_events(events);
        let added = merged.difference(&self.dirty).count();
        self.dirty.extend(merged);
        self.status.dirty_docs = self.dirty.iter().cloned().collect();
        added
    }

    /// 读取脏文件并暂存进会话；返回实际产生的批次数。
    ///
    /// 两层回声抑制：先按 `write_marks` 做字节比对并**消费命中标记**；再把磁盘文本与
    /// 名册文本比对，相同则不 stage。文件消失且会话中仍有活动文档时走墓碑化删除。
    /// 非回声观察必须清除该文档的旧标记，否则用户后续改回旧字节会被误判为回声而漏推。
    /// 暂存成功后立即落盘：断线/崩溃发生在网络同步之前时，outbox 与逻辑时钟仍可恢复。
    ///
    /// # Errors
    /// 读盘失败、非法 UTF-8、帧生成、暂存或状态落盘失败时返回错误；已成功暂存的文件不会回滚。
    pub fn drain_dirty(&mut self) -> Result<usize, LocalSyncError> {
        let ids = std::mem::take(&mut self.dirty);
        self.status.dirty_docs.clear();
        let mut staged = 0;
        for doc_id in ids {
            let path = DocPath::new(doc_id.clone())?;
            match self.provider.read(&path) {
                Ok(bytes) => {
                    if should_ignore_echo(
                        &doc_id,
                        ObservedBytes::Present(&bytes),
                        &self.write_marks,
                    ) {
                        self.write_marks.remove(&doc_id);
                        continue;
                    }
                    // 非回声：旧标记已失效，必须清除，否则用户改回旧字节时会被误吞。
                    self.write_marks.remove(&doc_id);
                    let text = String::from_utf8(bytes)
                        .map_err(|_| LocalSyncError::InvalidText(doc_id.clone()))?;
                    // 第二层保护：远端刚写回、用户又改回同名同内容时不产生无意义批次。
                    if let Ok(doc) = self.session.roster().get(&doc_id)
                        && doc.text() == text
                    {
                        continue;
                    }
                    let base = self.session.roster().get(&doc_id).ok();
                    let frames = local_text_to_frames(&doc_id, base, &text)?;
                    let change = MetaChange::Upsert {
                        doc_id: doc_id.clone(),
                        op_id: self.session.next_op_id()?,
                        // 只走元数据 Upsert 的应用路径（与 stage_text 一致），
                        // 不再把同一帧塞进 updates 造成重复应用。
                        frame: frames[&doc_id].clone(),
                    };
                    self.session
                        .stage_local_change(&BTreeMap::new(), &[change])?;
                    staged += 1;
                }
                Err(StoreError::NotFound(_)) => {
                    if should_ignore_echo(&doc_id, ObservedBytes::Missing, &self.write_marks) {
                        self.write_marks.remove(&doc_id);
                        continue;
                    }
                    // 非回声删除同样让旧标记失效（例如远端刚写回就被外部删除）。
                    self.write_marks.remove(&doc_id);
                    // 文件消失但名册里没有活动文档（例如外部删除了已墓碑化的路径）→ 无需再删。
                    if self.session.roster().contains(&doc_id) {
                        self.session.stage_delete(&doc_id)?;
                        staged += 1;
                    }
                }
                Err(error) => return Err(error.into()),
            }
        }
        if staged > 0 {
            // 离线编辑先落盘：outbox/逻辑时钟只在会话内存中，断线时不写盘会在重启后丢失。
            self.persist()?;
        }
        Ok(staged)
    }

    /// 一轮完整同步：标脏暂存 → `sync_once` → 写回远端变更 → 持久化。
    ///
    /// 快照确认被拒绝或传输失败时**保留本地文件/outbox**，置位状态并落盘后把错误
    /// 原样返回；调用方（S5 UI）据此提示用户选择合并/覆盖，不静默丢数据。
    ///
    /// # Errors
    /// 暂存、传输、写回或持久化任一步失败即返回错误。
    pub fn sync_once(
        &mut self,
        transport: &mut dyn SyncTransport,
    ) -> Result<LocalSyncOutcome, LocalSyncError> {
        let staged_local = self.drain_dirty()?;
        let before = self.roster_texts();
        let before_tombstones: BTreeSet<DocId> =
            self.session.roster().tombstones().keys().cloned().collect();
        let sync = match self.session.sync_once(transport) {
            Ok(outcome) => outcome,
            Err(error) => {
                // 已显式确认过的同一批 outbox 不再重复弹门槛横幅；新批次仍提示。
                if error.needs_confirmation()
                    && outbox_fingerprint(&self.session) != self.resolved_snapshot_batches
                {
                    self.status.needs_confirmation = true;
                }
                self.status.conflict_docs =
                    visible_conflict_docs(&self.session, &self.resolved_conflict_frames);
                self.status.last_cursor = self.session.last_ack_cursor();
                // 传输失败时也要落盘：握手可能已写入会话 ID，更关键的是已暂定的
                // outbox/时钟必须跨进程存活，否则断线重启会丢离线编辑（G2）。
                self.persist()?;
                return Err(error.into());
            }
        };
        let after = self.roster_texts();
        let writes = remote_frames_to_local_writes(&before, &after, &before_tombstones);
        for action in &writes {
            match action {
                LocalWriteAction::Write { doc_id, bytes } => {
                    let path = DocPath::new(doc_id.clone())?;
                    self.provider.write(&path, bytes)?;
                    self.write_marks.insert(doc_id.clone(), Some(bytes.clone()));
                }
                LocalWriteAction::Remove { doc_id } => {
                    let path = DocPath::new(doc_id.clone())?;
                    match self.provider.remove(&path) {
                        Ok(()) | Err(StoreError::NotFound(_)) => {}
                        Err(error) => return Err(error.into()),
                    }
                    self.write_marks.insert(doc_id.clone(), None);
                }
            }
        }
        self.status.needs_confirmation = false;
        self.status.conflict_docs =
            visible_conflict_docs(&self.session, &self.resolved_conflict_frames);
        self.status.last_cursor = self.session.last_ack_cursor();
        self.persist()?;
        Ok(LocalSyncOutcome {
            staged_local,
            writes,
            sync,
        })
    }

    /// 显式解决一个墓碑冲突（S5b/D104），绝不自动触发网络同步。
    ///
    /// 先读取冲突副本帧并**全部解码校验**，任一坏帧直接返回错误且本地状态零改动；
    /// 选择 `KeepLocal`/`Merge` 时才 stage 一次严格更高 `op_id` 的 upsert 复活文档，
    /// `UseRemote` 只保留墓碑与副本作审计、不碰本地文件。桌面拿到 `document_text`
    /// 后走既有保存路径写盘，本方法不依赖 `mazha-core`、不打开任何 UI 文档。
    ///
    /// # Errors
    /// 文档未墓碑化、冲突帧损坏、暂存或落盘失败时返回错误；坏帧路径不改任何状态。
    pub fn resolve_conflict(
        &mut self,
        doc_id: &str,
        local_text: &str,
        choice: ConflictChoice,
    ) -> Result<ResolveOutcome, LocalSyncError> {
        // 只读取并校验材料，不在此处修改任何状态：坏帧必须整批零改动。
        let frames = self
            .session
            .roster()
            .tombstones()
            .get(doc_id)
            .ok_or_else(|| {
                LocalSyncError::CorruptState(format!("冲突文档 {doc_id} 不存在或已复活"))
            })?
            .conflict_frames()
            .to_vec();
        let conflict_text = conflict_copy_text(&frames)?;
        let document_text = match choice {
            ConflictChoice::KeepLocal => Some(conflict_text),
            ConflictChoice::UseRemote => None,
            ConflictChoice::Merge => Some(merged_conflict_text(&conflict_text, local_text)),
        };
        let mut outcome = ResolveOutcome {
            document_text,
            ..ResolveOutcome::default()
        };
        if let Some(text) = &outcome.document_text {
            outcome.revived_docs = vec![doc_id.to_owned()];
            self.stage_revival(doc_id, text)?;
        }
        // 记录已确认的指纹：UseRemote 保留墓碑，下一轮 sync_once 才不会重复弹横幅。
        // 放在所有可能失败的步骤之后，坏帧/暂存失败路径不改任何状态。
        self.resolved_conflict_frames
            .insert(doc_id.to_owned(), frames);
        self.status.needs_confirmation = false;
        self.status.conflict_docs =
            visible_conflict_docs(&self.session, &self.resolved_conflict_frames);
        self.status.last_cursor = self.session.last_ack_cursor();
        self.persist()?;
        outcome.confirmation_cleared = true;
        Ok(outcome)
    }

    /// 显式解决快照确认门槛（S5b/D104），不拉取快照也不自动重试同步。
    ///
    /// `KeepLocal` 保留 outbox 与本地文件；`UseRemote` 只经 checkpoint 显式清空
    /// outbox（用户已明确选择放弃未推送批次），下一轮 `sync_once` 才由服务端快照
    /// 覆盖本地；`Merge` 保留 outbox 让 CRDT 在下一轮自行合并本地与远端。
    ///
    /// # Errors
    /// 检查点身份不匹配或状态落盘失败时返回错误。
    pub fn resolve_snapshot_confirmation(
        &mut self,
        choice: SnapshotChoice,
    ) -> Result<ResolveOutcome, LocalSyncError> {
        // 先记下本轮门槛的 outbox 指纹，再可能清空；KeepLocal/Merge 保留的 outbox
        // 下一轮仍会触发同样的门槛，靠此指纹在同一会话内抑制重复横幅。
        self.resolved_snapshot_batches = outbox_fingerprint(&self.session);
        let mut outcome = ResolveOutcome::default();
        if choice == SnapshotChoice::UseRemote {
            // 不新增 mazha-sync API：复用 additive checkpoint/restore 显式丢弃批次。
            let mut checkpoint = self.session.checkpoint();
            checkpoint.outbox.clear();
            self.session.restore_checkpoint(&checkpoint)?;
            outcome.outbox_cleared = true;
        }
        self.status.needs_confirmation = false;
        self.status.conflict_docs =
            visible_conflict_docs(&self.session, &self.resolved_conflict_frames);
        self.status.last_cursor = self.session.last_ack_cursor();
        self.persist()?;
        outcome.confirmation_cleared = true;
        Ok(outcome)
    }

    /// stage 一次严格高于墓碑 `op_id` 的 upsert，使被删除文档复活（D98）。
    fn stage_revival(&mut self, doc_id: &str, text: &str) -> Result<(), LocalSyncError> {
        let tombstone_clock = self
            .session
            .roster()
            .tombstones()
            .get(doc_id)
            .map(|tombstone| tombstone.op_id().clock());
        let mut checkpoint = self.session.checkpoint();
        if let Some(clock) = tombstone_clock {
            checkpoint.op_clock = checkpoint.op_clock.max(clock);
        }
        // restore 只推进本地逻辑时钟；随后 next_op_id 得到严格更高的值。
        self.session.restore_checkpoint(&checkpoint)?;
        let op_id = self.session.next_op_id()?;
        // doc_id 来自已校验的墓碑，frame 由本地文本生成，下面的暂存不会失败。
        let frame = CrdtDoc::from_text(text).encode_update();
        let change = MetaChange::Upsert {
            doc_id: doc_id.to_owned(),
            op_id,
            frame,
        };
        self.session
            .stage_local_change(&BTreeMap::new(), &[change])?;
        Ok(())
    }

    /// 把当前会话与名册落盘。
    fn persist(&self) -> Result<(), LocalSyncError> {
        let checkpoint = self.session.checkpoint();
        let (frames, meta) = self
            .session
            .roster()
            .encode_diff_with_meta(&BTreeMap::new())?;
        self.store.save(&LocalSyncState {
            frames,
            meta,
            last_cursor: checkpoint.last_ack_cursor,
            session_id: checkpoint.session_id,
            op_clock: checkpoint.op_clock,
            batch_counter: checkpoint.batch_counter,
            outbox: checkpoint.outbox,
        })
    }

    /// 当前名册的 ID → 文本投影（只取活动文档）。
    fn roster_texts(&self) -> BTreeMap<DocId, String> {
        self.session
            .roster()
            .ids()
            .into_iter()
            .filter_map(|id| {
                self.session
                    .roster()
                    .get(&id)
                    .ok()
                    .map(|doc| (id, doc.text()))
            })
            .collect()
    }
}

/// 当前 outbox 的批次 ID 指纹（升序去重集合）；空 outbox 返回 `None`。
///
/// 用于识别「同一轮」快照确认门槛：用户确认后若 outbox 未变，下一轮不应重复提示；
/// 新增了本地批次则指纹变化，提示照常出现。批次 ID 来自既有 `PendingBatch::batch_id()`。
fn outbox_fingerprint(session: &SyncSession) -> Option<BTreeSet<String>> {
    let batches = session.checkpoint().outbox;
    if batches.is_empty() {
        return None;
    }
    Some(
        batches
            .iter()
            .map(|batch| batch.batch_id().as_str().to_owned())
            .collect(),
    )
}

/// 仍需在状态中展示的墓碑冲突文档 ID（升序）。
///
/// 墓碑的冲突帧字节列表与已确认指纹完全一致时视为「已解决」，本会话内不再弹横幅；
/// 帧内容变化（同一文档产生新冲突）则照常提示。指纹只存内存，重启后自然重现。
fn visible_conflict_docs(
    session: &SyncSession,
    resolved: &BTreeMap<DocId, Vec<Vec<u8>>>,
) -> Vec<DocId> {
    session
        .roster()
        .tombstones()
        .iter()
        .filter(|(id, tombstone)| {
            let frames = tombstone.conflict_frames();
            !frames.is_empty() && resolved.get(*id).map(Vec::as_slice) != Some(frames)
        })
        .map(|(id, _)| id.clone())
        .collect()
}

/// 递归扫描工作区的可同步 Markdown，隐藏目录整棵跳过。
fn scan_markdown(
    provider: &dyn StorageProvider,
    dir: &DocPath,
    out: &mut BTreeMap<DocId, String>,
) -> Result<(), LocalSyncError> {
    for entry in provider.list(dir)? {
        match entry.kind {
            EntryKind::Dir => {
                // 隐藏目录（.mazha/.git/.obsidian）整棵忽略，不做无意义的递归读盘。
                if entry
                    .path
                    .as_str()
                    .split('/')
                    .any(|seg| seg.starts_with('.'))
                {
                    continue;
                }
                scan_markdown(provider, &entry.path, out)?;
            }
            EntryKind::File => {
                if !is_syncable_md(&entry.path) {
                    continue;
                }
                let bytes = provider.read(&entry.path)?;
                let text = String::from_utf8(bytes)
                    .map_err(|_| LocalSyncError::InvalidText(entry.path.to_string()))?;
                out.insert(entry.path.as_str().to_owned(), text);
            }
        }
    }
    Ok(())
}

/// 用落盘的内容帧与元数据重建名册。
///
/// 先剔除已墓碑化的内容帧，避免 `apply_updates_with_meta` 把活动文档帧再次并进
/// 冲突副本；再单独应用墓碑快照（其中已保存的 conflict_frames 原样保留）。
fn restore_roster(
    frames: &BTreeMap<DocId, Vec<u8>>,
    meta: &mazha_crdt::RosterMeta,
) -> Result<CrdtRoster, LocalSyncError> {
    let mut content = frames.clone();
    for id in meta.tombstones().keys() {
        content.remove(id);
    }
    let mut roster = CrdtRoster::new();
    roster.apply_updates_with_meta(&content, &[])?;
    let changes: Vec<MetaChange> = meta
        .tombstones()
        .values()
        .cloned()
        .map(MetaChange::Tombstone)
        .collect();
    if !changes.is_empty() {
        roster.apply_meta(&changes)?;
    }
    Ok(roster)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mazha_crdt::{CrdtDoc, OpId, Tombstone};
    use mazha_store::FileSystemProvider;
    use mazha_sync::{FakeTransport, InMemorySyncServer, SyncSessionError};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// 测试用工作区/设备标识。
    fn ids(device: &str) -> (WorkspaceId, DeviceId) {
        (
            WorkspaceId::new("笔记工作区").unwrap(),
            DeviceId::new(device).unwrap(),
        )
    }

    /// 构造一个临时工作区 Provider。
    fn provider(path: &std::path::Path) -> Arc<dyn StorageProvider> {
        Arc::new(FileSystemProvider::open(path).unwrap())
    }

    /// 写一份 Markdown 文件。
    fn write_md(provider: &dyn StorageProvider, path: &str, text: &str) {
        provider
            .write(&DocPath::new(path).unwrap(), text.as_bytes())
            .unwrap();
    }

    /// 读一份 Markdown 文件的 UTF-8 文本。
    fn read_md(provider: &dyn StorageProvider, path: &str) -> String {
        let bytes = provider.read(&DocPath::new(path).unwrap()).unwrap();
        String::from_utf8(bytes).unwrap()
    }

    /// 造一个单设备 transport。
    fn transport(
        server: &Rc<RefCell<InMemorySyncServer>>,
        workspace: &WorkspaceId,
        device: &DeviceId,
    ) -> FakeTransport {
        FakeTransport::new(server, workspace.clone(), device.clone())
    }

    #[test]
    fn first_open_scans_markdown_and_ignores_hidden() {
        let dir = tempfile::TempDir::new().unwrap();
        let storage = provider(dir.path());
        write_md(storage.as_ref(), "a.md", "甲");
        write_md(storage.as_ref(), "sub/乙.markdown", "乙");
        write_md(storage.as_ref(), ".hidden.md", "隐藏");
        write_md(storage.as_ref(), ".mazha/内部.md", "内部");
        write_md(storage.as_ref(), "notes/readme.txt", "非文档");
        let (ws, dev) = ids("设备甲");
        let engine = LocalSyncEngine::open(Arc::clone(&storage), ws, dev).unwrap();
        assert_eq!(
            engine.session().roster().ids(),
            vec!["a.md".to_owned(), "sub/乙.markdown".to_owned()]
        );
        assert_eq!(engine.coalesce_window(), COALESCE_WINDOW);
    }

    #[test]
    fn local_edits_debounce_and_batch_counter_survives_reopen() {
        let dir = tempfile::TempDir::new().unwrap();
        let storage = provider(dir.path());
        write_md(storage.as_ref(), "a.md", "原文");
        let server = Rc::new(RefCell::new(InMemorySyncServer::new()));
        let (ws, dev) = ids("设备甲");
        let mut engine =
            LocalSyncEngine::open(Arc::clone(&storage), ws.clone(), dev.clone()).unwrap();
        let mut client = transport(&server, &ws, &dev);

        write_md(storage.as_ref(), "a.md", "第一版");
        let events: Vec<_> = (0..3)
            .map(|_| WatchEvent::Modified {
                path: DocPath::new("a.md").unwrap(),
            })
            .collect();
        assert_eq!(engine.handle_watch_events(&events), 1);
        assert_eq!(engine.status().dirty_docs, vec!["a.md".to_owned()]);
        assert_eq!(engine.sync_once(&mut client).unwrap().staged_local, 1);
        assert!(engine.status().dirty_docs.is_empty());
        assert_eq!(server.borrow().text_of("a.md").unwrap(), "第一版");

        write_md(storage.as_ref(), "a.md", "第二版");
        engine.handle_watch_events(&[WatchEvent::Modified {
            path: DocPath::new("a.md").unwrap(),
        }]);
        engine.sync_once(&mut client).unwrap();
        assert_eq!(server.borrow().text_of("a.md").unwrap(), "第二版");

        // 重启：状态文件恢复 op_clock/batch_counter，新编辑不能被服务端去重吞掉。
        let mut reopened =
            LocalSyncEngine::open(Arc::clone(&storage), ws.clone(), dev.clone()).unwrap();
        let mut reopened_client = transport(&server, &ws, &dev);
        assert_eq!(reopened.session().op_clock(), 2);
        write_md(storage.as_ref(), "a.md", "第三版");
        reopened.handle_watch_events(&[WatchEvent::Modified {
            path: DocPath::new("a.md").unwrap(),
        }]);
        reopened.sync_once(&mut reopened_client).unwrap();
        assert_eq!(server.borrow().text_of("a.md").unwrap(), "第三版");
        assert_eq!(server.borrow().batch_count(), 3);

        let state = LocalSyncStore::new(Arc::clone(&storage))
            .load()
            .unwrap()
            .unwrap();
        assert_eq!(state.batch_counter, 3);
        assert_eq!(state.op_clock, 3);
    }

    #[test]
    fn remote_change_written_back_with_cjk_crlf_and_echo_suppressed() {
        let dir_a = tempfile::TempDir::new().unwrap();
        let dir_b = tempfile::TempDir::new().unwrap();
        let storage_a = provider(dir_a.path());
        let storage_b = provider(dir_b.path());
        let server = Rc::new(RefCell::new(InMemorySyncServer::new()));
        let (ws, dev_a) = ids("设备甲");
        let dev_b = DeviceId::new("设备乙").unwrap();
        let (mut client_a, mut client_b) =
            FakeTransport::pair(&server, ws.clone(), dev_a.clone(), dev_b.clone());

        let text = "你好\r\n世界🌿";
        // 先开空工作区，再写文件，确保这是真实本地编辑而非初始扫描内容。
        let mut engine_a =
            LocalSyncEngine::open(Arc::clone(&storage_a), ws.clone(), dev_a).unwrap();
        write_md(storage_a.as_ref(), "a.md", text);
        engine_a.handle_watch_events(&[WatchEvent::Created {
            path: DocPath::new("a.md").unwrap(),
        }]);
        engine_a.sync_once(&mut client_a).unwrap();

        let mut engine_b = LocalSyncEngine::open(Arc::clone(&storage_b), ws, dev_b).unwrap();
        engine_b.sync_once(&mut client_b).unwrap();
        assert_eq!(read_md(storage_b.as_ref(), "a.md"), text);
        // CRLF 必须原样落盘，不能改成 LF。
        assert!(dir_b.path().join("a.md").is_file());
        assert_eq!(
            std::fs::read(dir_b.path().join("a.md")).unwrap(),
            text.as_bytes()
        );

        // 把刚才的写回事件当作外来事件重放：不应再产生任何出站批次。
        engine_b.handle_watch_events(&[WatchEvent::Modified {
            path: DocPath::new("a.md").unwrap(),
        }]);
        assert_eq!(engine_b.sync_once(&mut client_b).unwrap().staged_local, 0);
        assert!(server.borrow().batch_count() <= 1);
    }

    #[test]
    fn remote_tombstone_removes_file_and_exposes_conflict_copy() {
        let dir_a = tempfile::TempDir::new().unwrap();
        let dir_b = tempfile::TempDir::new().unwrap();
        let storage_a = provider(dir_a.path());
        let storage_b = provider(dir_b.path());
        let server = Rc::new(RefCell::new(InMemorySyncServer::new()));
        let (ws, dev_a) = ids("设备甲");
        let dev_b = DeviceId::new("设备乙").unwrap();
        let (mut client_a, mut client_b) =
            FakeTransport::pair(&server, ws.clone(), dev_a.clone(), dev_b.clone());

        let mut engine_a =
            LocalSyncEngine::open(Arc::clone(&storage_a), ws.clone(), dev_a).unwrap();
        write_md(storage_a.as_ref(), "a.md", "内容");
        engine_a.handle_watch_events(&[WatchEvent::Created {
            path: DocPath::new("a.md").unwrap(),
        }]);
        engine_a.sync_once(&mut client_a).unwrap();

        let mut engine_b =
            LocalSyncEngine::open(Arc::clone(&storage_b), ws.clone(), dev_b).unwrap();
        engine_b.sync_once(&mut client_b).unwrap();
        assert!(dir_b.path().join("a.md").is_file());

        // A 删除本地文件 → stage_delete → 服务端墓碑；B 拉取后删除本地文件。
        storage_a.remove(&DocPath::new("a.md").unwrap()).unwrap();
        engine_a.handle_watch_events(&[WatchEvent::Removed {
            path: DocPath::new("a.md").unwrap(),
        }]);
        engine_a.sync_once(&mut client_a).unwrap();
        assert!(server.borrow().is_tombstoned("a.md"));

        engine_b.sync_once(&mut client_b).unwrap();
        assert!(storage_b.read(&DocPath::new("a.md").unwrap()).is_err());
        assert!(engine_b.status().conflict_docs.contains(&"a.md".to_owned()));
        assert!(!engine_b.session().roster().contains("a.md"));
    }

    #[test]
    fn snapshot_conflict_preserves_local_file_and_marks_status() {
        let dir_a = tempfile::TempDir::new().unwrap();
        let dir_b = tempfile::TempDir::new().unwrap();
        let storage_a = provider(dir_a.path());
        let storage_b = provider(dir_b.path());
        let server = Rc::new(RefCell::new(InMemorySyncServer::new()));
        let (ws, dev_a) = ids("设备甲");
        let dev_b = DeviceId::new("设备乙").unwrap();
        let (mut client_a, mut client_b) =
            FakeTransport::pair(&server, ws.clone(), dev_a.clone(), dev_b.clone());

        let mut engine_a =
            LocalSyncEngine::open(Arc::clone(&storage_a), ws.clone(), dev_a).unwrap();
        write_md(storage_a.as_ref(), "a.md", "服务端版");
        engine_a.handle_watch_events(&[WatchEvent::Created {
            path: DocPath::new("a.md").unwrap(),
        }]);
        engine_a.sync_once(&mut client_a).unwrap();
        let synced_cursor = server.borrow().cursor();

        let mut engine_b =
            LocalSyncEngine::open(Arc::clone(&storage_b), ws.clone(), dev_b).unwrap();
        engine_b.sync_once(&mut client_b).unwrap();
        write_md(storage_b.as_ref(), "a.md", "本地未同步");
        engine_b.handle_watch_events(&[WatchEvent::Modified {
            path: DocPath::new("a.md").unwrap(),
        }]);
        // 只暂存、不推送：保留 outbox 以触发快照确认门槛。
        assert_eq!(engine_b.drain_dirty().unwrap(), 1);

        server.borrow_mut().seed_text("b.md", "新增").unwrap();
        server.borrow_mut().prune_history(synced_cursor);
        assert!(matches!(
            engine_b.sync_once(&mut client_b),
            Err(LocalSyncError::Session(
                SyncSessionError::SnapshotNeedsConfirmation { .. }
            ))
        ));
        assert!(engine_b.status().needs_confirmation);
        assert_eq!(read_md(storage_b.as_ref(), "a.md"), "本地未同步");
        assert_eq!(engine_b.session().pending_batches().len(), 1);
    }

    #[test]
    fn rename_marks_old_delete_and_new_create() {
        let dir = tempfile::TempDir::new().unwrap();
        let storage = provider(dir.path());
        let server = Rc::new(RefCell::new(InMemorySyncServer::new()));
        let (ws, dev) = ids("设备甲");
        let mut engine =
            LocalSyncEngine::open(Arc::clone(&storage), ws.clone(), dev.clone()).unwrap();
        let mut client = transport(&server, &ws, &dev);
        write_md(storage.as_ref(), "old.md", "内容");
        engine.handle_watch_events(&[WatchEvent::Created {
            path: DocPath::new("old.md").unwrap(),
        }]);
        engine.sync_once(&mut client).unwrap();

        storage
            .rename(
                &DocPath::new("old.md").unwrap(),
                &DocPath::new("new.md").unwrap(),
            )
            .unwrap();
        engine.handle_watch_events(&[WatchEvent::Renamed {
            from: DocPath::new("old.md").unwrap(),
            to: DocPath::new("new.md").unwrap(),
        }]);
        assert_eq!(engine.sync_once(&mut client).unwrap().staged_local, 2);
        assert!(server.borrow().is_tombstoned("old.md"));
        assert_eq!(server.borrow().text_of("new.md").unwrap(), "内容");
    }

    #[test]
    fn corrupt_state_is_reported_instead_of_falling_back_to_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        let storage = provider(dir.path());
        storage
            .write(
                &DocPath::new(".mazha/sync-state.bin").unwrap(),
                b"not a valid state file",
            )
            .unwrap();
        let (ws, dev) = ids("设备甲");
        assert!(matches!(
            LocalSyncEngine::open(Arc::clone(&storage), ws, dev),
            Err(LocalSyncError::CorruptState(_))
        ));
    }

    #[test]
    fn restored_tombstone_with_conflict_frames_is_kept() {
        let dir = tempfile::TempDir::new().unwrap();
        let storage = provider(dir.path());
        let server = Rc::new(RefCell::new(InMemorySyncServer::new()));
        server
            .borrow_mut()
            .seed_tombstone(
                Tombstone::new("旧.md", OpId::new(1, "设备甲").unwrap())
                    .unwrap()
                    .with_conflict_frame(CrdtDoc::from_text("被取代内容").encode_update())
                    .unwrap(),
            )
            .unwrap();
        let (ws, dev) = ids("设备甲");
        let mut engine =
            LocalSyncEngine::open(Arc::clone(&storage), ws.clone(), dev.clone()).unwrap();
        let mut client = transport(&server, &ws, &dev);
        engine.sync_once(&mut client).unwrap();
        assert!(engine.status().conflict_docs.contains(&"旧.md".to_owned()));

        // 重启后墓碑与冲突副本仍从状态文件恢复，不因内容帧被剔除而丢失。
        let reopened = LocalSyncEngine::open(Arc::clone(&storage), ws, dev).unwrap();
        assert!(reopened.session().roster().is_tombstoned("旧.md"));
        assert!(
            reopened
                .status()
                .conflict_docs
                .contains(&"旧.md".to_owned())
        );
    }

    /// 非回声观察必须清除旧写入标记：用户改回旧字节时应识别为真实编辑。
    #[test]
    fn stale_write_mark_does_not_swallow_later_revert() {
        let dir_a = tempfile::TempDir::new().unwrap();
        let dir_b = tempfile::TempDir::new().unwrap();
        let storage_a = provider(dir_a.path());
        let storage_b = provider(dir_b.path());
        let server = Rc::new(RefCell::new(InMemorySyncServer::new()));
        let (ws, dev_a) = ids("设备甲");
        let dev_b = DeviceId::new("设备乙").unwrap();
        let (mut client_a, mut client_b) =
            FakeTransport::pair(&server, ws.clone(), dev_a.clone(), dev_b.clone());

        let mut engine_a =
            LocalSyncEngine::open(Arc::clone(&storage_a), ws.clone(), dev_a).unwrap();
        write_md(storage_a.as_ref(), "a.md", "原文");
        engine_a.handle_watch_events(&[WatchEvent::Created {
            path: DocPath::new("a.md").unwrap(),
        }]);
        engine_a.sync_once(&mut client_a).unwrap();

        let mut engine_b = LocalSyncEngine::open(Arc::clone(&storage_b), ws, dev_b).unwrap();
        engine_b.sync_once(&mut client_b).unwrap();
        assert_eq!(read_md(storage_b.as_ref(), "a.md"), "原文");
        // B 的写回留下 Some("原文") 标记；若非回声时不清除，下一步“改回原文”会被误吞。
        write_md(storage_b.as_ref(), "a.md", "第二版");
        engine_b.handle_watch_events(&[WatchEvent::Modified {
            path: DocPath::new("a.md").unwrap(),
        }]);
        assert_eq!(engine_b.sync_once(&mut client_b).unwrap().staged_local, 1);
        write_md(storage_b.as_ref(), "a.md", "原文");
        engine_b.handle_watch_events(&[WatchEvent::Modified {
            path: DocPath::new("a.md").unwrap(),
        }]);
        assert_eq!(engine_b.sync_once(&mut client_b).unwrap().staged_local, 1);
        assert_eq!(server.borrow().text_of("a.md").unwrap(), "原文");
    }

    /// S5b/D104：造出「远端删除 → 本地看到墓碑冲突」的材料，供 resolve 测试复用。
    /// 返回设备乙的临时目录、Provider、引擎与对应 transport。
    fn conflict_fixture() -> (
        tempfile::TempDir,
        Arc<dyn StorageProvider>,
        LocalSyncEngine,
        mazha_sync::FakeTransport,
    ) {
        let dir_a = tempfile::TempDir::new().unwrap();
        let dir_b = tempfile::TempDir::new().unwrap();
        let storage_a = provider(dir_a.path());
        let storage_b = provider(dir_b.path());
        let server = Rc::new(RefCell::new(InMemorySyncServer::new()));
        let (ws, dev_a) = ids("设备甲");
        let dev_b = DeviceId::new("设备乙").unwrap();
        let (mut client_a, mut client_b) =
            FakeTransport::pair(&server, ws.clone(), dev_a.clone(), dev_b.clone());

        let mut engine_a =
            LocalSyncEngine::open(Arc::clone(&storage_a), ws.clone(), dev_a).unwrap();
        write_md(storage_a.as_ref(), "a.md", "冲突副本内容");
        engine_a.handle_watch_events(&[WatchEvent::Created {
            path: DocPath::new("a.md").unwrap(),
        }]);
        engine_a.sync_once(&mut client_a).unwrap();
        storage_a.remove(&DocPath::new("a.md").unwrap()).unwrap();
        engine_a.handle_watch_events(&[WatchEvent::Removed {
            path: DocPath::new("a.md").unwrap(),
        }]);
        engine_a.sync_once(&mut client_a).unwrap();

        let mut engine_b = LocalSyncEngine::open(Arc::clone(&storage_b), ws, dev_b).unwrap();
        engine_b.sync_once(&mut client_b).unwrap();
        assert!(engine_b.status().conflict_docs.contains(&"a.md".to_owned()));
        (dir_b, storage_b, engine_b, client_b)
    }

    #[test]
    fn resolve_conflict_keep_local_revives_tombstone_without_writing_file() {
        let (_dir, storage, mut engine, _client) = conflict_fixture();
        let before_batches = engine.session().pending_batches().len();
        let outcome = engine
            .resolve_conflict("a.md", "本地补写", ConflictChoice::KeepLocal)
            .unwrap();
        assert_eq!(outcome.document_text.as_deref(), Some("冲突副本内容"));
        assert_eq!(outcome.revived_docs, vec!["a.md".to_owned()]);
        assert!(outcome.confirmation_cleared);
        // 复活后墓碑消失，冲突横幅随之消退。
        assert!(!engine.session().roster().is_tombstoned("a.md"));
        assert_eq!(
            engine.session().roster().get("a.md").unwrap().text(),
            "冲突副本内容"
        );
        assert!(engine.status().conflict_docs.is_empty());
        assert_eq!(engine.session().pending_batches().len(), before_batches + 1);
        // 引擎不直接写盘：桌面拿到 document_text 后走既有保存路径。
        assert!(storage.read(&DocPath::new("a.md").unwrap()).is_err());
    }

    #[test]
    fn resolve_conflict_merge_and_use_remote_have_distinct_effects() {
        let (_dir, _storage, mut merged, _client) = conflict_fixture();
        let outcome = merged
            .resolve_conflict("a.md", "本地补写", ConflictChoice::Merge)
            .unwrap();
        assert_eq!(
            outcome.document_text.as_deref(),
            Some("冲突副本内容\n本地补写")
        );
        assert_eq!(
            merged.session().roster().get("a.md").unwrap().text(),
            "冲突副本内容\n本地补写"
        );

        let (_dir2, storage2, mut remote, _client2) = conflict_fixture();
        let before_batches = remote.session().pending_batches().len();
        let outcome = remote
            .resolve_conflict("a.md", "本地补写", ConflictChoice::UseRemote)
            .unwrap();
        assert!(outcome.document_text.is_none());
        assert!(outcome.revived_docs.is_empty());
        // 采用远端：墓碑与冲突副本保留审计，不写回正文、不产生新批次。
        assert!(remote.session().roster().is_tombstoned("a.md"));
        // 已确认的同一冲突在本会话内不再重复弹横幅（墓碑仍在，仅横幅被抑制）。
        assert!(!remote.status().conflict_docs.contains(&"a.md".to_owned()));
        assert_eq!(remote.session().pending_batches().len(), before_batches);
        assert!(storage2.read(&DocPath::new("a.md").unwrap()).is_err());
    }

    /// 已确认的冲突/门槛在同一会话内不得每轮重现；新冲突/新批次仍须提示。
    #[test]
    fn acknowledged_conflicts_are_not_repeated_until_content_changes() {
        // 冲突：确认 UseRemote 后墓碑仍在，下一轮 sync_once 不应再弹横幅。
        let (_dir, _storage, mut engine, mut client) = conflict_fixture();
        let outcome = engine
            .resolve_conflict("a.md", "本地补写", ConflictChoice::UseRemote)
            .unwrap();
        assert!(outcome.document_text.is_none());
        assert!(!engine.status().conflict_docs.contains(&"a.md".to_owned()));
        engine.sync_once(&mut client).unwrap();
        assert!(!engine.status().conflict_docs.contains(&"a.md".to_owned()));
        assert!(engine.session().roster().is_tombstoned("a.md"));

        // 快照门槛：确认 KeepLocal 后 outbox 未变，下一轮不应再置 needs_confirmation。
        let dir_a = tempfile::TempDir::new().unwrap();
        let dir_b = tempfile::TempDir::new().unwrap();
        let storage_a = provider(dir_a.path());
        let storage_b = provider(dir_b.path());
        let server = Rc::new(RefCell::new(InMemorySyncServer::new()));
        let (ws, dev_a) = ids("设备甲");
        let dev_b = DeviceId::new("设备乙").unwrap();
        let (mut client_a, mut client_b) =
            FakeTransport::pair(&server, ws.clone(), dev_a.clone(), dev_b.clone());
        let mut a = LocalSyncEngine::open(Arc::clone(&storage_a), ws.clone(), dev_a).unwrap();
        write_md(storage_a.as_ref(), "a.md", "服务端版");
        a.handle_watch_events(&[WatchEvent::Created {
            path: DocPath::new("a.md").unwrap(),
        }]);
        a.sync_once(&mut client_a).unwrap();
        let synced_cursor = server.borrow().cursor();

        let mut b = LocalSyncEngine::open(Arc::clone(&storage_b), ws, dev_b).unwrap();
        b.sync_once(&mut client_b).unwrap();
        write_md(storage_b.as_ref(), "a.md", "本地未同步");
        b.handle_watch_events(&[WatchEvent::Modified {
            path: DocPath::new("a.md").unwrap(),
        }]);
        b.drain_dirty().unwrap();
        server.borrow_mut().seed_text("b.md", "新增").unwrap();
        server.borrow_mut().prune_history(synced_cursor);
        assert!(b.sync_once(&mut client_b).is_err());
        assert!(b.status().needs_confirmation);
        b.resolve_snapshot_confirmation(SnapshotChoice::KeepLocal)
            .unwrap();
        assert!(!b.status().needs_confirmation);
        // outbox 未变，同一门槛不应重复出现。
        assert!(b.sync_once(&mut client_b).is_err());
        assert!(!b.status().needs_confirmation);
    }

    #[test]
    fn resolve_snapshot_confirmation_choices_are_explicit() {
        let dir_a = tempfile::TempDir::new().unwrap();
        let dir_b = tempfile::TempDir::new().unwrap();
        let storage_a = provider(dir_a.path());
        let storage_b = provider(dir_b.path());
        let server = Rc::new(RefCell::new(InMemorySyncServer::new()));
        let (ws, dev_a) = ids("设备甲");
        let dev_b = DeviceId::new("设备乙").unwrap();
        let (mut client_a, mut client_b) =
            FakeTransport::pair(&server, ws.clone(), dev_a.clone(), dev_b.clone());
        let mut engine_a =
            LocalSyncEngine::open(Arc::clone(&storage_a), ws.clone(), dev_a).unwrap();
        write_md(storage_a.as_ref(), "a.md", "服务端版");
        engine_a.handle_watch_events(&[WatchEvent::Created {
            path: DocPath::new("a.md").unwrap(),
        }]);
        engine_a.sync_once(&mut client_a).unwrap();
        let synced_cursor = server.borrow().cursor();

        let mut engine_b = LocalSyncEngine::open(Arc::clone(&storage_b), ws, dev_b).unwrap();
        engine_b.sync_once(&mut client_b).unwrap();
        write_md(storage_b.as_ref(), "a.md", "本地未同步");
        engine_b.handle_watch_events(&[WatchEvent::Modified {
            path: DocPath::new("a.md").unwrap(),
        }]);
        engine_b.drain_dirty().unwrap();
        server.borrow_mut().seed_text("b.md", "新增").unwrap();
        server.borrow_mut().prune_history(synced_cursor);
        assert!(engine_b.sync_once(&mut client_b).is_err());
        assert!(engine_b.status().needs_confirmation);
        let before = engine_b.session().pending_batches().len();

        // KeepLocal / Merge 都不清空 outbox，只消除本轮确认标记。
        let keep = engine_b
            .resolve_snapshot_confirmation(SnapshotChoice::KeepLocal)
            .unwrap();
        assert!(!keep.outbox_cleared);
        assert!(keep.confirmation_cleared);
        assert_eq!(engine_b.session().pending_batches().len(), before);
        assert!(!engine_b.status().needs_confirmation);
        assert_eq!(read_md(storage_b.as_ref(), "a.md"), "本地未同步");
        let merge = engine_b
            .resolve_snapshot_confirmation(SnapshotChoice::Merge)
            .unwrap();
        assert!(!merge.outbox_cleared);
        assert_eq!(engine_b.session().pending_batches().len(), before);

        // UseRemote 是唯一会显式丢弃未推送批次的分支。
        let use_remote = engine_b
            .resolve_snapshot_confirmation(SnapshotChoice::UseRemote)
            .unwrap();
        assert!(use_remote.outbox_cleared);
        assert!(engine_b.session().pending_batches().is_empty());
        assert_eq!(read_md(storage_b.as_ref(), "a.md"), "本地未同步");
    }

    #[test]
    fn resolve_unknown_conflict_leaves_state_unchanged() {
        let (_dir, _storage, mut engine, _client) = conflict_fixture();
        let before = engine.status().clone();
        assert!(
            engine
                .resolve_conflict("不存在.md", "文本", ConflictChoice::KeepLocal)
                .is_err()
        );
        assert_eq!(engine.status(), &before);
        assert!(engine.status().conflict_docs.contains(&"a.md".to_owned()));
    }

    /// 断线时已暂存的编辑必须立即落盘，进程重启后仍能重放 outbox。
    #[test]
    fn staged_offline_edit_survives_reopen() {
        let dir = tempfile::TempDir::new().unwrap();
        let storage = provider(dir.path());
        let server = Rc::new(RefCell::new(InMemorySyncServer::new()));
        let (ws, dev) = ids("设备甲");
        let mut engine =
            LocalSyncEngine::open(Arc::clone(&storage), ws.clone(), dev.clone()).unwrap();
        let mut online = transport(&server, &ws, &dev);
        write_md(storage.as_ref(), "a.md", "原文");
        engine.handle_watch_events(&[WatchEvent::Created {
            path: DocPath::new("a.md").unwrap(),
        }]);
        engine.sync_once(&mut online).unwrap();
        assert_eq!(server.borrow().text_of("a.md").unwrap(), "原文");

        // 断线编辑 → drain 暂存 → sync_once 失败；重启前不再有任何成功同步。
        let mut offline = transport(&server, &ws, &dev);
        offline.set_connected(false);
        write_md(storage.as_ref(), "a.md", "离线编辑");
        engine.handle_watch_events(&[WatchEvent::Modified {
            path: DocPath::new("a.md").unwrap(),
        }]);
        assert!(engine.sync_once(&mut offline).is_err());
        drop(engine);

        let mut reopened =
            LocalSyncEngine::open(Arc::clone(&storage), ws.clone(), dev.clone()).unwrap();
        assert_eq!(reopened.session().pending_batches().len(), 1);
        assert_eq!(
            reopened.session().roster().get("a.md").unwrap().text(),
            "离线编辑"
        );
        let mut online_again = transport(&server, &ws, &dev);
        reopened.sync_once(&mut online_again).unwrap();
        assert_eq!(server.borrow().text_of("a.md").unwrap(), "离线编辑");
    }
}
