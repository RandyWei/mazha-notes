//! CRDT 桥接：`Document` 编辑 ↔ `CrdtDoc`（yrs Y.Text）双向同步。
//!
//! ## 为什么需要桥接
//!
//! - `Document` 的 [`Edit`] 使用 **UTF-8 字节偏移**（rope 语义）。
//! - `CrdtDoc` 使用 **字符索引**（yrs 内部按 UTF-8 字节计量，但对外接口是字符）。
//!
//! 因此所有跨边界操作都要先做字节→字符转换，避免 CJK 等多字节字符被切坏。
//!
//! ## 集成方式
//! - 本地编辑：`Document::apply` 成功后，用 [`apply_edit_to_crdt`] 把同一编辑
//!   同步到 `CrdtDoc`（经状态向量/更新帧可再同步给远端）。
//! - 远端更新：远端帧并入 `CrdtDoc` 后，用 [`crdt_to_document`] 取新文本，再经
//!   S5b/D103 的 [`crate::RemotePatch`] 增量应用到 `Document`；整体重建只作为
//!   `crdt_to_document` 的独立工具保留（默认同步路径不再走它，以免丢 undo/选区）。

use crate::remote_patch::RemotePatch;
use crate::{Document, Edit, Workspace};
use mazha_crdt::CrdtDoc;
use mazha_store::{DocPath, StorageProvider, StoreError};

/// CRDT 持久化/加载错误。
#[derive(Debug, thiserror::Error)]
pub enum CrdtSyncError {
    /// 存储层错误（文件不存在/读写失败等）。
    #[error(transparent)]
    Store(#[from] StoreError),

    /// CRDT 更新帧解码失败（文件损坏或格式不符）。
    #[error(transparent)]
    Crdt(#[from] mazha_crdt::CrdtError),
}

/// 本模块的 `Result` 别名。
pub type CrdtSyncResult<T> = std::result::Result<T, CrdtSyncError>;

/// 从 `Document` 的当前源文本创建一份 `CrdtDoc`（用于初次上云/持久化）。
pub fn document_to_crdt(doc: &Document) -> CrdtDoc {
    CrdtDoc::from_text(&doc.text())
}

/// 把 `CrdtDoc` 的完整更新帧写入工作区文件（覆盖写；自动创建父目录）。
///
/// 这是离线协同的最小可行方案：本地持续写更新帧，下次打开时加载并继续。
///
/// # Errors
/// 存储层写失败时返回 [`CrdtSyncError::Store`]。
pub fn save_crdt<S: StorageProvider>(
    ws: &Workspace<S>,
    path: &DocPath,
    crdt: &CrdtDoc,
) -> CrdtSyncResult<()> {
    ws.store().write(path, &crdt.encode_update())?;
    Ok(())
}

/// 从工作区文件加载 `CrdtDoc`（无更新帧内容时返回空文档）。
///
/// # Errors
/// - 文件不存在/读取失败 → [`CrdtSyncError::Store`]；
/// - 内容是损坏/非 Yjs v1 更新帧 → [`CrdtSyncError::Crdt`]。
pub fn load_crdt<S: StorageProvider>(ws: &Workspace<S>, path: &DocPath) -> CrdtSyncResult<CrdtDoc> {
    let bytes = ws.store().read(path)?;
    let mut crdt = CrdtDoc::new();
    crdt.apply_update(&bytes)?;
    Ok(crdt)
}

/// 从 `CrdtDoc` 的当前源文本重建 `Document`（远端更新后调用）。
///
/// 注意：这会丢弃本地的 undo/redo 历史与选区，属于 MVP 的整体重建策略。
pub fn crdt_to_document(crdt: &CrdtDoc) -> Document {
    Document::new(&crdt.text())
}

/// 把一次 [`Edit`] 同步应用到 `CrdtDoc`（字节偏移 → 字符索引）。
///
/// [`Edit::ToggleInline`] / [`Edit::ToggleBlock`] 会在 `Document::apply` 时
/// 先解析成基础编辑（Insert/Delete/Replace），因此这里只需处理三种基础编辑。
///
/// # Errors
/// 偏移越界时返回 [`mazha_crdt::CrdtError`]（由 `CrdtDoc` 的越界错误转换而来）。
pub fn apply_edit_to_crdt(crdt: &mut CrdtDoc, edit: &Edit) -> mazha_crdt::CrdtResult<()> {
    match edit {
        Edit::Insert { at, text } => {
            let idx = byte_to_char(&crdt.text(), *at);
            crdt.insert(idx, text)?;
        }
        Edit::Delete { range } => {
            let start = byte_to_char(&crdt.text(), range.start);
            let end = byte_to_char(&crdt.text(), range.end);
            crdt.delete(start, end - start)?;
        }
        Edit::Replace { range, text } => {
            let start = byte_to_char(&crdt.text(), range.start);
            let end = byte_to_char(&crdt.text(), range.end);
            crdt.replace(start, end - start, text)?;
        }
        // 高层命令：Document::apply 已 resolve 成基础编辑，这里兜底忽略
        // （如需同步，请先 resolve 再传入）。
        Edit::ToggleInline { .. } | Edit::ToggleBlock { .. } => {}
    }
    Ok(())
}

/// 字节偏移 → 字符索引（越界返回文本长度对应的字符数）。
fn byte_to_char(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].chars().count()
}

/// 一次远端应用的详细结果：是否变化，以及供 UI remap 光标/选区的补丁。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteApplyOutcome {
    /// 文档文本是否被远端帧改变；`false` 时所有历史与缓存均未被触动。
    pub changed: bool,
    /// 实际应用的增量补丁；无变化时为 `None`。
    pub patch: Option<RemotePatch>,
}

/// 应用一个远端更新帧并返回补丁（S5b/D103）。
///
/// `bytes` 为 Yjs v1 编码的更新（由对端的 `CrdtDoc::encode_diff` / `encode_update`
/// 产出）。先记下旧文本，再把帧并入 `crdt`；仅当新文本与旧文本不同才生成
/// [`RemotePatch`] 并增量应用到 `doc`，远端变更不进 undo、已有 undo 历史重映射、
/// redo 清空（契约见 `docs/SYNC-SPEC.md` §5.11.1）。
///
/// # Errors
/// 更新帧解码失败时返回 [`mazha_crdt::CrdtError`]，此时 `crdt`/`doc` 均不变；
/// 若 `doc` 与 `crdt` 的旧文本本就不一致（调用方绕过了同步路径），补丁无法匹配时
/// 返回 [`mazha_crdt::CrdtError::OutOfBounds`] 且不修改 `doc`。
pub fn apply_remote_detailed(
    doc: &mut Document,
    crdt: &mut CrdtDoc,
    bytes: &[u8],
) -> mazha_crdt::CrdtResult<RemoteApplyOutcome> {
    let before = doc.text();
    // CRDT 自带两阶段校验：坏帧在这里被拒绝，后面的文档改动不会发生。
    crdt.apply_update(bytes)?;
    let after = crdt.text();
    let Some(patch) = RemotePatch::from_texts(&before, &after) else {
        return Ok(RemoteApplyOutcome {
            changed: false,
            patch: None,
        });
    };
    doc.apply_remote_patch(&patch)
        .map_err(|_| mazha_crdt::CrdtError::OutOfBounds {
            index: patch.range.start,
            len: doc.len(),
        })?;
    Ok(RemoteApplyOutcome {
        changed: true,
        patch: Some(patch),
    })
}

/// 应用一个远端更新帧：先并入 `crdt`，若源文本变化则增量更新 `doc`。
///
/// D97/D95 的对外签名保持不变（返回 `CrdtResult<bool>`），内部改走
/// [`apply_remote_detailed`]：远端变更不再整体重建 `Document`，因此不会丢
/// undo/redo 与选区坐标（由调用方用返回的补丁 remap）。无变化时不触碰历史。
///
/// # Errors
/// 同 [`apply_remote_detailed`]。
pub fn apply_remote(
    doc: &mut Document,
    crdt: &mut CrdtDoc,
    bytes: &[u8],
) -> mazha_crdt::CrdtResult<bool> {
    Ok(apply_remote_detailed(doc, crdt, bytes)?.changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_to_crdt_roundtrip() {
        let doc = Document::new("# 标题\n正文");
        let crdt = document_to_crdt(&doc);
        assert_eq!(crdt.text(), "# 标题\n正文");
        let back = crdt_to_document(&crdt);
        assert_eq!(back.text(), doc.text());
    }

    #[test]
    fn sync_insert_to_crdt() {
        let mut doc = Document::new("hello world");
        let mut crdt = document_to_crdt(&doc);
        doc.apply(Edit::Insert {
            at: 5,
            text: " 你好".to_string(),
        })
        .unwrap();
        apply_edit_to_crdt(
            &mut crdt,
            &Edit::Insert {
                at: 5,
                text: " 你好".to_string(),
            },
        )
        .unwrap();
        assert_eq!(crdt.text(), doc.text());
    }

    #[test]
    fn sync_delete_cjk_safe() {
        // 删除 "你好"（字节 0..6），验证字节→字符转换不会切坏 CJK
        let mut doc = Document::new("你好世界");
        let mut crdt = document_to_crdt(&doc);
        doc.apply(Edit::Delete { range: 0..6 }).unwrap();
        apply_edit_to_crdt(&mut crdt, &Edit::Delete { range: 0..6 }).unwrap();
        assert_eq!(crdt.text(), "世界");
        assert_eq!(crdt.text(), doc.text());
    }

    #[test]
    fn sync_replace_after_cjk() {
        let mut doc = Document::new("你好 world");
        let mut crdt = document_to_crdt(&doc);
        // 替换 "world"（字节 7..12）为 "世界"
        doc.apply(Edit::Replace {
            range: 7..12,
            text: "世界".to_string(),
        })
        .unwrap();
        apply_edit_to_crdt(
            &mut crdt,
            &Edit::Replace {
                range: 7..12,
                text: "世界".to_string(),
            },
        )
        .unwrap();
        assert_eq!(crdt.text(), "你好 世界");
        assert_eq!(crdt.text(), doc.text());
    }

    #[test]
    fn sync_toggle_inline_resolved() {
        // ToggleInline 在 Document::apply 里 resolve 成 Replace；
        // 同步时传入 resolve 后的基础编辑即可。
        let mut doc = Document::new("hello");
        let mut crdt = document_to_crdt(&doc);
        let resolved = doc.resolve(Edit::ToggleInline {
            range: 0..5,
            mark: crate::InlineMark::Strong,
        });
        doc.apply(resolved.clone()).unwrap();
        apply_edit_to_crdt(&mut crdt, &resolved).unwrap();
        assert_eq!(crdt.text(), doc.text());
    }

    #[test]
    fn remote_update_rebuilds_document() {
        // 模拟双端：从同一 CRDT 状态派生两个副本（fork），A 本地编辑 →
        // 增量帧 → B 远端应用并重建。
        let mut crdt_a = document_to_crdt(&Document::new("hello world"));
        let mut crdt_b = crdt_a.fork();
        let mut a = crdt_to_document(&crdt_a);
        let mut b = crdt_to_document(&crdt_b);
        assert_eq!(a.text(), b.text());

        // A 端编辑（含 CJK，验证字节→字符转换）
        a.apply_and_sync(
            crate::Edit::Insert {
                at: 5,
                text: " 你好".to_string(),
            },
            &mut crdt_a,
        )
        .unwrap();

        // B 端状态向量 → A 端增量 → B 端应用
        let sv_b = crdt_b.state_vector();
        let diff = crdt_a.encode_diff(&sv_b).unwrap();
        let changed = apply_remote(&mut b, &mut crdt_b, &diff).unwrap();
        assert!(changed, "应有内容变化");
        assert_eq!(a.text(), b.text(), "双端应收敛");
        assert_eq!(b.text(), "hello 你好 world");

        // 再次应用同一帧应无变化
        let again = apply_remote(&mut b, &mut crdt_b, &diff).unwrap();
        assert!(!again, "重复应用不应产生变化");
    }

    #[test]
    fn remote_update_with_bad_bytes_errors() {
        let mut doc = Document::new("x");
        let mut crdt = document_to_crdt(&doc);
        // 构造一个非法的更新帧（0xFF 不是合法的 v1 编码）
        assert!(apply_remote(&mut doc, &mut crdt, &[0xFF, 0xFF]).is_err());
        assert_eq!(doc.text(), "x", "失败不应修改文档");
    }

    #[test]
    fn save_and_load_crdt_roundtrip() {
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let ws = crate::Workspace::new(mazha_store::FileSystemProvider::open(dir.path()).unwrap());
        let path = DocPath::new(".mazha/note.yrs").unwrap();

        // {} 保存
        let mut doc = Document::new("# 标题");
        let mut crdt = document_to_crdt(&doc);
        doc.apply_and_sync(
            Edit::Insert {
                at: 0,
                text: "> ".to_string(),
            },
            &mut crdt,
        )
        .unwrap();
        save_crdt(&ws, &path, &crdt).unwrap();

        // 模拟重开应用：加载后继续编辑，状态应延续
        let mut loaded = load_crdt(&ws, &path).unwrap();
        assert_eq!(loaded.text(), crdt.text());
        assert_eq!(crdt_to_document(&loaded).text(), "> # 标题");

        // 加载后的文档继续演进并与原副本收敛
        loaded.insert(6, " 新增").unwrap();
        let diff = loaded.encode_diff(&crdt.state_vector()).unwrap();
        crdt.apply_update(&diff).unwrap();
        assert_eq!(crdt.text(), loaded.text());
    }

    #[test]
    fn load_crdt_missing_file_errors() {
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let ws = crate::Workspace::new(mazha_store::FileSystemProvider::open(dir.path()).unwrap());
        let path = DocPath::new(".mazha/none.yrs").unwrap();
        assert!(matches!(
            load_crdt(&ws, &path),
            Err(CrdtSyncError::Store(StoreError::NotFound(_)))
        ));
    }

    #[test]
    fn load_crdt_corrupt_file_errors() {
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let ws = crate::Workspace::new(mazha_store::FileSystemProvider::open(dir.path()).unwrap());
        let path = DocPath::new(".mazha/bad.yrs").unwrap();
        ws.store().write(&path, &[0xFF, 0xFF, 0x00]).unwrap();
        assert!(matches!(load_crdt(&ws, &path), Err(CrdtSyncError::Crdt(_))));
    }

    /// D103：远端帧不再整体重建 `Document`，undo/redo 语义必须保持。
    #[test]
    fn apply_remote_keeps_undo_history_and_remaps_offsets() {
        let mut doc = Document::new("你好世界");
        let mut local = document_to_crdt(&doc);
        // 本地先有一条可撤销编辑（在 "你好" 后插入 "A"），并同步进本地 CRDT。
        let edit = Edit::Insert {
            at: 6,
            text: "A".to_owned(),
        };
        doc.apply(edit.clone()).unwrap();
        apply_edit_to_crdt(&mut local, &edit).unwrap();
        assert!(doc.can_undo());

        // 远端副本在文首插入中文：历史里 "A" 的偏移必须随之平移 3 字节。
        let mut remote = local.fork();
        remote.insert(0, "前").unwrap();
        let bytes = remote.encode_diff(&local.state_vector()).unwrap();
        let outcome = apply_remote_detailed(&mut doc, &mut local, &bytes).unwrap();
        assert!(outcome.changed);
        let patch = outcome.patch.expect("变化必须给出补丁");
        assert_eq!(patch.remap_offset(7), 10);
        assert_eq!(doc.text(), "前你好A世界");

        // 远端变更不入栈：undo 只撤销本地插入，而不是撤销远端插入。
        doc.undo().unwrap();
        assert_eq!(doc.text(), "前你好世界");
        assert!(!doc.can_undo());
        // 撤销后产生 redo 项；再应用远端变更时 redo 必须清空。
        assert!(doc.can_redo());
        remote.insert(remote.text().chars().count(), "？").unwrap();
        let more = remote.encode_diff(&local.state_vector()).unwrap();
        assert!(apply_remote(&mut doc, &mut local, &more).unwrap());
        assert!(!doc.can_redo());
    }

    /// D103：无变化的远端帧不能触动文本、undo 与 redo（含空文档边界）。
    #[test]
    fn apply_remote_no_change_is_noop() {
        let mut doc = Document::new("");
        let mut remote = document_to_crdt(&doc);
        doc.apply(Edit::Insert {
            at: 0,
            text: "甲".to_owned(),
        })
        .unwrap();
        doc.undo().unwrap();
        assert!(doc.can_redo());
        let bytes = remote.encode_update();
        let outcome = apply_remote_detailed(&mut doc, &mut remote, &bytes).unwrap();
        assert!(!outcome.changed);
        assert!(outcome.patch.is_none());
        assert_eq!(doc.text(), "");
        assert!(doc.can_redo());
        let bytes = remote.encode_update();
        assert!(!apply_remote(&mut doc, &mut remote, &bytes).unwrap());
    }

    /// D103：CJK 前后缀、纯插入/删除、全替换都走同一补丁路径且文本与 CRDT 收敛。
    #[test]
    fn apply_remote_patch_replays_cjk_and_replace_cases() {
        let mut doc = Document::new("前缀hello后缀");
        let mut local = document_to_crdt(&doc);
        let mut remote = local.fork();
        remote.replace(2, 5, "世界🌿").unwrap();
        let bytes = remote.encode_diff(&local.state_vector()).unwrap();
        let outcome = apply_remote_detailed(&mut doc, &mut local, &bytes).unwrap();
        assert_eq!(doc.text(), "前缀世界🌿后缀");
        assert_eq!(outcome.patch.unwrap().removed, "hello");

        // 删除 CJK 片段：补丁的 removed 必须是完整字符，不能切在字节中间。
        remote.delete(2, 2).unwrap();
        let bytes = remote.encode_diff(&local.state_vector()).unwrap();
        let outcome = apply_remote_detailed(&mut doc, &mut local, &bytes).unwrap();
        assert_eq!(outcome.patch.unwrap().removed, "世界");
        assert_eq!(doc.text(), "前缀🌿后缀");
    }
}
