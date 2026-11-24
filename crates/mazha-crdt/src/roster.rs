//! 多文档名册（`CrdtRoster`，D94）。
//!
//! 以稳定的字符串 ID（约定为工作区相对路径，如 `notes/a.md`）管理多篇 [`CrdtDoc`]，
//! 内部用 [`BTreeMap`] 保证枚举与整册同步结果确定。整册同步仅用
//! 「ID → 标准 Yjs v1 帧」的映射表达，不自造线协议/信封，不做序列化、传输与持久化。
//!
//! # 限制与兼容边界
//! - D94 的 `remove` / `state_vectors` / `encode_diff` / `apply_updates` 语义保持不变：
//!   只处理内容帧、不查墓碑；旧 `apply_updates` 仍可能按 D94 语义重建已墓碑化的文档。
//! - S1 新增的墓碑传播、冲突副本与复活规则只由 `*_with_meta` / `apply_meta` 等新 API
//!   承载（D95/D98），同步路径必须走新 API。
//! - 结构性并发冲突策略（块级锁定/提示）同样留待同步 SPEC `[待确认]`。
//!
//! 参见 `SPEC.md` §6.5 与 §14 D94/D95/D98。

use std::collections::{BTreeMap, BTreeSet};

use yrs::Update;
use yrs::updates::decoder::Decode;

use crate::{
    CrdtDoc, CrdtError, CrdtResult, DeviceAcks, MetaChange, OpId, RosterMeta, Tombstone,
    TombstoneGcReport,
};

/// 名册键的类型别名（等价于 [`String`]）。
///
/// 由上层稳定分配，约定为工作区相对路径（如 `notes/a.md`）；必须非空且不含控制字符。
pub type DocId = String;

/// 校验文档 ID：必须非空且不含控制字符（S1 元数据模块复用同一规则）。
pub(crate) fn validate_id(id: &str) -> CrdtResult<()> {
    // 控制字符（C0/C1 与 DEL）会污染日志、文件名和未来的同步信封边界；空串无法作为稳定键，
    // 两种都拒绝，从而守住"名册内所有键都合法"的不变量。
    if id.is_empty() || id.chars().any(char::is_control) {
        Err(CrdtError::InvalidDocumentId(id.to_owned()))
    } else {
        Ok(())
    }
}

/// 工作区级多文档名册：按稳定 ID 管理多篇 [`CrdtDoc`]，并提供整册增量同步映射。
///
/// 内部使用 [`BTreeMap`]，因此 `ids`、`state_vectors`、`encode_diff` 的结果按键升序确定。
/// S1 起同时持有 [`RosterMeta`] 墓碑表；内容帧与元数据严格分离（D94/D95/D98）。
/// 本类型是纯 Rust 容器：无 GUI、无 IO、无新依赖，可无 UI 单测。
#[derive(Debug, Default)]
pub struct CrdtRoster {
    /// ID → 文档；有序映射保证枚举与同步映射的确定性。
    docs: BTreeMap<DocId, CrdtDoc>,
    /// 名册元数据：只保存墓碑，供 S1 删除传播/复活规则使用。
    meta: RosterMeta,
}

impl CrdtRoster {
    /// 新建空名册。
    pub fn new() -> Self {
        Self::default()
    }

    /// 校验一篇待创建文档的 ID：格式合法且尚未被占用。
    fn check_new_id(&self, id: &str) -> CrdtResult<()> {
        validate_id(id)?;
        if self.docs.contains_key(id) {
            return Err(CrdtError::DocumentAlreadyExists(id.to_owned()));
        }
        Ok(())
    }

    /// 在名册中新建一篇空文档。
    ///
    /// # Errors
    /// ID 非法返回 [`CrdtError::InvalidDocumentId`]；ID 已存在返回
    /// [`CrdtError::DocumentAlreadyExists`]，且不会覆盖原文档的 CRDT 历史。
    pub fn create(&mut self, id: &str) -> CrdtResult<()> {
        self.check_new_id(id)?;
        self.docs.insert(id.to_owned(), CrdtDoc::new());
        Ok(())
    }

    /// 以初始源文本新建一篇文档。
    ///
    /// # Errors
    /// ID 非法返回 [`CrdtError::InvalidDocumentId`]；ID 已存在返回
    /// [`CrdtError::DocumentAlreadyExists`]，且原有文本与历史不变。
    pub fn create_from_text(&mut self, id: &str, text: &str) -> CrdtResult<()> {
        self.check_new_id(id)?;
        self.docs.insert(id.to_owned(), CrdtDoc::from_text(text));
        Ok(())
    }

    /// 按 ID 取可变文档；不存在时新建空文档（幂等）。
    ///
    /// 已存在的文档不会被重置，其文本与 CRDT 历史保持不变。
    ///
    /// # Errors
    /// ID 非法返回 [`CrdtError::InvalidDocumentId`]；与 [`CrdtRoster::create`] 不同，
    /// 本方法不会因为 ID 已存在而报错（幂等）。
    pub fn open_or_create(&mut self, id: &str) -> CrdtResult<&mut CrdtDoc> {
        validate_id(id)?;
        // entry 保证幂等：命中时直接返回原文档，绝不覆盖已有历史。
        Ok(self.docs.entry(id.to_owned()).or_default())
    }

    /// 按 ID 只读访问文档。
    ///
    /// # Errors
    /// ID 非法返回 [`CrdtError::InvalidDocumentId`]；不存在返回 [`CrdtError::DocumentNotFound`]。
    pub fn get(&self, id: &str) -> CrdtResult<&CrdtDoc> {
        validate_id(id)?;
        self.docs
            .get(id)
            .ok_or_else(|| CrdtError::DocumentNotFound(id.to_owned()))
    }

    /// 按 ID 可变访问文档。
    ///
    /// # Errors
    /// ID 非法返回 [`CrdtError::InvalidDocumentId`]；不存在返回 [`CrdtError::DocumentNotFound`]。
    pub fn get_mut(&mut self, id: &str) -> CrdtResult<&mut CrdtDoc> {
        validate_id(id)?;
        self.docs
            .get_mut(id)
            .ok_or_else(|| CrdtError::DocumentNotFound(id.to_owned()))
    }

    /// 移除并返回一篇文档；该删除只在本机生效，不同步墓碑。
    ///
    /// 远端仍持旧文档时，重新全量同步可能"复活"已删文档（见模块级说明）。
    ///
    /// # Errors
    /// ID 非法返回 [`CrdtError::InvalidDocumentId`]；不存在返回 [`CrdtError::DocumentNotFound`]。
    pub fn remove(&mut self, id: &str) -> CrdtResult<CrdtDoc> {
        validate_id(id)?;
        self.docs
            .remove(id)
            .ok_or_else(|| CrdtError::DocumentNotFound(id.to_owned()))
    }

    /// 名册是否含有该 ID 的文档；非法 ID 视为不存在（此签名无错误通道）。
    pub fn contains(&self, id: &str) -> bool {
        validate_id(id).is_ok() && self.docs.contains_key(id)
    }

    /// 返回全部文档 ID 的升序列表。
    pub fn ids(&self) -> Vec<DocId> {
        // BTreeMap 的键序即字典序，直接收集即可得到确定的升序枚举。
        self.docs.keys().cloned().collect()
    }

    /// 返回文档数量。
    pub fn len(&self) -> usize {
        self.docs.len()
    }

    /// 名册是否为空。
    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    /// 整册快照：逐文档调用 [`CrdtDoc::snapshot`]，ID 与顺序不变、CRDT 历史清零。
    ///
    /// 快照后与旧状态向量/更新帧不再兼容，远端需重新全量同步（见 [`CrdtDoc::snapshot`]）。
    /// S1 起元数据（墓碑与冲突副本）原样保留：快照只压缩内容帧，不丢弃删除记录。
    #[must_use]
    pub fn snapshot(&self) -> Self {
        // 逐文档快照使更新帧体积收敛到 O(文本长度)，ID 与键序由 BTreeMap 原样保留。
        Self {
            docs: self
                .docs
                .iter()
                .map(|(id, doc)| (id.clone(), doc.snapshot()))
                .collect(),
            // 墓碑是收敛真相，快照不得清理（MVP 不做 GC，D95）。
            meta: self.meta.clone(),
        }
    }

    /// 返回 ID → 该文档状态向量（标准 Yjs v1）的有序映射。
    ///
    /// 结果可直接交给对端的 [`CrdtRoster::encode_diff`]，键升序。
    pub fn state_vectors(&self) -> BTreeMap<DocId, Vec<u8>> {
        self.docs
            .iter()
            .map(|(id, doc)| (id.clone(), doc.state_vector()))
            .collect()
    }

    /// 计算整册相对远端状态向量的增量帧：ID → 标准 Yjs v1 更新帧。
    ///
    /// 仅输出本地存在、且相对远端状态向量有实际变化（新块或删除集）的文档；远端缺失某文档 ID
    /// 时输出其全量更新。结果按键升序。注意 yrs 的增量帧总是附带整份删除集，而状态向量不含
    /// 删除信息，故删除发生后即使远端已同步仍可能输出该文档的帧（重复应用幂等）。
    ///
    /// # Errors
    /// 远端任一状态向量不是合法的 Yjs v1 帧时返回 [`CrdtError::Decode`]。
    pub fn encode_diff(
        &self,
        remote: &BTreeMap<DocId, Vec<u8>>,
    ) -> CrdtResult<BTreeMap<DocId, Vec<u8>>> {
        let mut out = BTreeMap::new();
        for (id, doc) in &self.docs {
            let Some(remote_bytes) = remote.get(id) else {
                // 远端没有该文档的 SV：按空状态向量输出全量帧（初次同步）。
                out.insert(id.clone(), doc.encode_update());
                continue;
            };
            // encode_diff 内部先解码远端 SV：坏帧在此报错，即使该文档最终没有变化。
            let frame = doc.encode_diff(remote_bytes)?;
            // 仅靠状态向量时钟无法发现删除集变化（yrs 删除不推进客户端时钟），
            // 因此解码实际增量帧判断是否为空，避免漏掉"仅删除"的同步。
            if Update::decode_v1(&frame)?.is_empty() {
                // 增量帧为空：不输出，保证映射只含真正变化的文档。
                continue;
            }
            out.insert(id.clone(), frame);
        }
        Ok(out)
    }

    /// 批量应用整册更新帧，返回实际应用的文档 ID（升序）。
    ///
    /// 未知 ID 会先建空文档再应用帧。任一帧解码或应用失败时**整批拒绝**，
    /// 名册内所有文档（包括已成功应用的帧）保持原状，调用方无需自行回滚。
    ///
    /// # Errors
    /// ID 非法返回 [`CrdtError::InvalidDocumentId`]；帧解码失败返回 [`CrdtError::Decode`]；
    /// 应用失败返回 [`CrdtError::Apply`]。任何错误都不改变名册状态。
    pub fn apply_updates(&mut self, updates: &BTreeMap<DocId, Vec<u8>>) -> CrdtResult<Vec<DocId>> {
        // 第一遍先校验全部键：未知 ID 允许，非法 ID 整批拒绝，避免只应用了一半。
        for id in updates.keys() {
            validate_id(id)?;
        }
        // 两阶段提交：fork 保留每篇文档的 CRDT 历史，临时副本与真文档同步语义一致；
        // 直接在 self 上应用遇到坏帧无法回滚。
        let mut staged: BTreeMap<DocId, CrdtDoc> = self
            .docs
            .iter()
            .map(|(id, doc)| (id.clone(), doc.fork()))
            .collect();
        for (id, bytes) in updates {
            // 未知 ID 建空文档：文档身份由名册键给出，不依赖帧内元数据。
            staged.entry(id.clone()).or_default().apply_update(bytes)?;
        }
        // 全部成功才一次性替换：坏帧路径上 self 从未被触碰。
        self.docs = staged;
        // BTreeMap 键序即升序，直接收集得到受影响的 ID 列表。
        Ok(updates.keys().cloned().collect())
    }

    // ---------- S1/D95/D98：墓碑元数据与内容帧分离的新 API ----------

    /// 墓碑化删除：移除活动文档并把删除记录（含可恢复内容帧）写入元数据。
    ///
    /// 与 D94 的 [`CrdtRoster::remove`] 不同：这里会留下墓碑供跨设备传播；旧 `remove`
    /// 的本机删除语义不变。若同名已有更高 `op_id` 墓碑，则保留其 `op_id`/原因，仅归并
    /// 本次删除保存的内容帧；返回值始终是名册中**实际生效**的墓碑。
    ///
    /// # Errors
    /// ID 非法返回 [`CrdtError::InvalidDocumentId`]；文档不存在返回
    /// [`CrdtError::DocumentNotFound`]；元数据归并失败返回对应错误，名册不变。
    pub fn remove_with_op(&mut self, id: &str, op_id: OpId) -> CrdtResult<Tombstone> {
        validate_id(id)?;
        // 先取内容帧并构造墓碑，全部校验通过后才移除文档，避免半途失败留下悬空删除。
        let frame = self.get(id)?.encode_update();
        let tombstone = Tombstone::new(id, op_id)?.with_conflict_frame(frame)?;
        let mut staged_meta = self.meta.clone();
        staged_meta.merge_tombstone(tombstone)?;
        let effective = staged_meta
            .get(id)
            .cloned()
            .ok_or_else(|| CrdtError::DocumentNotFound(id.to_owned()))?;
        self.docs.remove(id);
        self.meta = staged_meta;
        Ok(effective)
    }

    /// 文档是否已墓碑化；非法 ID 视为 false。
    #[must_use]
    pub fn is_tombstoned(&self, id: &str) -> bool {
        self.meta.is_tombstoned(id)
    }

    /// 只读墓碑表（按键升序）。
    #[must_use]
    pub fn tombstones(&self) -> &BTreeMap<DocId, Tombstone> {
        self.meta.tombstones()
    }

    /// 元数据快照（克隆）：调用方修改不影响名册内部状态。
    #[must_use]
    pub fn meta_snapshot(&self) -> RosterMeta {
        self.meta.clone()
    }

    /// 按设备确认收缩名册墓碑副本（S5b 第二批 D105，additive）。
    ///
    /// 只委托 [`RosterMeta::gc_tombstones`]；活动文档不受影响，墓碑本体保留以防旧 upsert
    /// 复活，转储出的副本由报告返回给调用方（服务端落审计归档）。
    ///
    /// # Errors
    /// 任一墓碑非法时返回对应错误，名册不变。
    pub fn gc_tombstones(&mut self, device_acks: &DeviceAcks) -> CrdtResult<TombstoneGcReport> {
        self.meta.gc_tombstones(device_acks)
    }

    /// 应用一组元数据变更（无内容帧），返回受影响文档 ID（升序）。
    ///
    /// 规则见 [`CrdtRoster::apply_updates_with_meta`]：高 `op_id` 胜出、同 `op_id` 墓碑胜出、
    /// 严格更高的 upsert 才复活、坏元数据整批拒绝零改动。
    ///
    /// # Errors
    /// 任一变更非法返回对应错误，名册不变。
    pub fn apply_meta(&mut self, changes: &[MetaChange]) -> CrdtResult<Vec<DocId>> {
        self.apply_updates_with_meta(&BTreeMap::new(), changes)
    }

    /// 合并另一份元数据快照：只传播墓碑，不复活文档（快照里没有 upsert 内容帧）。
    ///
    /// 在暂存副本上归并后再提交，因此任一条墓碑非法时整批零改动。
    ///
    /// # Errors
    /// 任一墓碑非法返回对应错误，名册不变。
    pub fn merge_meta(&mut self, other: &RosterMeta) -> CrdtResult<Vec<DocId>> {
        let changes: Vec<MetaChange> = other
            .tombstones()
            .values()
            .cloned()
            .map(MetaChange::Tombstone)
            .collect();
        self.apply_meta(&changes)
    }

    /// 整册内容增量 + 独立元数据快照：旧 [`CrdtRoster::encode_diff`] 不夹带墓碑。
    ///
    /// 返回值第一部分与 D94 `encode_diff` 完全一致（ID → 标准 Yjs v1 帧）；第二部分是
    /// 元数据快照，交给 S2 协议层放进显式信封，不混入 Y.Text。
    ///
    /// # Errors
    /// 远端状态向量坏帧时返回 [`CrdtError::Decode`]。
    pub fn encode_diff_with_meta(
        &self,
        remote: &BTreeMap<DocId, Vec<u8>>,
    ) -> CrdtResult<(BTreeMap<DocId, Vec<u8>>, RosterMeta)> {
        Ok((self.encode_diff(remote)?, self.meta.clone()))
    }

    /// 原子应用「内容帧 + 元数据变更」：墓碑优先，坏帧/坏元数据整批拒绝零改动。
    ///
    /// 处理顺序：
    /// 1. 校验所有 ID、OpId 与帧；按 `(op_id, 类型[Upsert<Tombstone], doc_id, frame)` 排序，
    ///    使批次结果与输入顺序无关，且相同 `op_id` 时墓碑胜出；
    /// 2. 墓碑化：从活动文档表移除目标，把其全量帧并入墓碑的 `conflict_frames`；更高
    ///    `op_id` 覆盖旧墓碑但归并双方副本帧；
    /// 3. upsert：严格更高 `op_id` 才删除墓碑并用全量帧建文档；低/等 `op_id` 不复活，
    ///    其帧作为可恢复副本并入墓碑；无墓碑时按 D94 语义应用内容帧；
    /// 4. 内容帧：已墓碑化文档不建活动文档，其帧作为副本保底（D95 禁止静默丢数据）；
    /// 5. 全部成功才一次性提交。
    ///
    /// 与 D94 旧 `apply_updates` 的区别：旧 API 不查墓碑、未知 ID 一律自动建文档，语义不变。
    ///
    /// # Errors
    /// ID/OpId 非法、冲突副本/upsert/内容帧解码失败或应用失败时返回对应错误，名册不变。
    pub fn apply_updates_with_meta(
        &mut self,
        updates: &BTreeMap<DocId, Vec<u8>>,
        changes: &[MetaChange],
    ) -> CrdtResult<Vec<DocId>> {
        // 第一遍只校验：坏帧/坏元数据必须在暂存之前被拒绝，错误信息也更精确。
        for id in updates.keys() {
            validate_id(id)?;
        }
        for change in changes {
            change.validate()?;
        }

        let mut sorted: Vec<MetaChange> = changes.to_vec();
        sorted.sort_by_key(MetaChange::sort_key);

        // 两阶段提交：在 fork/克隆的暂存副本上操作，任何错误路径都不触碰 self。
        let mut staged_docs: BTreeMap<DocId, CrdtDoc> = self
            .docs
            .iter()
            .map(|(id, doc)| (id.clone(), doc.fork()))
            .collect();
        let mut staged_meta = self.meta.clone();

        for change in &sorted {
            match change {
                MetaChange::Tombstone(tombstone) => {
                    // 墓碑 wins：先取走活动文档的全量帧作为可恢复副本，再交给元数据归并。
                    let mut incoming = tombstone.clone();
                    if let Some(doc) = staged_docs.remove(tombstone.doc_id()) {
                        incoming.push_conflict_frame(doc.encode_update());
                    }
                    staged_meta.merge_tombstone(incoming)?;
                }
                MetaChange::Upsert {
                    doc_id,
                    op_id,
                    frame,
                } => {
                    // 先克隆胜出墓碑的 op_id，避免 match 借用与后续可变借用冲突。
                    let existing = staged_meta.get(doc_id).map(|t| t.op_id().clone());
                    match existing {
                        Some(previous) if *op_id > previous => {
                            // 严格更高才复活：用全量帧重建文档，墓碑随删除意图一起消失。
                            staged_meta.remove(doc_id);
                            let mut doc = CrdtDoc::new();
                            doc.apply_update(frame)?;
                            staged_docs.insert(doc_id.clone(), doc);
                        }
                        Some(_) => {
                            // 低/等 op_id 不复活：帧并入副本而不是丢弃。
                            staged_meta.add_conflict_frame(doc_id, frame.clone())?;
                        }
                        None => {
                            // 无墓碑：沿用 D94 语义（未知 ID 自动建文档）。
                            staged_docs
                                .entry(doc_id.clone())
                                .or_default()
                                .apply_update(frame)?;
                        }
                    }
                }
            }
        }

        for (id, frame) in updates {
            if staged_meta.is_tombstoned(id) {
                // 墓碑 wins：离线编辑的内容帧只作为副本保留，绝不复活活动文档。
                staged_meta.add_conflict_frame(id, frame.clone())?;
            } else {
                staged_docs
                    .entry(id.clone())
                    .or_default()
                    .apply_update(frame)?;
            }
        }

        self.docs = staged_docs;
        self.meta = staged_meta;

        let mut affected: BTreeSet<DocId> = updates.keys().cloned().collect();
        affected.extend(changes.iter().map(|change| change.doc_id().to_owned()));
        Ok(affected.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TombstoneReason;

    #[test]
    fn invalid_ids_are_rejected_by_all_fallible_apis() {
        let mut roster = CrdtRoster::new();
        for bad in [
            "",
            "a\nb",
            "a\u{0000}b",
            "\u{001f}",
            "\u{007f}",
            "a\u{0085}b",
        ] {
            assert!(matches!(
                roster.create(bad),
                Err(CrdtError::InvalidDocumentId(_))
            ));
            assert!(matches!(
                roster.create_from_text(bad, "x"),
                Err(CrdtError::InvalidDocumentId(_))
            ));
            assert!(matches!(
                roster.get(bad),
                Err(CrdtError::InvalidDocumentId(_))
            ));
            assert!(matches!(
                roster.get_mut(bad),
                Err(CrdtError::InvalidDocumentId(_))
            ));
            assert!(matches!(
                roster.remove(bad),
                Err(CrdtError::InvalidDocumentId(_))
            ));
            // contains 无错误通道：非法 ID 永远不可能在名册内。
            assert!(!roster.contains(bad));
        }
        assert!(roster.is_empty());
    }

    #[test]
    fn open_or_create_rejects_invalid_id() {
        let mut roster = CrdtRoster::new();
        assert!(matches!(
            roster.open_or_create("bad\nid"),
            Err(CrdtError::InvalidDocumentId(_))
        ));
        assert!(roster.is_empty());
    }

    #[test]
    fn create_and_get_roundtrip() {
        let mut roster = CrdtRoster::new();
        assert!(roster.is_empty());
        roster.create("notes/a.md").unwrap();
        assert_eq!(roster.len(), 1);
        assert!(!roster.is_empty());
        assert!(roster.contains("notes/a.md"));
        assert_eq!(roster.get("notes/a.md").unwrap().text(), "");
    }

    #[test]
    fn create_from_text_rejects_duplicate_without_overwriting() {
        let mut roster = CrdtRoster::new();
        roster.create_from_text("a", "hello").unwrap();
        assert_eq!(roster.get("a").unwrap().text(), "hello");
        assert!(matches!(
            roster.create("a"),
            Err(CrdtError::DocumentAlreadyExists(_))
        ));
        assert!(matches!(
            roster.create_from_text("a", "other"),
            Err(CrdtError::DocumentAlreadyExists(_))
        ));
        assert_eq!(roster.len(), 1);
        assert_eq!(roster.get("a").unwrap().text(), "hello");
    }

    #[test]
    fn open_or_create_is_idempotent_and_keeps_history() {
        let mut roster = CrdtRoster::new();
        roster.open_or_create("a").unwrap().push("hello").unwrap();
        let sv_after_first_edit = roster.state_vectors()["a"].clone();
        roster.open_or_create("a").unwrap().push(" world").unwrap();
        assert_eq!(roster.get("a").unwrap().text(), "hello world");
        assert_eq!(roster.len(), 1);
        assert_ne!(roster.state_vectors()["a"], sv_after_first_edit);
        // 再次打开既不建新文档也不重置历史：状态向量保持不变。
        let sv = roster.state_vectors()["a"].clone();
        roster.open_or_create("a").unwrap();
        assert_eq!(roster.state_vectors()["a"], sv);
    }

    #[test]
    fn get_mut_edits_are_visible_through_get() {
        let mut roster = CrdtRoster::new();
        roster.create_from_text("a", "ab").unwrap();
        roster.get_mut("a").unwrap().insert(1, "X").unwrap();
        assert_eq!(roster.get("a").unwrap().text(), "aXb");
    }

    #[test]
    fn missing_document_errors() {
        let mut roster = CrdtRoster::new();
        roster.create("a").unwrap();
        assert!(matches!(
            roster.get("b"),
            Err(CrdtError::DocumentNotFound(_))
        ));
        assert!(matches!(
            roster.get_mut("b"),
            Err(CrdtError::DocumentNotFound(_))
        ));
        assert!(matches!(
            roster.remove("b"),
            Err(CrdtError::DocumentNotFound(_))
        ));
        assert!(!roster.contains("b"));
    }

    #[test]
    fn remove_returns_document_and_allows_recreate() {
        let mut roster = CrdtRoster::new();
        roster.create_from_text("a", "payload").unwrap();
        let removed = roster.remove("a").unwrap();
        assert_eq!(removed.text(), "payload");
        assert!(!roster.contains("a"));
        assert!(roster.is_empty());
        roster.create("a").unwrap();
        assert_eq!(roster.get("a").unwrap().text(), "");
    }

    #[test]
    fn ids_are_sorted_with_cjk_keys() {
        let mut roster = CrdtRoster::new();
        for id in ["笔记/乙.md", "notes/b.md", "笔记/甲.md", "notes/a.md"] {
            roster.create(id).unwrap();
        }
        // 升序按 UTF-8 字典序：ASCII 路径在前，CJK 路径在后且"乙"<"甲"。
        assert_eq!(
            roster.ids(),
            vec!["notes/a.md", "notes/b.md", "笔记/乙.md", "笔记/甲.md"]
        );
        assert!(roster.contains("笔记/甲.md"));
    }

    #[test]
    fn state_vectors_cover_all_docs_and_are_repeatable() {
        let mut roster = CrdtRoster::new();
        for id in ["a", "b", "笔记/文档.md"] {
            roster.create_from_text(id, "共同文本").unwrap();
        }
        let sv = roster.state_vectors();
        assert_eq!(sv.keys().cloned().collect::<Vec<_>>(), roster.ids());
        assert_eq!(sv, roster.state_vectors());
        assert!(sv.values().all(|bytes| !bytes.is_empty()));
    }

    #[test]
    fn encode_diff_only_returns_changed_docs() {
        let mut local = CrdtRoster::new();
        local.create_from_text("a", "alpha").unwrap();
        local.create_from_text("b", "beta").unwrap();
        let mut remote = CrdtRoster::new();
        let full = local.encode_diff(&BTreeMap::new()).unwrap();
        remote.apply_updates(&full).unwrap();
        assert_eq!(remote.ids(), local.ids());

        let remote_sv = remote.state_vectors();
        local.get_mut("a").unwrap().push("+").unwrap();
        let diff = local.encode_diff(&remote_sv).unwrap();
        assert_eq!(diff.keys().cloned().collect::<Vec<_>>(), vec!["a"]);
        remote.apply_updates(&diff).unwrap();
        assert_eq!(
            remote.get("a").unwrap().text(),
            local.get("a").unwrap().text()
        );
        assert_eq!(remote.get("b").unwrap().text(), "beta");
        // 同步完成后再算增量应为空映射。
        assert!(
            local
                .encode_diff(&remote.state_vectors())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn encode_diff_missing_remote_sv_sends_full_doc() {
        let mut local = CrdtRoster::new();
        local.create_from_text("a", "AAA").unwrap();
        local.create_from_text("b", "BBB").unwrap();
        let mut remote_sv = local.state_vectors();
        remote_sv.remove("b");
        let diff = local.encode_diff(&remote_sv).unwrap();
        assert_eq!(diff.keys().cloned().collect::<Vec<_>>(), vec!["b"]);
        let mut fresh = CrdtRoster::new();
        fresh.apply_updates(&diff).unwrap();
        assert_eq!(fresh.ids(), vec!["b"]);
        assert_eq!(fresh.get("b").unwrap().text(), "BBB");
    }

    #[test]
    fn encode_diff_rejects_bad_remote_state_vector() {
        let mut local = CrdtRoster::new();
        local.create_from_text("a", "x").unwrap();
        let mut remote = local.state_vectors();
        remote.insert("a".to_owned(), vec![0xff, 0xff, 0xff]);
        assert!(matches!(
            local.encode_diff(&remote),
            Err(CrdtError::Decode(_))
        ));
    }

    #[test]
    fn encode_diff_propagates_delete_only_changes() {
        // 删除只改删除集、不推进状态向量时钟：此时两端时钟相同，但远端仍持旧文本。
        let mut local = CrdtRoster::new();
        local.create_from_text("a", "abcdef").unwrap();
        let mut remote = CrdtRoster::new();
        remote
            .apply_updates(&local.encode_diff(&BTreeMap::new()).unwrap())
            .unwrap();
        assert_eq!(remote.get("a").unwrap().text(), "abcdef");

        // 本地删除前 3 个字符；yrs 的删除不产生新块，状态向量应保持不变。
        let sv_before = local.state_vectors();
        local.get_mut("a").unwrap().delete(0, 3).unwrap();
        assert_eq!(local.state_vectors(), sv_before, "删除不应推进状态向量时钟");
        assert_eq!(local.get("a").unwrap().text(), "def");

        // 尽管 SV 相同，删除集变化仍须产出非空增量帧并让远端收敛。
        let diff = local.encode_diff(&remote.state_vectors()).unwrap();
        assert!(diff.contains_key("a"));
        remote.apply_updates(&diff).unwrap();
        assert_eq!(remote.get("a").unwrap().text(), "def");

        // yrs 的 encode_state_as_update_v1 总是附带整份删除集，而状态向量不含删除信息，
        // 因此无法仅凭 SV 判断远端是否已收到删除：同步后再算仍会得到帧，但重复应用是幂等的。
        let again = local.encode_diff(&remote.state_vectors()).unwrap();
        assert!(again.contains_key("a"));
        remote.apply_updates(&again).unwrap();
        assert_eq!(remote.get("a").unwrap().text(), "def");
    }

    #[test]
    fn apply_updates_creates_unknown_docs_and_converges() {
        let mut source = CrdtRoster::new();
        for id in ["z.md", "笔记/a.md", "a.md"] {
            source
                .create_from_text(id, &format!("内容 of {id}"))
                .unwrap();
        }
        let mut target = CrdtRoster::new();
        let updates = source.encode_diff(&target.state_vectors()).unwrap();
        let affected = target.apply_updates(&updates).unwrap();
        assert_eq!(affected, vec!["a.md", "z.md", "笔记/a.md"]);
        assert_eq!(target.ids(), source.ids());
        for id in source.ids() {
            assert_eq!(
                target.get(&id).unwrap().text(),
                source.get(&id).unwrap().text()
            );
        }
    }

    #[test]
    fn apply_updates_incremental_roundtrip() {
        let mut a = CrdtRoster::new();
        a.create_from_text("a", "hello").unwrap();
        let mut b = CrdtRoster::new();
        b.apply_updates(&a.encode_diff(&BTreeMap::new()).unwrap())
            .unwrap();
        a.get_mut("a").unwrap().push(" world").unwrap();
        let affected = b
            .apply_updates(&a.encode_diff(&b.state_vectors()).unwrap())
            .unwrap();
        assert_eq!(affected, vec!["a"]);
        assert_eq!(b.get("a").unwrap().text(), "hello world");
        assert!(a.encode_diff(&b.state_vectors()).unwrap().is_empty());
    }

    #[test]
    fn bad_update_frame_rejects_whole_batch_without_mutation() {
        let mut roster = CrdtRoster::new();
        roster.create_from_text("a", "keep-a").unwrap();
        roster.create_from_text("b", "keep-b").unwrap();
        let before_sv = roster.state_vectors();

        // 合法帧若被应用会改变文档 a；坏帧放在 b 上，整批必须一起拒绝。
        let mut peer = CrdtDoc::from_text("keep-a");
        peer.push("!").unwrap();
        let good = peer
            .encode_diff(&roster.get("a").unwrap().state_vector())
            .unwrap();
        let mut updates = BTreeMap::new();
        updates.insert("a".to_owned(), good);
        updates.insert("b".to_owned(), vec![0xff, 0xff, 0xff]);
        assert!(roster.apply_updates(&updates).is_err());

        assert_eq!(roster.get("a").unwrap().text(), "keep-a");
        assert_eq!(roster.get("b").unwrap().text(), "keep-b");
        assert_eq!(roster.state_vectors(), before_sv);
        assert_eq!(roster.ids(), vec!["a", "b"]);
    }

    #[test]
    fn invalid_id_in_updates_rejects_whole_batch() {
        let mut roster = CrdtRoster::new();
        roster.create_from_text("a", "unchanged").unwrap();
        let before_sv = roster.state_vectors();
        let mut updates = BTreeMap::new();
        updates.insert(
            "a".to_owned(),
            CrdtDoc::from_text("changed").encode_update(),
        );
        updates.insert("bad\nid".to_owned(), vec![]);
        assert!(matches!(
            roster.apply_updates(&updates),
            Err(CrdtError::InvalidDocumentId(_))
        ));
        assert_eq!(roster.get("a").unwrap().text(), "unchanged");
        assert_eq!(roster.state_vectors(), before_sv);
    }

    #[test]
    fn roster_snapshot_keeps_texts_and_compacts_frames() {
        let mut roster = CrdtRoster::new();
        roster.create("a").unwrap();
        roster.create_from_text("笔记/b.md", "你好").unwrap();
        for i in 0..60 {
            let doc = roster.open_or_create("a").unwrap();
            let at = doc.text().chars().count();
            doc.insert(at, &format!("第 {i} 行内容\n")).unwrap();
            let doc = roster.get_mut("a").unwrap();
            let len = doc.text().chars().count();
            if len > 4 {
                // 从尾部反复删除可制造大量不可见历史，快照应把它们清掉。
                doc.delete(len - 3, 3).unwrap();
            }
        }
        let fat_total: usize = roster
            .encode_diff(&BTreeMap::new())
            .unwrap()
            .values()
            .map(Vec::len)
            .sum();
        let texts: Vec<(String, String)> = roster
            .ids()
            .into_iter()
            .map(|id| {
                let text = roster.get(&id).unwrap().text();
                (id, text)
            })
            .collect();

        let snap = roster.snapshot();
        assert_eq!(snap.ids(), roster.ids());
        for (id, text) in &texts {
            assert_eq!(snap.get(id).unwrap().text(), *text);
        }
        let slim_total: usize = snap
            .encode_diff(&BTreeMap::new())
            .unwrap()
            .values()
            .map(Vec::len)
            .sum();
        assert!(
            slim_total * 2 < fat_total,
            "快照应显著压缩：fat={fat_total} slim={slim_total}"
        );
    }

    #[test]
    fn cjk_ids_and_text_survive_full_sync() {
        let mut source = CrdtRoster::new();
        source
            .create_from_text("笔记/甲.md", "你好，世界 🌏")
            .unwrap();
        source
            .get_mut("笔记/甲.md")
            .unwrap()
            .insert(2, "码札")
            .unwrap();
        let mut target = CrdtRoster::new();
        let updates = source.encode_diff(&BTreeMap::new()).unwrap();
        assert_eq!(target.apply_updates(&updates).unwrap(), vec!["笔记/甲.md"]);
        assert_eq!(target.ids(), vec!["笔记/甲.md"]);
        // 字符索引 2 落在"，"之前：插入后不得把中文切坏。
        assert_eq!(
            target.get("笔记/甲.md").unwrap().text(),
            "你好码札，世界 🌏"
        );
    }

    #[test]
    fn empty_roster_operations_are_noops() {
        let roster = CrdtRoster::new();
        assert!(roster.ids().is_empty());
        assert_eq!(roster.len(), 0);
        assert!(roster.is_empty());
        assert!(roster.state_vectors().is_empty());
        assert!(roster.encode_diff(&BTreeMap::new()).unwrap().is_empty());
        assert!(roster.snapshot().is_empty());
        let mut roster = roster;
        assert!(roster.apply_updates(&BTreeMap::new()).unwrap().is_empty());
        assert!(roster.is_empty());
    }

    // ---------- S1/D95/D98：墓碑元数据纯模型单测 ----------

    /// 测试用 OpId，减少重复样板。
    fn op(clock: u64, device: &str) -> OpId {
        OpId::new(clock, device).unwrap()
    }

    /// 测试用自包含 Yjs v1 全量帧。
    fn frame(text: &str) -> Vec<u8> {
        CrdtDoc::from_text(text).encode_update()
    }

    /// 把可恢复副本帧应用到空文档，验证文本没有静默丢失。
    fn recover(bytes: &[u8]) -> String {
        let mut doc = CrdtDoc::new();
        doc.apply_update(bytes).unwrap();
        doc.text()
    }

    #[test]
    fn remove_with_op_creates_tombstone_and_keeps_recoverable_frame() {
        let mut roster = CrdtRoster::new();
        roster.create_from_text("notes/a.md", "原文").unwrap();
        let tombstone = roster.remove_with_op("notes/a.md", op(3, "dev-a")).unwrap();
        assert!(!roster.contains("notes/a.md"));
        assert!(roster.is_tombstoned("notes/a.md"));
        assert_eq!(tombstone.op_id(), &op(3, "dev-a"));
        assert_eq!(tombstone.conflict_frames().len(), 1);
        assert_eq!(recover(&tombstone.conflict_frames()[0]), "原文");
        assert_eq!(
            roster.tombstones().keys().cloned().collect::<Vec<_>>(),
            vec!["notes/a.md"]
        );
        assert_eq!(roster.meta_snapshot().len(), 1);
        // D94 的 remove 不翻案：文档已删，旧 API 仍报不存在且不产生新墓碑。
        assert!(matches!(
            roster.remove("notes/a.md"),
            Err(CrdtError::DocumentNotFound(_))
        ));
        assert_eq!(roster.tombstones().len(), 1);
    }

    #[test]
    fn tombstone_wins_over_low_upsert_and_keeps_conflict_copy() {
        let mut roster = CrdtRoster::new();
        let tombstone = Tombstone::new("a.md", op(5, "dev-a")).unwrap();
        roster
            .apply_meta(&[MetaChange::Tombstone(tombstone)])
            .unwrap();
        // 离线编辑的 op_id 更低：不得复活，但内容必须落在可恢复副本里。
        let affected = roster
            .apply_meta(&[MetaChange::Upsert {
                doc_id: "a.md".to_owned(),
                op_id: op(4, "dev-b"),
                frame: frame("离线编辑"),
            }])
            .unwrap();
        assert_eq!(affected, vec!["a.md"]);
        assert!(!roster.contains("a.md"));
        assert!(roster.is_tombstoned("a.md"));
        let frames = &roster.tombstones()["a.md"].conflict_frames();
        assert_eq!(frames.len(), 1);
        assert_eq!(recover(&frames[0]), "离线编辑");
    }

    #[test]
    fn gc_tombstones_keeps_tombstone_and_blocks_old_upsert() {
        let mut roster = CrdtRoster::new();
        roster.create_from_text("笔记/c.md", "原文").unwrap();
        roster.remove_with_op("笔记/c.md", op(2, "dev-a")).unwrap();

        let mut acks = DeviceAcks::new();
        acks.insert("dev-a".to_owned(), op(2, "dev-a"));
        acks.insert("dev-b".to_owned(), op(5, "dev-b"));
        let report = roster.gc_tombstones(&acks).unwrap();
        assert_eq!(report.shrunk().to_vec(), vec!["笔记/c.md".to_owned()]);
        assert_eq!(report.archived().len(), 1);
        assert_eq!(recover(&report.archived()[0].frames()[0]), "原文");
        assert!(roster.is_tombstoned("笔记/c.md"));
        assert!(
            roster.tombstones()["笔记/c.md"]
                .conflict_frames()
                .is_empty()
        );

        // 收缩后墓碑的 op_id 仍在：低/等 op_id 的旧 upsert 不得复活，新帧重新成为副本。
        roster
            .apply_meta(&[MetaChange::Upsert {
                doc_id: "笔记/c.md".to_owned(),
                op_id: op(2, "dev-a"),
                frame: frame("旧编辑"),
            }])
            .unwrap();
        assert!(!roster.contains("笔记/c.md"));
        let frames = &roster.tombstones()["笔记/c.md"].conflict_frames();
        assert_eq!(frames.len(), 1);
        assert_eq!(recover(&frames[0]), "旧编辑");
    }

    #[test]
    fn only_strictly_higher_upsert_revives_tombstoned_doc() {
        let mut roster = CrdtRoster::new();
        roster
            .apply_meta(&[MetaChange::Tombstone(
                Tombstone::new("a.md", op(5, "dev-a")).unwrap(),
            )])
            .unwrap();
        // 相同 op_id：墓碑 wins（排序时 Upsert 先于 Tombstone，也与输入顺序无关）。
        roster
            .apply_meta(&[MetaChange::Upsert {
                doc_id: "a.md".to_owned(),
                op_id: op(5, "dev-a"),
                frame: frame("同钟编辑"),
            }])
            .unwrap();
        assert!(!roster.contains("a.md"));
        assert!(roster.is_tombstoned("a.md"));
        assert_eq!(
            recover(&roster.tombstones()["a.md"].conflict_frames()[0]),
            "同钟编辑"
        );

        // 严格更高 op_id：删除墓碑并用全量帧复活。
        roster
            .apply_meta(&[MetaChange::Upsert {
                doc_id: "a.md".to_owned(),
                op_id: op(6, "dev-b"),
                frame: frame("重建后的内容"),
            }])
            .unwrap();
        assert!(roster.contains("a.md"));
        assert!(!roster.is_tombstoned("a.md"));
        assert!(roster.tombstones().is_empty());
        assert_eq!(roster.get("a.md").unwrap().text(), "重建后的内容");
    }

    #[test]
    fn higher_tombstone_overwrites_lower_and_unions_recoverable_frames() {
        let mut roster = CrdtRoster::new();
        let low = Tombstone::new("a.md", op(2, "dev-a"))
            .unwrap()
            .with_conflict_frame(frame("旧副本"))
            .unwrap();
        let high = Tombstone::new("a.md", op(9, "dev-b"))
            .unwrap()
            .with_reason(TombstoneReason::RenameSource)
            .with_conflict_frame(frame("新副本"))
            .unwrap();
        roster
            .apply_meta(&[MetaChange::Tombstone(low.clone())])
            .unwrap();
        roster.apply_meta(&[MetaChange::Tombstone(high)]).unwrap();
        let winner = &roster.tombstones()["a.md"];
        assert_eq!(winner.op_id(), &op(9, "dev-b"));
        assert_eq!(winner.reason(), TombstoneReason::RenameSource);
        assert_eq!(winner.conflict_frames().len(), 2);
        // 更低 op_id 的墓碑再次到达：不覆盖胜出者，但副本帧仍归并（幂等）。
        roster.apply_meta(&[MetaChange::Tombstone(low)]).unwrap();
        let winner = &roster.tombstones()["a.md"];
        assert_eq!(winner.op_id(), &op(9, "dev-b"));
        assert_eq!(winner.conflict_frames().len(), 2);
    }

    #[test]
    fn merge_meta_propagates_tombstone_and_preserves_local_content() {
        let mut local = CrdtRoster::new();
        local.create_from_text("a.md", "本地内容").unwrap();
        let mut remote_meta = RosterMeta::new();
        remote_meta
            .merge_tombstone(Tombstone::new("a.md", op(7, "dev-b")).unwrap())
            .unwrap();
        let affected = local.merge_meta(&remote_meta).unwrap();
        assert_eq!(affected, vec!["a.md"]);
        assert!(!local.contains("a.md"));
        assert!(local.is_tombstoned("a.md"));
        let frames = &local.tombstones()["a.md"].conflict_frames();
        assert_eq!(frames.len(), 1);
        assert_eq!(recover(&frames[0]), "本地内容");
    }

    #[test]
    fn apply_updates_with_meta_tombstone_blocks_stale_content() {
        let mut roster = CrdtRoster::new();
        roster
            .apply_meta(&[MetaChange::Tombstone(
                Tombstone::new("a.md", op(5, "dev-a")).unwrap(),
            )])
            .unwrap();
        let mut updates = BTreeMap::new();
        updates.insert("a.md".to_owned(), frame("远端旧内容"));
        let affected = roster.apply_updates_with_meta(&updates, &[]).unwrap();
        assert_eq!(affected, vec!["a.md"]);
        assert!(!roster.contains("a.md"));
        assert!(roster.is_tombstoned("a.md"));
        // 旧内容不静默丢弃：作为可恢复副本挂在墓碑上。
        let frames = &roster.tombstones()["a.md"].conflict_frames();
        assert_eq!(frames.len(), 1);
        assert_eq!(recover(&frames[0]), "远端旧内容");
        // 无墓碑时新 API 仍沿用 D94 的未知 ID 自动建文档语义。
        let mut fresh_updates = BTreeMap::new();
        fresh_updates.insert("b.md".to_owned(), frame("新文档"));
        roster.apply_updates_with_meta(&fresh_updates, &[]).unwrap();
        assert_eq!(roster.get("b.md").unwrap().text(), "新文档");
    }

    #[test]
    fn bad_meta_or_frame_rejects_whole_batch_without_mutation() {
        let mut roster = CrdtRoster::new();
        roster.create_from_text("a.md", "保持不变").unwrap();
        let before_sv = roster.state_vectors();
        let bad_meta = MetaChange::Upsert {
            doc_id: "a.md".to_owned(),
            op_id: op(9, "dev-a"),
            frame: vec![0xff, 0xff, 0xff],
        };
        let good_tombstone = MetaChange::Tombstone(Tombstone::new("b.md", op(8, "dev-a")).unwrap());
        let mut updates = BTreeMap::new();
        updates.insert("a.md".to_owned(), frame("会被中途应用的新内容"));
        let result = roster.apply_updates_with_meta(&updates, &[good_tombstone, bad_meta]);
        assert!(matches!(result, Err(CrdtError::InvalidMetaFrame { .. })));
        assert_eq!(roster.get("a.md").unwrap().text(), "保持不变");
        assert_eq!(roster.state_vectors(), before_sv);
        assert_eq!(roster.ids(), vec!["a.md"]);
        assert!(roster.tombstones().is_empty());

        // 非法 DocId 同样整批拒绝；非法 OpId 在 OpId::new 构造期即被拒绝（见 meta.rs 单测）。
        let bad_id = MetaChange::Upsert {
            doc_id: "bad\nid".to_owned(),
            op_id: op(9, "dev-a"),
            frame: frame("x"),
        };
        assert!(matches!(
            roster.apply_meta(&[bad_id]),
            Err(CrdtError::InvalidDocumentId(_))
        ));
        assert_eq!(roster.get("a.md").unwrap().text(), "保持不变");
        assert!(roster.tombstones().is_empty());
    }

    #[test]
    fn batch_order_does_not_change_merge_result() {
        // 同 op_id：墓碑胜出，不受输入顺序影响。
        let tomb = MetaChange::Tombstone(Tombstone::new("a.md", op(7, "dev-a")).unwrap());
        let upsert = MetaChange::Upsert {
            doc_id: "a.md".to_owned(),
            op_id: op(7, "dev-a"),
            frame: frame("同钟编辑"),
        };
        let mut first = CrdtRoster::new();
        first.apply_meta(&[upsert.clone(), tomb.clone()]).unwrap();
        let mut second = CrdtRoster::new();
        second.apply_meta(&[tomb, upsert.clone()]).unwrap();
        assert_eq!(first.meta_snapshot(), second.meta_snapshot());
        assert!(!first.contains("a.md") && !second.contains("a.md"));
        assert_eq!(
            recover(&first.tombstones()["a.md"].conflict_frames()[0]),
            "同钟编辑"
        );

        // 更高 op_id upsert：两种顺序都应复活。
        let tomb_low = MetaChange::Tombstone(Tombstone::new("b.md", op(2, "dev-a")).unwrap());
        let upsert_high = MetaChange::Upsert {
            doc_id: "b.md".to_owned(),
            op_id: op(9, "dev-b"),
            frame: frame("复活"),
        };
        let mut third = CrdtRoster::new();
        third
            .apply_meta(&[tomb_low.clone(), upsert_high.clone()])
            .unwrap();
        let mut fourth = CrdtRoster::new();
        fourth.apply_meta(&[upsert_high, tomb_low]).unwrap();
        assert_eq!(third.ids(), fourth.ids());
        assert_eq!(third.meta_snapshot(), fourth.meta_snapshot());
        assert_eq!(third.get("b.md").unwrap().text(), "复活");
        assert!(!third.is_tombstoned("b.md"));
    }

    #[test]
    fn encode_diff_with_meta_keeps_content_and_metadata_separate() {
        let mut roster = CrdtRoster::new();
        roster.create_from_text("a.md", "内容").unwrap();
        roster
            .apply_meta(&[MetaChange::Tombstone(
                Tombstone::new("b.md", op(1, "dev-a")).unwrap(),
            )])
            .unwrap();
        let (diff, meta) = roster.encode_diff_with_meta(&BTreeMap::new()).unwrap();
        assert_eq!(diff, roster.encode_diff(&BTreeMap::new()).unwrap());
        // D94 旧接口只表达活动文档内容，不含墓碑。
        assert_eq!(diff.keys().cloned().collect::<Vec<_>>(), vec!["a.md"]);
        assert!(meta.is_tombstoned("b.md"));
        assert_eq!(meta.len(), 1);
    }

    #[test]
    fn legacy_apply_updates_still_creates_tombstoned_docs() {
        // 明确的兼容边界：D94 旧 API 不查墓碑；同步路径必须走 apply_updates_with_meta。
        let mut roster = CrdtRoster::new();
        roster
            .apply_meta(&[MetaChange::Tombstone(
                Tombstone::new("a.md", op(5, "dev-a")).unwrap(),
            )])
            .unwrap();
        let mut updates = BTreeMap::new();
        updates.insert("a.md".to_owned(), frame("按旧语义复活"));
        let affected = roster.apply_updates(&updates).unwrap();
        assert_eq!(affected, vec!["a.md"]);
        assert_eq!(roster.get("a.md").unwrap().text(), "按旧语义复活");
        assert!(roster.is_tombstoned("a.md"));
    }

    #[test]
    fn snapshot_preserves_tombstones() {
        let mut roster = CrdtRoster::new();
        roster.create_from_text("a.md", "内容").unwrap();
        roster.remove_with_op("a.md", op(1, "dev-a")).unwrap();
        let snap = roster.snapshot();
        assert_eq!(snap.meta_snapshot(), roster.meta_snapshot());
        assert!(snap.is_tombstoned("a.md"));
        assert_eq!(
            recover(&snap.tombstones()["a.md"].conflict_frames()[0]),
            "内容"
        );
    }

    #[test]
    fn cjk_docid_tombstone_and_revive() {
        let mut roster = CrdtRoster::new();
        roster
            .create_from_text("笔记/甲.md", "你好，世界 🌏")
            .unwrap();
        roster
            .remove_with_op("笔记/甲.md", op(2, "设备甲"))
            .unwrap();
        assert!(roster.is_tombstoned("笔记/甲.md"));
        assert_eq!(
            recover(&roster.tombstones()["笔记/甲.md"].conflict_frames()[0]),
            "你好，世界 🌏"
        );
        roster
            .apply_meta(&[MetaChange::Upsert {
                doc_id: "笔记/甲.md".to_owned(),
                op_id: op(3, "设备乙"),
                frame: frame("复活后继续写"),
            }])
            .unwrap();
        assert!(!roster.is_tombstoned("笔记/甲.md"));
        assert_eq!(roster.ids(), vec!["笔记/甲.md"]);
        assert_eq!(roster.get("笔记/甲.md").unwrap().text(), "复活后继续写");
    }
}
