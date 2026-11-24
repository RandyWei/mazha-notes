//! 名册同步元数据纯模型（Phase 5 S1，D95/D98）。
//!
//! 本模块只定义内存类型与纯函数合并规则：操作 ID 全序、删除墓碑、可恢复冲突副本与
//! 名册元数据快照。不读系统时间、不做序列化/IO/GUI、不新增依赖；内容仍由 [`crate::CrdtDoc`]
//! 承载标准 Yjs v1 帧。线上信封、批次游标与传输留 S2（见 `docs/SYNC-SPEC.md` §4.5）。
//!
//! 参见 `SPEC.md` §6.5、§14 D95/D98。

use std::collections::BTreeMap;

use yrs::Update;
use yrs::updates::decoder::Decode;

use crate::error::CrdtError;
use crate::roster::validate_id;
use crate::{CrdtResult, DocId};

/// 同步操作 ID：逻辑时钟 + 设备标识，构成跨设备收敛用的全序（D98）。
///
/// 字段顺序即比较顺序：先比 `clock`，时钟相同再比 `device` 的字节序。派生 [`Ord`]
/// 依赖这一顺序，不要调整字段声明次序。逻辑时钟由调用方单调推进，本类型不读墙钟。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OpId {
    /// 逻辑时钟：调用方保证单调，较大者代表较新的操作。
    clock: u64,
    /// 设备标识：时钟相同时的确定性 tie-break，同时标明操作来源。
    device: String,
}

impl OpId {
    /// 构造操作 ID。
    ///
    /// # Errors
    /// 设备标识为空或含控制字符时返回 [`CrdtError::InvalidOpId`]，不 panic。
    pub fn new(clock: u64, device: impl Into<String>) -> CrdtResult<Self> {
        let device = device.into();
        // 空设备无法做 tie-break，控制字符会污染日志与未来的同步信封边界；
        // 与 DocId 采用同一套校验，保证元数据字段都可用作稳定键。
        if device.is_empty() || device.chars().any(char::is_control) {
            return Err(CrdtError::InvalidOpId(device));
        }
        Ok(Self { clock, device })
    }

    /// 逻辑时钟。
    #[must_use]
    pub fn clock(&self) -> u64 {
        self.clock
    }

    /// 设备标识。
    #[must_use]
    pub fn device(&self) -> &str {
        &self.device
    }
}

/// 墓碑原因：仅用于诊断与 UI 文案，不参与收敛判断（D95 重命名按删旧建新）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum TombstoneReason {
    /// 普通删除。
    #[default]
    Delete,
    /// 重命名来源（MVP 建模为旧路径删除 + 新路径创建）。
    RenameSource,
    /// 被更高优先级的同路径内容替换。
    Replace,
}

/// 删除墓碑：标记某 `DocId` 已删除，并保存被墓碑取代的可恢复副本（D95/D98）。
///
/// `conflict_frames` 中每项都是自包含的 Yjs v1 全量帧（`CrdtDoc::encode_update()`）：
/// 来源可以是本机删除时被移除的文档、墓碑 wins 时被忽略的离线编辑，或被拒绝的低/等
/// `op_id` upsert。任取一帧应用到空 [`crate::CrdtDoc`] 即可还原当时文本，从而满足
/// 「禁止静默丢数据」。帧按字节排序去重，保证多端归并顺序无关。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tombstone {
    /// 被删除的文档 ID（工作区相对路径）。
    doc_id: DocId,
    /// 决定胜负的操作 ID。
    op_id: OpId,
    /// 诊断用原因，不参与比较。
    reason: TombstoneReason,
    /// 可恢复的冲突副本帧（排序去重后的 Yjs v1 全量帧）。
    conflict_frames: Vec<Vec<u8>>,
}

impl Tombstone {
    /// 创建一条还没有冲突副本的墓碑。
    ///
    /// # Errors
    /// `doc_id` 非法时返回 [`CrdtError::InvalidDocumentId`]。
    pub fn new(doc_id: &str, op_id: OpId) -> CrdtResult<Self> {
        validate_id(doc_id)?;
        Ok(Self {
            doc_id: doc_id.to_owned(),
            op_id,
            reason: TombstoneReason::Delete,
            conflict_frames: Vec::new(),
        })
    }

    /// 设置诊断原因（不改变收敛语义）。
    #[must_use]
    pub fn with_reason(mut self, reason: TombstoneReason) -> Self {
        self.reason = reason;
        self
    }

    /// 追加一份可恢复副本帧。
    ///
    /// # Errors
    /// 帧不是合法 Yjs v1 帧时返回 [`CrdtError::InvalidConflictFrame`]，self 不变。
    pub fn with_conflict_frame(mut self, frame: Vec<u8>) -> CrdtResult<Self> {
        validate_conflict_frame(&self.doc_id, &frame)?;
        self.push_conflict_frame(frame);
        Ok(self)
    }

    /// 文档 ID。
    #[must_use]
    pub fn doc_id(&self) -> &str {
        &self.doc_id
    }

    /// 操作 ID。
    #[must_use]
    pub fn op_id(&self) -> &OpId {
        &self.op_id
    }

    /// 诊断原因。
    #[must_use]
    pub fn reason(&self) -> TombstoneReason {
        self.reason
    }

    /// 可恢复副本帧列表（按字节升序且无重复）。
    #[must_use]
    pub fn conflict_frames(&self) -> &[Vec<u8>] {
        &self.conflict_frames
    }

    /// 校验墓碑全部字段：ID、OpId 与每份冲突副本帧。
    ///
    /// # Errors
    /// 任一字段非法返回对应错误；本方法只读，不改变 self。
    pub fn validate(&self) -> CrdtResult<()> {
        validate_id(&self.doc_id)?;
        for frame in &self.conflict_frames {
            validate_conflict_frame(&self.doc_id, frame)?;
        }
        Ok(())
    }

    /// 归并另一条同文档墓碑：高 `op_id` 胜出，副本帧始终取并集。
    ///
    /// 即使对方 `op_id` 更低也不丢弃其副本帧——那可能是另一台设备离线编辑留下的
    /// 唯一可恢复记录；这正是 D95「禁止静默丢数据」的要求。
    ///
    /// # Errors
    /// 墓碑自身非法返回对应错误；`doc_id` 不一致返回 [`CrdtError::TombstoneConflict`]。
    pub fn merge_from(&mut self, other: &Tombstone) -> CrdtResult<()> {
        self.validate()?;
        other.validate()?;
        if self.doc_id != other.doc_id {
            return Err(CrdtError::TombstoneConflict(format!(
                "{} != {}",
                self.doc_id, other.doc_id
            )));
        }
        if other.op_id > self.op_id {
            self.op_id = other.op_id.clone();
            self.reason = other.reason;
        }
        self.extend_conflict_frames(other.conflict_frames.iter().cloned());
        Ok(())
    }

    /// 追加一份已校验过的副本帧（内部使用，不重复解码）。
    pub(crate) fn push_conflict_frame(&mut self, frame: Vec<u8>) {
        self.extend_conflict_frames(std::iter::once(frame));
    }

    /// 归并副本帧并排序去重：排序让不同到达顺序得到相同结果。
    fn extend_conflict_frames(&mut self, frames: impl IntoIterator<Item = Vec<u8>>) {
        self.conflict_frames.extend(frames);
        self.conflict_frames.sort();
        self.conflict_frames.dedup();
    }
}

/// 名册元数据：只保存墓碑表，确定性枚举（D98）。
///
/// 存活文档仍由 `CrdtRoster` 的 [`crate::CrdtDoc`] 承载；批次号、游标与服务端确认留 S2，
/// 这里不放占位字段以免过度设计。`BTreeMap` 保证快照与枚举顺序稳定。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RosterMeta {
    /// 文档 ID → 墓碑。
    tombstones: BTreeMap<DocId, Tombstone>,
}

impl RosterMeta {
    /// 新建空元数据。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 只读墓碑表（按键升序）。
    #[must_use]
    pub fn tombstones(&self) -> &BTreeMap<DocId, Tombstone> {
        &self.tombstones
    }

    /// 查询某文档的墓碑；非法 ID 等价于不存在。
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Tombstone> {
        self.tombstones.get(id)
    }

    /// 文档是否已墓碑化；非法 ID 返回 false。
    #[must_use]
    pub fn is_tombstoned(&self, id: &str) -> bool {
        validate_id(id).is_ok() && self.tombstones.contains_key(id)
    }

    /// 墓碑数量。
    #[must_use]
    pub fn len(&self) -> usize {
        self.tombstones.len()
    }

    /// 元数据是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tombstones.is_empty()
    }

    /// 合并一条墓碑：高 `op_id` 胜出，低/等不覆盖但归并冲突副本。
    ///
    /// # Errors
    /// 墓碑字段非法返回对应错误；同键但 `doc_id` 不一致返回
    /// [`CrdtError::TombstoneConflict`]。失败时元数据不变。
    pub fn merge_tombstone(&mut self, tombstone: Tombstone) -> CrdtResult<()> {
        // 先校验再写入：坏元数据不得在表里留下半条记录。
        tombstone.validate()?;
        if let Some(existing) = self.tombstones.get_mut(tombstone.doc_id()) {
            existing.merge_from(&tombstone)
        } else {
            self.tombstones
                .insert(tombstone.doc_id().to_owned(), tombstone);
            Ok(())
        }
    }

    /// 合并另一份元数据快照（纯函数式归并，不触碰活动文档）。
    ///
    /// # Errors
    /// 任一墓碑非法返回对应错误；采用逐条校验后写入，失败时可能已合并前面的墓碑，
    /// 调用方若要整批原子性应使用 `CrdtRoster::merge_meta`（内部在暂存副本上操作）。
    pub fn merge_from(&mut self, other: &RosterMeta) -> CrdtResult<()> {
        for tombstone in other.tombstones.values() {
            self.merge_tombstone(tombstone.clone())?;
        }
        Ok(())
    }

    /// 给已存在的墓碑追加一份已校验副本帧（内部使用）。
    pub(crate) fn add_conflict_frame(&mut self, id: &str, frame: Vec<u8>) -> CrdtResult<()> {
        validate_conflict_frame(id, &frame)?;
        let tombstone = self
            .tombstones
            .get_mut(id)
            .ok_or_else(|| CrdtError::DocumentNotFound(id.to_owned()))?;
        tombstone.push_conflict_frame(frame);
        Ok(())
    }

    /// 复活时移除墓碑（内部使用）。
    pub(crate) fn remove(&mut self, id: &str) {
        self.tombstones.remove(id);
    }
}

/// 元数据变更：一条删除墓碑，或一次同路径 upsert（D98）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetaChange {
    /// 删除/改名源/替换：`op_id` 高者胜出，胜出墓碑 wins。
    Tombstone(Tombstone),
    /// 同路径重建：`frame` 必须是可独立应用的 Yjs v1 全量帧，严格更高 `op_id` 才复活。
    Upsert {
        /// 文档 ID（工作区相对路径）。
        doc_id: DocId,
        /// 本次 upsert 的操作 ID。
        op_id: OpId,
        /// 自包含 Yjs v1 全量更新帧（通常由 `CrdtDoc::encode_update()` 产出）。
        frame: Vec<u8>,
    },
}

impl MetaChange {
    /// 变更对应的文档 ID。
    #[must_use]
    pub fn doc_id(&self) -> &str {
        match self {
            Self::Tombstone(tombstone) => tombstone.doc_id(),
            Self::Upsert { doc_id, .. } => doc_id,
        }
    }

    /// 变更对应的操作 ID。
    #[must_use]
    pub fn op_id(&self) -> &OpId {
        match self {
            Self::Tombstone(tombstone) => tombstone.op_id(),
            Self::Upsert { op_id, .. } => op_id,
        }
    }

    /// 校验变更的 ID 与帧：坏帧整批拒绝的前提。
    ///
    /// # Errors
    /// ID 非法返回 [`CrdtError::InvalidDocumentId`]；`Upsert.frame` 解码失败返回
    /// [`CrdtError::InvalidMetaFrame`]；墓碑字段非法返回对应错误。
    pub fn validate(&self) -> CrdtResult<()> {
        match self {
            Self::Tombstone(tombstone) => tombstone.validate(),
            Self::Upsert { doc_id, frame, .. } => {
                validate_id(doc_id)?;
                validate_upsert_frame(doc_id, frame)
            }
        }
    }

    /// 批次排序键：`op_id` 升序；同 `op_id` 时 `Upsert` 排在 `Tombstone` 前，
    /// 这样墓碑后应用并胜出（D95 默认墓碑 wins）；其余字段只为保证确定性。
    pub(crate) fn sort_key(&self) -> (OpId, u8, DocId, Vec<Vec<u8>>) {
        match self {
            Self::Tombstone(tombstone) => (
                tombstone.op_id().clone(),
                1,
                tombstone.doc_id().to_owned(),
                tombstone.conflict_frames().to_vec(),
            ),
            Self::Upsert {
                doc_id,
                op_id,
                frame,
            } => (op_id.clone(), 0, doc_id.clone(), vec![frame.clone()]),
        }
    }
}

/// 校验一份冲突副本帧可解码为 Yjs v1 更新。
fn validate_conflict_frame(doc_id: &str, frame: &[u8]) -> CrdtResult<()> {
    Update::decode_v1(frame).map_err(|source| CrdtError::InvalidConflictFrame {
        doc_id: doc_id.to_owned(),
        source,
    })?;
    Ok(())
}

/// 校验一份 upsert 内容帧可解码为 Yjs v1 更新。
fn validate_upsert_frame(doc_id: &str, frame: &[u8]) -> CrdtResult<()> {
    Update::decode_v1(frame).map_err(|source| CrdtError::InvalidMetaFrame {
        doc_id: doc_id.to_owned(),
        source,
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CrdtDoc;

    /// 测试用合法帧：空文档的全量更新。
    fn empty_frame() -> Vec<u8> {
        CrdtDoc::new().encode_update()
    }

    /// 测试用合法帧：带文本的全量更新。
    fn text_frame(text: &str) -> Vec<u8> {
        CrdtDoc::from_text(text).encode_update()
    }

    #[test]
    fn op_id_order_is_clock_then_device() {
        let a1 = OpId::new(1, "a").unwrap();
        let b1 = OpId::new(1, "b").unwrap();
        let a2 = OpId::new(2, "a").unwrap();
        // 时钟优先；同一时钟按设备字节序 tie-break。
        assert!(a1 < b1);
        assert!(b1 < a2);
        assert_eq!(a1, OpId::new(1, "a").unwrap());
        assert_eq!(a1.clock(), 1);
        assert_eq!(a1.device(), "a");
    }

    #[test]
    fn op_id_rejects_invalid_device_without_panic() {
        for bad in ["", "a\nb", "a\u{0000}b", "\u{001f}", "设备\u{0085}"] {
            assert!(matches!(OpId::new(1, bad), Err(CrdtError::InvalidOpId(_))));
        }
    }

    #[test]
    fn tombstone_merge_keeps_higher_op_id_and_unions_frames() {
        let low = Tombstone::new("a.md", OpId::new(1, "dev-a").unwrap())
            .unwrap()
            .with_conflict_frame(text_frame("离线编辑 A"))
            .unwrap();
        let high = Tombstone::new("a.md", OpId::new(2, "dev-b").unwrap())
            .unwrap()
            .with_reason(TombstoneReason::RenameSource)
            .with_conflict_frame(text_frame("离线编辑 B"))
            .unwrap();
        let mut merged = low.clone();
        merged.merge_from(&high).unwrap();
        assert_eq!(merged.op_id(), high.op_id());
        assert_eq!(merged.reason(), TombstoneReason::RenameSource);
        assert_eq!(merged.conflict_frames().len(), 2);
        // 排序去重：重复归并不增加帧数，结果字节一致。
        merged.merge_from(&low).unwrap();
        merged.merge_from(&high).unwrap();
        assert_eq!(merged.conflict_frames().len(), 2);
        let mut frames: Vec<Vec<u8>> = merged.conflict_frames().to_vec();
        frames.sort();
        assert_eq!(merged.conflict_frames(), frames);
        // 逆序归并也收敛到相同墓碑。
        let mut reversed = high;
        reversed.merge_from(&low).unwrap();
        assert_eq!(reversed, merged);
    }

    #[test]
    fn tombstone_merge_rejects_mismatched_doc_id() {
        let mut a = Tombstone::new("a.md", OpId::new(1, "dev").unwrap()).unwrap();
        let b = Tombstone::new("b.md", OpId::new(2, "dev").unwrap()).unwrap();
        assert!(matches!(
            a.merge_from(&b),
            Err(CrdtError::TombstoneConflict(_))
        ));
    }

    #[test]
    fn roster_meta_merge_is_deterministic_with_cjk_ids() {
        let mut left = RosterMeta::new();
        let mut right = RosterMeta::new();
        left.merge_tombstone(Tombstone::new("笔记/甲.md", OpId::new(5, "dev-a").unwrap()).unwrap())
            .unwrap();
        right
            .merge_tombstone(Tombstone::new("笔记/甲.md", OpId::new(7, "dev-b").unwrap()).unwrap())
            .unwrap();
        right
            .merge_tombstone(Tombstone::new("notes/a.md", OpId::new(1, "dev-a").unwrap()).unwrap())
            .unwrap();

        let mut a = left.clone();
        a.merge_from(&right).unwrap();
        let mut b = right.clone();
        b.merge_from(&left).unwrap();
        assert_eq!(a, b);
        assert_eq!(
            a.tombstones()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["notes/a.md", "笔记/甲.md"]
        );
        assert_eq!(
            a.get("笔记/甲.md").unwrap().op_id(),
            &OpId::new(7, "dev-b").unwrap()
        );
        assert!(a.is_tombstoned("笔记/甲.md"));
        assert!(!a.is_tombstoned("bad\nid"));
        assert_eq!(a.len(), 2);
        assert!(!a.is_empty());
        // 快照是克隆，外部修改不影响内部状态。
        let mut snapshot = a.clone();
        snapshot.remove("notes/a.md");
        assert!(a.is_tombstoned("notes/a.md"));
    }

    #[test]
    fn tombstone_rejects_bad_conflict_frame() {
        let good = Tombstone::new("a.md", OpId::new(1, "dev").unwrap()).unwrap();
        assert!(matches!(
            good.clone().with_conflict_frame(vec![0xff, 0xff, 0xff]),
            Err(CrdtError::InvalidConflictFrame { .. })
        ));
        let mut meta = RosterMeta::new();
        meta.merge_tombstone(good).unwrap();
        assert!(matches!(
            meta.add_conflict_frame("a.md", vec![0xff]),
            Err(CrdtError::InvalidConflictFrame { .. })
        ));
        assert!(meta.get("a.md").unwrap().conflict_frames().is_empty());
    }

    #[test]
    fn meta_change_validate_and_sort_key_put_tombstone_after_equal_upsert() {
        let tombstone = Tombstone::new("a.md", OpId::new(1, "dev").unwrap()).unwrap();
        let tomb_change = MetaChange::Tombstone(tombstone);
        let upsert = MetaChange::Upsert {
            doc_id: "a.md".to_owned(),
            op_id: OpId::new(1, "dev").unwrap(),
            frame: empty_frame(),
        };
        assert!(tomb_change.validate().is_ok());
        assert!(upsert.validate().is_ok());
        assert!(upsert.sort_key() < tomb_change.sort_key());
        assert_eq!(tomb_change.doc_id(), "a.md");
        assert_eq!(upsert.op_id().clock(), 1);

        let bad = MetaChange::Upsert {
            doc_id: "a.md".to_owned(),
            op_id: OpId::new(2, "dev").unwrap(),
            frame: vec![0xff, 0xff, 0xff],
        };
        assert!(matches!(
            bad.validate(),
            Err(CrdtError::InvalidMetaFrame { .. })
        ));
        let bad_id = MetaChange::Upsert {
            doc_id: "bad\nid".to_owned(),
            op_id: OpId::new(2, "dev").unwrap(),
            frame: empty_frame(),
        };
        assert!(matches!(
            bad_id.validate(),
            Err(CrdtError::InvalidDocumentId(_))
        ));
    }
}
