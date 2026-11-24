//! 本地同步状态持久化（D101，契约见 `docs/SYNC-SPEC.md` §5.9.4）。
//!
//! 状态文件固定为工作区内 `.mazha/sync-state.bin`，通过 `StorageProvider` 的 `DocPath`
//! 读写并采用 temp+rename 原子替换。磁盘格式复用 `mazha-sync::wire` 的小端/长度前缀/
//! CRC32 助手，但用独立的 `MZLS` magic 与线上 `MZS0` 帧区分。坏文件一律报错，
//! 不回退空状态（那会丢游标与批次计数器，导致服务端去重吞掉新编辑）。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use mazha_crdt::{DocId, RosterMeta};
use mazha_store::{DocPath, StorageProvider, StoreError};
use mazha_sync::DocFrameMap;
use mazha_sync::wire::{
    WireReader, crc32, put_bytes, put_frame_map, put_meta, put_meta_changes, put_string, put_u8,
    put_u32, put_u64, read_frame_map, read_meta, read_meta_changes,
};
use mazha_sync::{BatchId, PendingBatch, SyncCursor};

use crate::error::LocalSyncError;

/// 状态文件 magic（与线上 `MZS0` 区分）。
const MAGIC: [u8; 4] = *b"MZLS";

/// 当前状态文件格式版本（S5b 第二批升 v2，追加已确认指纹）。
const VERSION: u32 = 2;

/// 旧版格式：只含 v1 字段，读取时新字段初始化为空（D107）。
const VERSION_V1: u32 = 1;

/// 状态文件工作区相对路径。
fn state_path() -> DocPath {
    DocPath::new(".mazha/sync-state.bin").expect("常量路径合法")
}

/// 临时文件路径（写入成功后 rename 到 `state_path`）。
fn temp_path() -> DocPath {
    DocPath::new(".mazha/sync-state.tmp").expect("常量路径合法")
}

/// 可持久化的本地同步状态。
///
/// 除名册内容帧与墓碑元数据外，**必须**带上逻辑时钟、批次计数器与未确认 outbox：
/// 否则重启后批次号从 0 重来，会被服务端 `(workspace, device, batch)` 去重吞掉新编辑。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSyncState {
    /// 名册全量内容帧（ID → Yjs v1）。
    pub frames: DocFrameMap,
    /// 名册元数据（墓碑与冲突副本）。
    pub meta: RosterMeta,
    /// 最后确认的服务端游标。
    pub last_cursor: SyncCursor,
    /// 服务端会话 ID（重启后仅作诊断，仍需重新握手）。
    pub session_id: Option<String>,
    /// `OpId` 逻辑时钟。
    pub op_clock: u64,
    /// 出站批次计数器。
    pub batch_counter: u64,
    /// 未确认出站批次（FIFO 原序）。
    pub outbox: Vec<PendingBatch>,
    /// 已显式确认的冲突帧指纹：DocId → 冲突帧字节列表（v2 新增，v1 读为空）。
    pub resolved_conflict_frames: BTreeMap<DocId, Vec<Vec<u8>>>,
    /// 已显式确认的快照门槛 outbox 批次指纹（v2 新增，v1 读为 None）。
    pub resolved_snapshot_batches: Option<BTreeSet<String>>,
}

impl Default for LocalSyncState {
    fn default() -> Self {
        Self {
            frames: BTreeMap::new(),
            meta: RosterMeta::new(),
            last_cursor: SyncCursor::new(0),
            session_id: None,
            op_clock: 0,
            batch_counter: 0,
            outbox: Vec::new(),
            resolved_conflict_frames: BTreeMap::new(),
            resolved_snapshot_batches: None,
        }
    }
}

/// 状态文件访问器：只依赖 `StorageProvider`，不自行做文件 IO。
#[derive(Clone)]
pub struct LocalSyncStore {
    /// 工作区存储 Provider。
    provider: Arc<dyn StorageProvider>,
}

impl LocalSyncStore {
    /// 用给定 Provider 构造状态存储。
    #[must_use]
    pub fn new(provider: Arc<dyn StorageProvider>) -> Self {
        Self { provider }
    }

    /// 加载状态；文件不存在时返回 `Ok(None)`（首次启动），坏文件返回错误。
    ///
    /// # Errors
    /// 状态文件损坏/版本不支持返回 [`LocalSyncError::CorruptState`]；
    /// 除「不存在」外的存储错误原样透传。
    pub fn load(&self) -> Result<Option<LocalSyncState>, LocalSyncError> {
        let bytes = match self.provider.read(&state_path()) {
            Ok(bytes) => bytes,
            Err(StoreError::NotFound(_)) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        decode_state(&bytes).map(Some)
    }

    /// 原子写入状态：先写临时文件，再 rename 覆盖正式文件。
    ///
    /// 任一步失败都尝试清理临时文件；成功后不留下临时文件。
    ///
    /// # Errors
    /// 编码、写盘或 rename 失败时返回对应错误。
    pub fn save(&self, state: &LocalSyncState) -> Result<(), LocalSyncError> {
        let bytes = encode_state(state);
        let temp = temp_path();
        if let Err(error) = self.provider.write(&temp, &bytes) {
            let _ = self.provider.remove(&temp);
            return Err(error.into());
        }
        if let Err(error) = self.provider.rename(&temp, &state_path()) {
            // 临时文件可能已写出一半，失败时清理，避免下次误读。
            let _ = self.provider.remove(&temp);
            return Err(error.into());
        }
        Ok(())
    }
}

/// 把状态编码为磁盘字节（始终写当前 v2）：头 + 负载 + CRC32。
fn encode_state(state: &LocalSyncState) -> Vec<u8> {
    encode_state_version(state, VERSION)
}

/// 按指定版本编码，供 v1 兼容测试复用同一套字段布局。
fn encode_state_version(state: &LocalSyncState, version: u32) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC);
    put_u32(&mut out, version);
    put_u64(&mut out, state.last_cursor.get());
    put_u64(&mut out, state.op_clock);
    put_u64(&mut out, state.batch_counter);
    match &state.session_id {
        Some(id) => {
            put_u8(&mut out, 1);
            put_string(&mut out, id);
        }
        None => put_u8(&mut out, 0),
    }
    put_frame_map(&mut out, &state.frames);
    put_meta(&mut out, &state.meta);
    put_u32(
        &mut out,
        u32::try_from(state.outbox.len()).unwrap_or(u32::MAX),
    );
    for batch in &state.outbox {
        let (batch_id, frames, changes) = batch.clone().into_parts();
        put_string(&mut out, batch_id.as_str());
        put_frame_map(&mut out, &frames);
        put_meta_changes(&mut out, &changes);
    }

    // v2 新字段：已确认指纹，均受末尾 CRC 覆盖；v1 不写这两段。
    if version >= VERSION {
        put_u32(
            &mut out,
            u32::try_from(state.resolved_conflict_frames.len()).unwrap_or(u32::MAX),
        );
        for (doc_id, frames) in &state.resolved_conflict_frames {
            put_string(&mut out, doc_id);
            put_u32(&mut out, u32::try_from(frames.len()).unwrap_or(u32::MAX));
            for frame in frames {
                put_bytes(&mut out, frame);
            }
        }
        match &state.resolved_snapshot_batches {
            None => put_u8(&mut out, 0),
            Some(batches) => {
                put_u8(&mut out, 1);
                put_u32(&mut out, u32::try_from(batches.len()).unwrap_or(u32::MAX));
                for batch in batches {
                    put_string(&mut out, batch);
                }
            }
        }
    }
    let checksum = crc32(&out);
    put_u32(&mut out, checksum);
    out
}

/// 读取一个带上限的数量，防止损坏文件诱导超大预分配。
fn read_count(reader: &mut WireReader<'_>) -> Result<usize, LocalSyncError> {
    let declared = reader.read_u32()?;
    let count = usize::try_from(declared)
        .map_err(|_| LocalSyncError::CorruptState("状态文件数量字段超出可用范围".to_owned()))?;
    if count > 1_000_000 {
        return Err(LocalSyncError::CorruptState(format!(
            "状态文件数量异常：{count}"
        )));
    }
    Ok(count)
}

/// 把磁盘字节解码为状态；任何非法字段都映射为 `CorruptState`。
fn decode_state(bytes: &[u8]) -> Result<LocalSyncState, LocalSyncError> {
    // CRC 是最后 4 字节，其余字节全部参与校验（先校验再解释字段，避免误读垃圾）。
    if bytes.len() < MAGIC.len() + 4 + 4 {
        return Err(LocalSyncError::CorruptState("状态文件被截断".to_owned()));
    }
    let body_end = bytes.len() - 4;
    let expected = u32::from_le_bytes(
        bytes[body_end..]
            .try_into()
            .map_err(|_| LocalSyncError::CorruptState("CRC 字段非法".to_owned()))?,
    );
    if crc32(&bytes[..body_end]) != expected {
        return Err(LocalSyncError::CorruptState(
            "状态文件 CRC 校验失败".to_owned(),
        ));
    }
    let mut reader = WireReader::new(&bytes[..body_end]);
    let mut magic = [0u8; 4];
    for slot in &mut magic {
        *slot = reader.read_u8()?;
    }
    if magic != MAGIC {
        return Err(LocalSyncError::CorruptState(
            "状态文件 magic 不匹配".to_owned(),
        ));
    }
    let version = reader.read_u32()?;
    // v1 文件缺新增两段，读为空指纹即可平滑升级；未知版本仍拒绝。
    if version != VERSION && version != VERSION_V1 {
        return Err(LocalSyncError::CorruptState(format!(
            "不支持的状态文件版本：{version}"
        )));
    }
    let last_cursor = SyncCursor::new(reader.read_u64()?);
    let op_clock = reader.read_u64()?;
    let batch_counter = reader.read_u64()?;
    let session_id = match reader.read_u8()? {
        0 => None,
        1 => Some(reader.read_string()?),
        other => {
            return Err(LocalSyncError::CorruptState(format!(
                "会话 ID 标志非法：{other}"
            )));
        }
    };
    let frames = read_frame_map(&mut reader)?;
    let meta = read_meta(&mut reader)?;
    let outbox_len = read_count(&mut reader)?;
    let mut outbox = Vec::with_capacity(outbox_len);
    for _ in 0..outbox_len {
        let raw_id = reader.read_string()?;
        let batch_id = BatchId::parse(raw_id).map_err(|error| {
            LocalSyncError::CorruptState(format!("状态文件批次号非法：{error}"))
        })?;
        let batch_frames = read_frame_map(&mut reader)?;
        let changes = read_meta_changes(&mut reader)?;
        outbox.push(PendingBatch::new(batch_id, batch_frames, changes));
    }

    let mut resolved_conflict_frames = BTreeMap::new();
    let mut resolved_snapshot_batches = None;
    if version >= VERSION {
        let doc_count = read_count(&mut reader)?;
        for _ in 0..doc_count {
            let doc_id = reader.read_string()?;
            let frame_count = read_count(&mut reader)?;
            let mut frames = Vec::with_capacity(frame_count);
            for _ in 0..frame_count {
                frames.push(reader.read_bytes()?.to_vec());
            }
            resolved_conflict_frames.insert(doc_id, frames);
        }
        resolved_snapshot_batches = match reader.read_u8()? {
            0 => None,
            1 => {
                let count = read_count(&mut reader)?;
                let mut batches = BTreeSet::new();
                for _ in 0..count {
                    batches.insert(reader.read_string()?);
                }
                Some(batches)
            }
            other => {
                return Err(LocalSyncError::CorruptState(format!(
                    "已确认快照批次标志非法：{other}"
                )));
            }
        };
    }
    reader.ensure_finished()?;
    Ok(LocalSyncState {
        frames,
        meta,
        last_cursor,
        session_id,
        op_clock,
        batch_counter,
        outbox,
        resolved_conflict_frames,
        resolved_snapshot_batches,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mazha_crdt::{CrdtDoc, MetaChange, OpId, Tombstone};
    use mazha_store::FileSystemProvider;
    use tempfile::TempDir;

    /// 造一个含帧、墓碑与两个待推送批次的状态，覆盖所有字段。
    fn rich_state() -> LocalSyncState {
        let mut frames = BTreeMap::new();
        frames.insert(
            "甲.md".to_owned(),
            CrdtDoc::from_text("你好").encode_update(),
        );
        let mut meta = RosterMeta::new();
        meta.merge_tombstone(
            Tombstone::new("旧.md", OpId::new(3, "设备甲").unwrap())
                .unwrap()
                .with_conflict_frame(CrdtDoc::from_text("旧内容").encode_update())
                .unwrap(),
        )
        .unwrap();
        let batch = PendingBatch::new(
            BatchId::parse("设备甲-00000000000000000000").unwrap(),
            frames.clone(),
            vec![MetaChange::Upsert {
                doc_id: "甲.md".to_owned(),
                op_id: OpId::new(4, "设备甲").unwrap(),
                frame: frames["甲.md"].clone(),
            }],
        );
        LocalSyncState {
            frames,
            meta,
            last_cursor: SyncCursor::new(9),
            session_id: Some("sess-9".to_owned()),
            op_clock: 7,
            batch_counter: 1,
            outbox: vec![batch],
            resolved_conflict_frames: BTreeMap::from([(
                "旧.md".to_owned(),
                vec![CrdtDoc::from_text("已确认副本").encode_update()],
            )]),
            resolved_snapshot_batches: Some(BTreeSet::from([
                "设备甲-00000000000000000000".to_owned()
            ])),
        }
    }

    #[test]
    fn state_bytes_roundtrip_and_are_deterministic() {
        let state = rich_state();
        let bytes = encode_state(&state);
        assert_eq!(&bytes[..4], b"MZLS");
        assert_eq!(decode_state(&bytes).unwrap(), state);
        assert_eq!(encode_state(&state), bytes);
    }

    #[test]
    fn state_rejects_bad_magic_version_crc_and_truncation() {
        let good = encode_state(&rich_state());
        let mut bad_magic = good.clone();
        bad_magic[0] = b'X';
        assert!(matches!(
            decode_state(&bad_magic),
            Err(LocalSyncError::CorruptState(_))
        ));

        let mut bad_version = good.clone();
        bad_version[4..8].copy_from_slice(&3u32.to_le_bytes());
        assert!(matches!(
            decode_state(&bad_version),
            Err(LocalSyncError::CorruptState(_))
        ));

        let mut bad_crc = good.clone();
        let last = bad_crc.len() - 1;
        bad_crc[last] ^= 0x01;
        assert!(matches!(
            decode_state(&bad_crc),
            Err(LocalSyncError::CorruptState(_))
        ));

        let truncated = &good[..good.len() - 1];
        assert!(matches!(
            decode_state(truncated),
            Err(LocalSyncError::CorruptState(_))
        ));
    }

    #[test]
    fn v1_state_is_readable_with_empty_fingerprints() {
        let state = rich_state();
        let bytes = encode_state_version(&state, VERSION_V1);
        let decoded = decode_state(&bytes).unwrap();
        let mut expected = state;
        expected.resolved_conflict_frames.clear();
        expected.resolved_snapshot_batches = None;
        assert_eq!(decoded, expected);
    }

    #[test]
    fn unknown_version_with_valid_crc_is_rejected() {
        let bytes = encode_state_version(&rich_state(), 3);
        assert!(matches!(
            decode_state(&bytes),
            Err(LocalSyncError::CorruptState(_))
        ));
    }

    #[test]
    fn store_load_missing_is_none_and_save_is_atomic() {
        let dir = TempDir::new().unwrap();
        let provider: Arc<dyn StorageProvider> =
            Arc::new(FileSystemProvider::open(dir.path()).unwrap());
        let store = LocalSyncStore::new(Arc::clone(&provider));
        assert!(store.load().unwrap().is_none());

        let state = rich_state();
        store.save(&state).unwrap();
        assert_eq!(store.load().unwrap(), Some(state));
        assert!(provider.read(&temp_path()).is_err());
        assert!(provider.read(&state_path()).is_ok());
    }
}
