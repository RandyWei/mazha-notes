//! 本地文件 ⇄ 名册帧的纯函数映射（D101，契约见 `docs/SYNC-SPEC.md` §5.9.2）。
//!
//! 本模块不读盘、不握手、不写文件：只做路径过滤、文本 ⇄ Yjs v1 帧转换、
//! 名册差异 → 本地写回动作、回声字节比对与监听事件合并。所有函数都可独立单测。

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use mazha_crdt::{CrdtDoc, CrdtError, DocId};
use mazha_store::{DocPath, WatchEvent};
use mazha_sync::DocFrameMap;

/// 引擎对本地文件要执行的写回动作（远端变更 → 本地）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalWriteAction {
    /// 把 UTF-8 字节写回工作区相对路径。
    Write {
        /// 工作区相对文档 ID（即路径）。
        doc_id: DocId,
        /// 直接来自 `CrdtDoc::text()` 的 UTF-8 字节（保留 CJK/CRLF，不重排 Markdown）。
        bytes: Vec<u8>,
    },
    /// 删除本地文件（远端墓碑到达）。
    Remove {
        /// 工作区相对文档 ID（即路径）。
        doc_id: DocId,
    },
}

/// 观察到的磁盘状态：供回声抑制做字节比对。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservedBytes<'a> {
    /// 文件存在，内容为这些字节。
    Present(&'a [u8]),
    /// 文件不存在。
    Missing,
}

/// 路径任一段是否以 `.` 开头（隐藏文件/目录一律不参与同步）。
fn has_hidden_segment(path: &str) -> bool {
    path.split('/').any(|segment| segment.starts_with('.'))
}

/// 是否为可同步的 Markdown 文档。
///
/// 只接受 `.md`/`.markdown`（不区分大小写）且**任一路径段都不以 `.` 开头**；
/// `.mazha/`、`.obsidian/`、`.hidden.md` 等内部/隐藏文件全部忽略。控制字符也在
/// 这里挡掉，保证后续 `DocId` 始终满足 CRDT 的 ID 校验规则。
#[must_use]
pub fn is_syncable_md(path: &DocPath) -> bool {
    let raw = path.as_str();
    path.is_markdown() && !has_hidden_segment(raw) && !raw.chars().any(char::is_control)
}

/// 把工作区内的绝对路径转为稳定 `DocId`（工作区相对路径，`/` 分隔）。
///
/// 越界、非 UTF-8、非法 `DocPath`、隐藏路径与非 Markdown 一律返回 `None`。
/// Windows 上 `notify` 报告反斜杠路径，需按 `DocPath` 约定归一化为 `/`。
#[must_use]
pub fn doc_id_from_path(root: &Path, absolute: &Path) -> Option<DocId> {
    let relative = absolute.strip_prefix(root).ok()?;
    let raw = relative.to_str()?;
    #[cfg(windows)]
    let normalized = raw.replace('\\', "/");
    #[cfg(not(windows))]
    let normalized = raw.to_owned();
    let path = DocPath::new(normalized).ok()?;
    if !is_syncable_md(&path) {
        return None;
    }
    Some(path.as_str().to_owned())
}

/// 把本地 UTF-8 文本转换为标准 Yjs v1 全量更新帧。
///
/// `base` 是名册中现有 `CrdtDoc` 的引用（引擎会先 `fork`），在其上整段 `replace`
/// 再 `encode_update`，因此保留原 CRDT client id，不会把整篇文本当作新文档合并；
/// `base` 为 `None`（新建或墓碑已删除）时用 `CrdtDoc::from_text` 生成自包含全量帧。
/// 返回的映射只有 `doc_id` 一个键，交给 `SyncSession::stage_local_change` 入队。
///
/// # Errors
/// `doc_id` 非法或文本替换/编码失败时返回 [`CrdtError`]。
pub fn local_text_to_frames(
    doc_id: &str,
    base: Option<&CrdtDoc>,
    text: &str,
) -> Result<DocFrameMap, CrdtError> {
    let frame = match base {
        Some(doc) => {
            // fork 保留完整 CRDT 历史；直接在原文档上改会污染调用方持有的名册。
            let mut forked = doc.fork();
            let len = forked.text().chars().count();
            forked.replace(0, len, text)?;
            forked.encode_update()
        }
        None => CrdtDoc::from_text(text).encode_update(),
    };
    let mut frames = BTreeMap::new();
    frames.insert(doc_id.to_owned(), frame);
    Ok(frames)
}

/// 比较 `sync_once` 前后的名册文本与墓碑，产出需要落到本地文件的动作。
///
/// - 新文档或文本变化 → [`LocalWriteAction::Write`]；
/// - 之前墓碑化、现在复活（文本可能相同）→ 也写回；
/// - 活动文档消失且之前不是墓碑 → [`LocalWriteAction::Remove`]；
/// - 前后都是墓碑 → 不动（文件早已删除）。
///
/// 用 `BTreeMap` 收集保证动作顺序确定，便于测试与重复回放。
#[must_use]
pub fn remote_frames_to_local_writes(
    before: &BTreeMap<DocId, String>,
    after: &BTreeMap<DocId, String>,
    before_tombstones: &BTreeSet<DocId>,
) -> Vec<LocalWriteAction> {
    let mut actions: BTreeMap<DocId, LocalWriteAction> = BTreeMap::new();
    for (doc_id, text) in after {
        if before.get(doc_id) != Some(text) || before_tombstones.contains(doc_id) {
            actions.insert(
                doc_id.clone(),
                LocalWriteAction::Write {
                    doc_id: doc_id.clone(),
                    bytes: text.as_bytes().to_vec(),
                },
            );
        }
    }
    for doc_id in before.keys() {
        if !after.contains_key(doc_id) && !before_tombstones.contains(doc_id) {
            actions.insert(
                doc_id.clone(),
                LocalWriteAction::Remove {
                    doc_id: doc_id.clone(),
                },
            );
        }
    }
    actions.into_values().collect()
}

/// 磁盘观察值是否与「刚写回/刚删除」标记一致。
///
/// `Some(bytes)` 只与相同的 `Present(bytes)` 匹配，`None` 只与 `Missing` 匹配；
/// 其余情况都不是回声，必须当真实本地编辑处理。命中后由引擎**消费标记**，
/// 否则后续真实编辑会被永久忽略。
#[must_use]
pub fn should_ignore_echo(
    doc_id: &str,
    observed: ObservedBytes<'_>,
    marks: &BTreeMap<DocId, Option<Vec<u8>>>,
) -> bool {
    match (marks.get(doc_id), observed) {
        (Some(Some(expected)), ObservedBytes::Present(actual)) => expected.as_slice() == actual,
        (Some(None), ObservedBytes::Missing) => true,
        _ => false,
    }
}

/// 把一批监听事件合并成「需要重新检查的文档 ID」集合（只标脏，不读盘）。
///
/// 创建/修改/重命名目标标为待检查；删除/重命名来源也标为待检查——引擎读盘发现
/// 文件已不存在时会走 `stage_delete`，因此无需在此区分脏类型。隐藏路径与非
/// Markdown 在入口即过滤。
#[must_use]
pub fn merge_watch_events(events: &[WatchEvent]) -> BTreeSet<DocId> {
    let mut dirty = BTreeSet::new();
    for event in events {
        match event {
            WatchEvent::Created { path }
            | WatchEvent::Modified { path }
            | WatchEvent::Removed { path } => {
                if is_syncable_md(path) {
                    dirty.insert(path.as_str().to_owned());
                }
            }
            WatchEvent::Renamed { from, to } => {
                // 旧路径读盘会消失 → stage_delete；新路径存在 → stage_text。
                if is_syncable_md(from) {
                    dirty.insert(from.as_str().to_owned());
                }
                if is_syncable_md(to) {
                    dirty.insert(to.as_str().to_owned());
                }
            }
        }
    }
    dirty
}

#[cfg(test)]
mod tests {
    use super::*;
    use mazha_crdt::CrdtDoc;
    use std::path::PathBuf;

    #[test]
    fn is_syncable_md_filters_hidden_and_non_markdown() {
        for raw in ["a.md", "笔记/B.MARKDOWN", "notes/a.markdown"] {
            assert!(is_syncable_md(&DocPath::new(raw).unwrap()), "{raw}");
        }
        for raw in [
            "a.txt",
            ".hidden.md",
            ".mazha/sync-state.bin",
            ".obsidian/a.md",
            "笔记/.draft/a.md",
            "a\n.md",
        ] {
            assert!(!is_syncable_md(&DocPath::new(raw).unwrap()), "{raw}");
        }
    }

    #[test]
    fn doc_id_from_path_rejects_outside_hidden_and_non_markdown() {
        let root = PathBuf::from("/工作区");
        assert_eq!(
            doc_id_from_path(&root, &root.join("笔记/甲.md")).as_deref(),
            Some("笔记/甲.md")
        );
        assert_eq!(
            doc_id_from_path(
                &root,
                &root
                    .join("外部/甲.md")
                    .parent()
                    .unwrap()
                    .join("../../甲.md")
            ),
            None
        );
        assert_eq!(doc_id_from_path(&root, &root.join(".mazha/a.md")), None);
        assert_eq!(doc_id_from_path(&root, &root.join("a.txt")), None);
    }

    #[test]
    fn local_text_to_frames_preserves_cjk_crlf_and_base_identity() {
        let base = CrdtDoc::from_text("原文\r\n第二行");
        let frames = local_text_to_frames("甲.md", Some(&base), "原文\r\n改动").unwrap();
        let mut rebuilt = base.fork();
        rebuilt.apply_update(&frames["甲.md"]).unwrap();
        assert_eq!(rebuilt.text(), "原文\r\n改动");

        let fresh = local_text_to_frames("乙.md", None, "你好🌿").unwrap();
        let mut empty = CrdtDoc::new();
        empty.apply_update(&fresh["乙.md"]).unwrap();
        assert_eq!(empty.text(), "你好🌿");
    }

    #[test]
    fn remote_writes_detect_add_change_remove_and_revive() {
        let before = BTreeMap::from([
            ("a.md".to_owned(), "原文".to_owned()),
            ("gone.md".to_owned(), "旧文".to_owned()),
        ]);
        let after = BTreeMap::from([
            ("a.md".to_owned(), "原文".to_owned()),
            ("new.md".to_owned(), "新文".to_owned()),
            ("revived.md".to_owned(), "复活".to_owned()),
        ]);
        let tombstones = BTreeSet::from(["revived.md".to_owned()]);
        let actions = remote_frames_to_local_writes(&before, &after, &tombstones);
        assert_eq!(
            actions,
            vec![
                LocalWriteAction::Remove {
                    doc_id: "gone.md".to_owned(),
                },
                LocalWriteAction::Write {
                    doc_id: "new.md".to_owned(),
                    bytes: "新文".as_bytes().to_vec(),
                },
                LocalWriteAction::Write {
                    doc_id: "revived.md".to_owned(),
                    bytes: "复活".as_bytes().to_vec(),
                },
            ]
        );
    }

    #[test]
    fn echo_matches_only_identical_presence_or_missing() {
        let marks = BTreeMap::from([
            ("a.md".to_owned(), Some("内容".as_bytes().to_vec())),
            ("b.md".to_owned(), None),
        ]);
        assert!(should_ignore_echo(
            "a.md",
            ObservedBytes::Present("内容".as_bytes()),
            &marks
        ));
        assert!(!should_ignore_echo(
            "a.md",
            ObservedBytes::Present("改动".as_bytes()),
            &marks
        ));
        assert!(should_ignore_echo("b.md", ObservedBytes::Missing, &marks));
        assert!(!should_ignore_echo(
            "b.md",
            ObservedBytes::Present("内容".as_bytes()),
            &marks
        ));
        assert!(!should_ignore_echo("c.md", ObservedBytes::Missing, &marks));
    }

    #[test]
    fn merge_watch_events_handles_rename_and_filters_hidden() {
        let events = vec![
            WatchEvent::Created {
                path: DocPath::new("b.md").unwrap(),
            },
            WatchEvent::Modified {
                path: DocPath::new("b.md").unwrap(),
            },
            WatchEvent::Renamed {
                from: DocPath::new("old.md").unwrap(),
                to: DocPath::new("new.md").unwrap(),
            },
            WatchEvent::Removed {
                path: DocPath::new(".hidden.md").unwrap(),
            },
            WatchEvent::Created {
                path: DocPath::new("notes/x.txt").unwrap(),
            },
        ];
        let dirty = merge_watch_events(&events);
        assert_eq!(
            dirty,
            BTreeSet::from(["b.md".to_owned(), "new.md".to_owned(), "old.md".to_owned(),])
        );
    }
}
