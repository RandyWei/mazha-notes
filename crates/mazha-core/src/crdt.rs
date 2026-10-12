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
//! - 远端更新：远端更新应用到 `CrdtDoc` 后，用 [`crdt_to_document`] 把源文本
//!   重新载入 `Document`（MVP 采用"整体重建"策略，不做字符级合并）。

use crate::{Document, Edit};
use mazha_crdt::CrdtDoc;

/// 从 `Document` 的当前源文本创建一份 `CrdtDoc`（用于初次上云/持久化）。
pub fn document_to_crdt(doc: &Document) -> CrdtDoc {
    CrdtDoc::from_text(&doc.text())
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

/// 应用一个远端更新帧：先并入 `crdt`，若源文本变化则重建 `doc`。
///
/// 这是 MVP 的"远端 → 本地"通道：`bytes` 为 Yjs v1 编码的更新（由对端的
/// [`mazha_crdt::CrdtDoc::encode_diff`] / `encode_update` 产出）。返回是否发生实际变化
/// （无变化时不会触动本地 undo/redo 历史）。
///
/// # Errors
/// 更新帧解码失败时返回 [`mazha_crdt::CrdtError`]。
pub fn apply_remote(
    doc: &mut Document,
    crdt: &mut CrdtDoc,
    bytes: &[u8],
) -> mazha_crdt::CrdtResult<bool> {
    crdt.apply_update(bytes)?;
    let latest = crdt.text();
    if latest == doc.text() {
        return Ok(false);
    }
    *doc = crdt_to_document(crdt);
    Ok(true)
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
}
