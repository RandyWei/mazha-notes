//! 工作区持久化层（D100，契约见 `docs/SYNC-SPEC.md` §7.4.1）。
//!
//! `SyncStore` 只暴露「按工作区加载/保存状态」两个操作；`FileSyncStore` 用单文件 +
//! 临时文件 rename 实现原子替换，文件名是工作区 ID 的 UTF-8 十六进制，天然避免路径穿越。
//! 磁盘编码复用 `mazha-sync::wire` 的小端/长度前缀助手，与线上帧各自有独立 magic。

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use mazha_crdt::RosterMeta;
use mazha_sync::wire::{
    MAX_FRAME_BYTES, WireError, WireReader, crc32, put_frame_map, put_meta, put_meta_changes,
    put_string, put_u32, put_u64, read_frame_map, read_meta, read_meta_changes,
};
use mazha_sync::{BatchId, DeviceId, DocFrameMap, SyncCursor, WorkspaceId};

/// 磁盘状态文件 magic：与线上帧的 `MZS0` 区分，避免误把状态当同步帧解码。
const STATE_MAGIC: [u8; 4] = *b"MZWS";

/// 磁盘格式版本，与线上 `WIRE_VERSION` 独立演进。
const STATE_FORMAT_VERSION: u32 = 1;

/// 持久化错误：IO、字节格式与 CRC 分层，便于上层映射为 `Internal`。
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// 文件系统错误（目录创建/读写/rename）。
    #[error("存储 IO 错误：{0}")]
    Io(#[from] std::io::Error),
    /// 复用 wire 助手时的字节解码错误。
    #[error("持久化字节非法：{0}")]
    Wire(#[from] WireError),
    /// 不支持的磁盘格式版本。
    #[error("不支持的持久化格式版本：{0}")]
    UnsupportedFormat(u32),
    /// 状态文件 CRC 校验失败。
    #[error("持久化文件 CRC 校验失败")]
    BadCrc,
    /// 状态文件被截断。
    #[error("持久化文件被截断")]
    Truncated,
    /// 状态超过帧上限，拒绝落盘（避免写出无法读回的巨大文件）。
    #[error("持久化状态超过大小上限")]
    TooLarge,
}

/// 变更日志的一项：游标 + 该批次内容帧 + 元数据变更。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    /// 提交后的服务端游标。
    pub cursor: SyncCursor,
    /// 该批次携带的内容帧。
    pub frames: DocFrameMap,
    /// 该批次携带的元数据变更。
    pub meta_changes: Vec<mazha_crdt::MetaChange>,
}

/// 单工作区的完整可恢复状态。
///
/// `frames` 是当前全量内容帧、`meta` 是墓碑快照；`log` 保存按游标编号的变更，
/// `dedup` 记录 `(device, batch) -> 首次 ack 游标`（工作区已由文件名区分，故不入键）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceState {
    /// 当前服务端游标。
    pub cursor: SyncCursor,
    /// 日志裁剪下界；`since < log_floor` 要求快照引导。
    pub log_floor: SyncCursor,
    /// 会话计数器，用于生成确定性 `session_id`。
    pub session_counter: u64,
    /// 当前全量内容帧（标准 Yjs v1）。
    pub frames: DocFrameMap,
    /// 当前墓碑快照。
    pub meta: RosterMeta,
    /// 按提交顺序排列的变更日志。
    pub log: Vec<LogEntry>,
    /// 已应用批次 → 首次 ack 游标。
    pub dedup: BTreeMap<(DeviceId, BatchId), SyncCursor>,
}

impl Default for WorkspaceState {
    fn default() -> Self {
        Self {
            cursor: SyncCursor::new(0),
            log_floor: SyncCursor::new(0),
            session_counter: 0,
            frames: BTreeMap::new(),
            meta: RosterMeta::new(),
            log: Vec::new(),
            dedup: BTreeMap::new(),
        }
    }
}

/// 工作区状态存储抽象：只负责加载与原子保存，不含协议规则。
pub trait SyncStore {
    /// 加载工作区状态；首次访问时应返回空状态而不是错误。
    ///
    /// # Errors
    /// 文件存在但损坏、或 IO 失败时返回 [`StoreError`]。
    fn load(&self, workspace: &WorkspaceId) -> Result<WorkspaceState, StoreError>;

    /// 保存工作区状态，必须保证失败时旧文件完整可用。
    ///
    /// # Errors
    /// 编码、目录创建、写入或 rename 失败时返回 [`StoreError`]。
    fn save(&self, workspace: &WorkspaceId, state: &WorkspaceState) -> Result<(), StoreError>;
}

/// 基于本地文件系统的 `SyncStore` 实现（temp + rename 原子写）。
#[derive(Debug, Clone)]
pub struct FileSyncStore {
    /// 状态文件所在目录。
    root: PathBuf,
}

impl FileSyncStore {
    /// 构造文件存储；目录在首次 `save` 时按需创建。
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// 存储根目录（测试/诊断用）。
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 工作区 ID → 十六进制文件名：只含 `[0-9a-f]`，杜绝 `../` 之类路径穿越。
    fn path_for(&self, workspace: &WorkspaceId) -> PathBuf {
        let mut encoded = String::with_capacity(workspace.as_str().len() * 2);
        for byte in workspace.as_str().as_bytes() {
            // 忽略 fmt::Write 的返回值：写 String 不会失败，这里只关心生成的 hex。
            let _ = write!(encoded, "{byte:02x}");
        }
        self.root.join(format!("{encoded}.ws"))
    }
}

impl SyncStore for FileSyncStore {
    fn load(&self, workspace: &WorkspaceId) -> Result<WorkspaceState, StoreError> {
        let path = self.path_for(workspace);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(WorkspaceState::default());
            }
            Err(error) => return Err(StoreError::Io(error)),
        };
        decode_state(&bytes)
    }

    fn save(&self, workspace: &WorkspaceId, state: &WorkspaceState) -> Result<(), StoreError> {
        let bytes = encode_state(state)?;
        fs::create_dir_all(&self.root)?;
        let target = self.path_for(workspace);
        let temp = target.with_extension("tmp");
        // 先写临时文件并完整落盘，再 rename：进程被杀时只会留下旧文件或新文件，
        // 不会出现被截断的半状态（与 §7.4.1 的原子性契约一致）。
        fs::write(&temp, &bytes)?;
        fs::rename(&temp, &target)?;
        Ok(())
    }
}

/// 把工作区状态编码为磁盘字节：`magic | version | 字段 | CRC32`。
fn encode_state(state: &WorkspaceState) -> Result<Vec<u8>, StoreError> {
    let mut out = Vec::new();
    out.extend_from_slice(&STATE_MAGIC);
    put_u32(&mut out, STATE_FORMAT_VERSION);
    put_u64(&mut out, state.cursor.get());
    put_u64(&mut out, state.log_floor.get());
    put_u64(&mut out, state.session_counter);

    put_frame_map(&mut out, &state.frames);
    put_meta(&mut out, &state.meta);

    put_u32(&mut out, u32::try_from(state.log.len()).unwrap_or(u32::MAX));
    for entry in &state.log {
        put_u64(&mut out, entry.cursor.get());
        put_frame_map(&mut out, &entry.frames);
        put_meta_changes(&mut out, &entry.meta_changes);
    }

    put_u32(
        &mut out,
        u32::try_from(state.dedup.len()).unwrap_or(u32::MAX),
    );
    for ((device, batch), cursor) in &state.dedup {
        put_string(&mut out, device.as_str());
        put_string(&mut out, batch.as_str());
        put_u64(&mut out, cursor.get());
    }

    if out.len() > MAX_FRAME_BYTES {
        return Err(StoreError::TooLarge);
    }
    // 先算 CRC 再追加：避免同一 Vec 的不可变借用（算校验和）与可变借用（写入）重叠。
    let checksum = crc32(&out);
    put_u32(&mut out, checksum);
    Ok(out)
}

/// 解码磁盘字节；CRC 覆盖 `magic..字段`（不含末尾 4 字节 CRC）。
fn decode_state(bytes: &[u8]) -> Result<WorkspaceState, StoreError> {
    if bytes.len() < STATE_MAGIC.len() + 4 + 4 {
        return Err(StoreError::Truncated);
    }
    if &bytes[..STATE_MAGIC.len()] != STATE_MAGIC.as_slice() {
        return Err(StoreError::Wire(WireError::BadMagic));
    }
    let version = u32::from_le_bytes(
        bytes[STATE_MAGIC.len()..STATE_MAGIC.len() + 4]
            .try_into()
            .map_err(|_| StoreError::Truncated)?,
    );
    if version != STATE_FORMAT_VERSION {
        return Err(StoreError::UnsupportedFormat(version));
    }
    let body_end = bytes.len() - 4;
    let raw_crc: [u8; 4] = bytes[body_end..]
        .try_into()
        .map_err(|_| StoreError::Truncated)?;
    if crc32(&bytes[..body_end]) != u32::from_le_bytes(raw_crc) {
        return Err(StoreError::BadCrc);
    }

    let mut reader = WireReader::new(&bytes[STATE_MAGIC.len() + 4..body_end]);
    let cursor = SyncCursor::new(reader.read_u64()?);
    let log_floor = SyncCursor::new(reader.read_u64()?);
    let session_counter = reader.read_u64()?;
    let frames = read_frame_map(&mut reader)?;
    let meta = read_meta(&mut reader)?;

    let log_count = read_bounded_count(&mut reader)?;
    let mut log = Vec::with_capacity(log_count);
    for _ in 0..log_count {
        log.push(LogEntry {
            cursor: SyncCursor::new(reader.read_u64()?),
            frames: read_frame_map(&mut reader)?,
            meta_changes: read_meta_changes(&mut reader)?,
        });
    }

    let dedup_count = read_bounded_count(&mut reader)?;
    let mut dedup = BTreeMap::new();
    for _ in 0..dedup_count {
        let device = DeviceId::new(reader.read_string()?)
            .map_err(|_| StoreError::Wire(WireError::InvalidId { field: "device_id" }))?;
        let batch = BatchId::parse(reader.read_string()?)
            .map_err(|_| StoreError::Wire(WireError::InvalidId { field: "batch_id" }))?;
        let cursor = SyncCursor::new(reader.read_u64()?);
        dedup.insert((device, batch), cursor);
    }
    reader.ensure_finished()?;
    Ok(WorkspaceState {
        cursor,
        log_floor,
        session_counter,
        frames,
        meta,
        log,
        dedup,
    })
}

/// 读取带上限的数量前缀，避免损坏文件触发超大预分配。
fn read_bounded_count(reader: &mut WireReader<'_>) -> Result<usize, StoreError> {
    let declared = reader.read_u32()?;
    let count = usize::try_from(declared).unwrap_or(usize::MAX);
    if count > MAX_FRAME_BYTES {
        return Err(StoreError::Wire(WireError::BodyTooLarge(count)));
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use mazha_crdt::{CrdtDoc, MetaChange, OpId, Tombstone};

    use super::*;

    /// 每个测试用独立临时目录，避免并行测试互相覆盖状态文件。
    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("mazha-store-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    /// 测试用合法 Yjs v1 全量帧。
    fn frame(text: &str) -> Vec<u8> {
        CrdtDoc::from_text(text).encode_update()
    }

    /// 构造含帧/墓碑/日志/去重的非空状态。
    fn sample_state() -> WorkspaceState {
        let tombstone = Tombstone::new("笔记/旧.md", OpId::new(4, "设备-B").unwrap())
            .unwrap()
            .with_conflict_frame(frame("旧内容"))
            .unwrap();
        let mut meta = RosterMeta::new();
        meta.merge_tombstone(tombstone.clone()).unwrap();
        let mut frames = BTreeMap::new();
        frames.insert("notes/a.md".to_owned(), frame("甲内容"));
        let batch = BatchId::new(&DeviceId::new("设备-A").unwrap(), 3).unwrap();
        let mut dedup = BTreeMap::new();
        dedup.insert(
            (DeviceId::new("设备-A").unwrap(), batch),
            SyncCursor::new(2),
        );
        WorkspaceState {
            cursor: SyncCursor::new(2),
            log_floor: SyncCursor::new(1),
            session_counter: 5,
            frames,
            meta,
            log: vec![LogEntry {
                cursor: SyncCursor::new(2),
                frames: BTreeMap::new(),
                meta_changes: vec![MetaChange::Tombstone(tombstone)],
            }],
            dedup,
        }
    }

    #[test]
    fn state_survives_store_restart() {
        let root = temp_root("restart");
        let store = FileSyncStore::new(&root);
        let workspace = WorkspaceId::new("../工作区/甲").unwrap();
        let state = sample_state();
        store.save(&workspace, &state).unwrap();

        // 新建 store 实例模拟进程重启：文件仍在同一目录，内容逐字段一致。
        let reopened = FileSyncStore::new(&root);
        assert_eq!(reopened.load(&workspace).unwrap(), state);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn workspace_id_is_hex_encoded_without_path_traversal() {
        let root = temp_root("hex");
        let store = FileSyncStore::new(&root);
        let workspace = WorkspaceId::new("../evil/路径").unwrap();
        let path = store.path_for(&workspace);
        let name = path.file_name().unwrap().to_string_lossy();
        assert!(name.ends_with(".ws"));
        assert!(!name.contains(".."));
        assert!(!name.contains('/'));
        assert_eq!(path.parent().unwrap(), root.as_path());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn corrupted_state_file_is_rejected() {
        let root = temp_root("corrupt");
        let store = FileSyncStore::new(&root);
        let workspace = WorkspaceId::new("ws").unwrap();
        store.save(&workspace, &WorkspaceState::default()).unwrap();
        let path = store.path_for(&workspace);
        let mut bytes = fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0x01;
        fs::write(&path, &bytes).unwrap();
        assert!(matches!(store.load(&workspace), Err(StoreError::BadCrc)));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn save_replaces_previous_state_atomically() {
        let root = temp_root("replace");
        let store = FileSyncStore::new(&root);
        let workspace = WorkspaceId::new("ws").unwrap();
        // 保存两次验证整文件替换：第二次的游标与会话号完整覆盖第一次。
        let first = WorkspaceState {
            cursor: SyncCursor::new(1),
            ..WorkspaceState::default()
        };
        store.save(&workspace, &first).unwrap();
        let mut second = first.clone();
        second.cursor = SyncCursor::new(9);
        second.session_counter = 7;
        store.save(&workspace, &second).unwrap();
        assert_eq!(store.load(&workspace).unwrap(), second);
        assert!(!store.path_for(&workspace).with_extension("tmp").exists());
        let _ = fs::remove_dir_all(&root);
    }
}
