//! 快照载荷的 CRDT 级压缩（Phase 5 S5b 第二批，D106）。
//!
//! 压缩只作用于快照引导路径：把 `SnapshotPayload` 中的 Yjs v1 帧在空名册上重放后
//! 重新快照，使帧体积收敛到 O(文本长度)。不新增外部依赖、不读时钟、不碰 IO/GUI/async，
//! 也不修改九种 `SyncMessage` 与 `MZS0` wire 编码。

use std::collections::BTreeMap;

use mazha_crdt::{CrdtResult, CrdtRoster, MetaChange};

use crate::{DocFrameMap, SnapshotPayload};

/// 压缩快照载荷的内容帧，保持 `meta`/`cursor` 语义不变。
///
/// 在新的空名册上一次原子应用全部内容帧与墓碑元数据（坏帧整批拒绝），随后 `snapshot()`
/// 清掉 CRDT 历史，再用空状态向量取回全量帧。输出仍是标准 Yjs v1 帧，可直接用于新设备
/// 引导；**不得**作为增量 pull 帧应用到已有名册（快照是新 CRDT 身份，会重复文本）。
///
/// # Errors
/// 任一内容帧或墓碑元数据非法时返回 `CrdtError`，入参不可变、零改动。
pub fn compact_snapshot_payload(payload: &SnapshotPayload) -> CrdtResult<SnapshotPayload> {
    // 元数据快照只有墓碑；转成 MetaChange 走与同步路径相同的原子提交边界。
    let changes: Vec<MetaChange> = payload
        .meta
        .tombstones()
        .values()
        .cloned()
        .map(MetaChange::Tombstone)
        .collect();

    let mut roster = CrdtRoster::new();
    roster.apply_updates_with_meta(&payload.frames, &changes)?;

    // 快照清空历史；墓碑原样保留，被墓碑挡下的内容帧已在上面并入 conflict_frames。
    let compact = roster.snapshot();
    let empty_remote: DocFrameMap = BTreeMap::new();
    let (frames, meta) = compact.encode_diff_with_meta(&empty_remote)?;
    Ok(SnapshotPayload {
        frames,
        meta,
        cursor: payload.cursor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mazha_crdt::{CrdtDoc, OpId, RosterMeta, Tombstone};

    use crate::SyncCursor;

    /// 构造一份带大量历史的快照载荷（中文/emoji + 反复插入删除）。
    fn fat_payload() -> SnapshotPayload {
        let mut roster = CrdtRoster::new();
        roster
            .create_from_text("笔记/甲.md", "你好，世界 🌏")
            .unwrap();
        roster.create_from_text("notes/b.md", "baseline").unwrap();
        for i in 0..100 {
            let inserted = format!("第 {i} 行临时内容{}", "x".repeat(80));
            let inserted_len = inserted.chars().count();
            let doc = roster.open_or_create("笔记/甲.md").unwrap();
            // 每轮在末尾插入较长临时内容，再删到只留 1 个字符，制造大量不可见历史。
            doc.push(&inserted).unwrap();
            let len = doc.text().chars().count();
            doc.delete(len - (inserted_len - 1), inserted_len - 1)
                .unwrap();
        }
        // 加一条墓碑，验证元数据与冲突副本在压缩中不被丢弃。
        let deleted = CrdtDoc::from_text("被删除的草稿").encode_update();
        let mut meta = RosterMeta::new();
        meta.merge_tombstone(
            Tombstone::new("notes/old.md", OpId::new(3, "dev-a").unwrap())
                .unwrap()
                .with_conflict_frame(deleted)
                .unwrap(),
        )
        .unwrap();

        SnapshotPayload {
            frames: roster.encode_diff(&BTreeMap::new()).unwrap(),
            meta,
            cursor: SyncCursor::new(42),
        }
    }

    /// 把快照载荷应用到空名册，返回名册供文本断言。
    fn apply_payload(payload: &SnapshotPayload) -> CrdtRoster {
        let mut roster = CrdtRoster::new();
        let changes: Vec<MetaChange> = payload
            .meta
            .tombstones()
            .values()
            .cloned()
            .map(MetaChange::Tombstone)
            .collect();
        roster
            .apply_updates_with_meta(&payload.frames, &changes)
            .unwrap();
        roster
    }

    #[test]
    fn compacted_snapshot_keeps_text_meta_and_cursor() {
        let payload = fat_payload();
        let compact = compact_snapshot_payload(&payload).unwrap();
        assert_eq!(compact.cursor, payload.cursor);
        assert_eq!(compact.meta, payload.meta);

        let original = apply_payload(&payload);
        let restored = apply_payload(&compact);
        assert_eq!(restored.ids(), original.ids());
        for id in &restored.ids() {
            assert_eq!(
                restored.get(id).unwrap().text(),
                original.get(id).unwrap().text()
            );
        }
        assert!(restored.is_tombstoned("notes/old.md"));
        assert_eq!(
            recovered_text(&restored.tombstones()["notes/old.md"].conflict_frames()[0]),
            "被删除的草稿"
        );
    }

    #[test]
    fn compacted_frames_are_much_smaller() {
        let payload = fat_payload();
        let before: usize = payload.frames.values().map(Vec::len).sum();
        let compact = compact_snapshot_payload(&payload).unwrap();
        let after: usize = compact.frames.values().map(Vec::len).sum();
        assert!(
            after * 3 < before,
            "压缩应显著减小：before={before} after={after}"
        );
    }

    #[test]
    fn peers_on_compacted_snapshot_converge_after_new_edits() {
        let compact = compact_snapshot_payload(&fat_payload()).unwrap();
        let mut a = apply_payload(&compact);
        let mut b = apply_payload(&compact);
        assert_eq!(
            a.get("笔记/甲.md").unwrap().text(),
            b.get("笔记/甲.md").unwrap().text()
        );

        // 两台都基于压缩后的新身份继续编辑，再走普通增量同步应收敛。
        a.get_mut("笔记/甲.md").unwrap().push("\nA 新增").unwrap();
        b.get_mut("notes/b.md").unwrap().push(" + B").unwrap();
        let a_diff = a.encode_diff(&b.state_vectors()).unwrap();
        let b_diff = b.encode_diff(&a.state_vectors()).unwrap();
        a.apply_updates(&b_diff).unwrap();
        b.apply_updates(&a_diff).unwrap();
        assert_eq!(a.ids(), b.ids());
        for id in &a.ids() {
            assert_eq!(a.get(id).unwrap().text(), b.get(id).unwrap().text());
        }
    }

    #[test]
    fn bad_frame_returns_error_and_keeps_input_unchanged() {
        let mut payload = fat_payload();
        payload
            .frames
            .insert("notes/bad.md".to_owned(), vec![0xff, 0xff, 0xff]);
        let before = payload.clone();
        assert!(compact_snapshot_payload(&payload).is_err());
        assert_eq!(payload, before);
    }

    /// 从可恢复副本帧还原文本。
    fn recovered_text(frame: &[u8]) -> String {
        let mut doc = CrdtDoc::new();
        doc.apply_update(frame).unwrap();
        doc.text()
    }
}
