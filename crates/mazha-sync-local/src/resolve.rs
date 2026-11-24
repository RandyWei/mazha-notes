//! 冲突/快照的显式解决材料与选择类型（S5b 第一批 D104，契约见 `docs/SYNC-SPEC.md` §5.11.2）。
//!
//! 这里只放**纯函数与数据类型**：冲突副本帧解码、确定性拼接。真正的状态变更在
//! `engine.rs` 的 `LocalSyncEngine::resolve_*` 方法中完成；本 crate 不依赖 `mazha-core`、
//! 不碰 UI，也不主动触发网络同步。

use mazha_crdt::{CrdtDoc, CrdtResult, DocId};

/// 墓碑冲突的三种用户选择（语义见 `docs/SYNC-SPEC.md` §5.11.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictChoice {
    /// 保留本地：用冲突副本文本替换当前正文，墓碑与副本保留。
    KeepLocal,
    /// 采用远端：接受远端删除，墓碑与副本留作审计，不写回正文。
    UseRemote,
    /// 合并：确定性拼接“冲突副本 + 换行 + 本地正文”。
    Merge,
}

/// 快照确认门槛的三种用户选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotChoice {
    /// 保留本地：保留本地文件与 outbox，只清除本轮确认提示。
    KeepLocal,
    /// 采用远端：显式清空未推送 outbox，下一轮由服务端快照覆盖本地。
    UseRemote,
    /// 合并：保留 outbox 让 CRDT 在下一轮自行合并本地与远端，不清空数据。
    Merge,
}

/// 一次显式解决的结果，供桌面回写文档与刷新状态。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolveOutcome {
    /// 需要写回当前文档正文的文本；`None` 表示不改动本地文件。
    pub document_text: Option<String>,
    /// 因 upsert 成功复活的活动文档 ID（升序）。
    pub revived_docs: Vec<DocId>,
    /// 是否显式清空了 outbox（仅快照 `UseRemote` 为 true）。
    pub outbox_cleared: bool,
    /// 是否清除了 `needs_confirmation` 标记。
    pub confirmation_cleared: bool,
}

/// 把墓碑里所有冲突副本帧还原成确定性文本。
///
/// 每个 `conflict_frames()` 项都是自包含 Yjs v1 全量帧；先**全部**解码校验，任一坏帧
/// 直接返回错误（调用方据此零改动），再把恢复出的文本去重后按帧序用 `\n` 拼接，
/// 保证没有哪一份副本被静默丢弃。
///
/// # Errors
/// 任一帧无法解码时返回 [`CrdtError::Decode`]，不返回部分结果。
pub fn conflict_copy_text(frames: &[Vec<u8>]) -> CrdtResult<String> {
    let mut texts = Vec::with_capacity(frames.len());
    for frame in frames {
        let mut doc = CrdtDoc::new();
        doc.apply_update(frame)?;
        let text = doc.text();
        if !texts.contains(&text) {
            texts.push(text);
        }
    }
    Ok(texts.join("\n"))
}

/// 冲突合并的确定性拼接文本：冲突副本在前、本地正文在后。
///
/// 注意这**不是 CRDT 语义合并**；CRDT 自身的并发合并仍由 `SyncSession` 负责。
#[must_use]
pub fn merged_conflict_text(conflict_text: &str, local_text: &str) -> String {
    format!("{conflict_text}\n{local_text}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use mazha_crdt::CrdtDoc;

    /// 造一份自包含全量帧。
    fn frame(text: &str) -> Vec<u8> {
        CrdtDoc::from_text(text).encode_update()
    }

    #[test]
    fn conflict_copy_text_recovers_cjk_frames_deterministically() {
        let frames = vec![frame("冲突甲"), frame("冲突乙"), frame("冲突甲")];
        assert_eq!(conflict_copy_text(&frames).unwrap(), "冲突甲\n冲突乙");
        // 帧顺序变化时结果随之变化，但同一份输入永远得到同一份输出。
        assert_eq!(conflict_copy_text(&frames[1..]).unwrap(), "冲突乙\n冲突甲");
        assert_eq!(conflict_copy_text(&[]).unwrap(), "");
    }

    #[test]
    fn bad_frame_is_rejected_without_partial_text() {
        let frames = vec![frame("可用"), vec![0xff, 0xff, 0xff]];
        assert!(conflict_copy_text(&frames).is_err());
        assert!(conflict_copy_text(&[vec![0xff, 0xff, 0xff]]).is_err());
    }

    #[test]
    fn merged_text_keeps_conflict_copy_before_local() {
        assert_eq!(
            merged_conflict_text("远端副本", "本地正文"),
            "远端副本\n本地正文"
        );
        assert_eq!(merged_conflict_text("", "本地"), "\n本地");
    }

    #[test]
    fn outcome_default_is_non_destructive() {
        let outcome = ResolveOutcome::default();
        assert!(outcome.document_text.is_none());
        assert!(outcome.revived_docs.is_empty());
        assert!(!outcome.outbox_cleared);
        assert!(!outcome.confirmation_cleared);
    }
}
