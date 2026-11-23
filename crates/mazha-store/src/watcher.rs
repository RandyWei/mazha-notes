//! 文件监听（SPEC §6.2、D91）：事件模型、纯函数规范化/去重，以及基于 `notify` 的本地实现。
//!
//! 本模块不含 GUI 依赖；监听线程封在内部，`StorageProvider` 对外仍是同步接口（D24 不翻案）。

use crate::error::StoreResult;
use crate::path::DocPath;
use notify::event::{CreateKind, Event, EventKind, RemoveKind, RenameMode};
use notify::{RecursiveMode, Watcher as NotifyWatcher};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// 去重窗口：同一路径在该时长内的重复修改只上报首次（D91）。
pub const COALESCE_WINDOW: Duration = Duration::from_millis(150);

/// 规范化事件种类（对应 [`WatchEvent`] 的四个变体）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WatchEventKind {
    /// 新建文件。
    Create,
    /// 文件内容或元数据修改。
    Modify,
    /// 文件删除。
    Remove,
    /// 重命名/移动（含旧新路径）。
    Rename,
}

/// 规范化后的工作区文件事件。
///
/// 所有路径都是工作区相对的 [`DocPath`]，且只可能是 Markdown 文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    /// 文件被创建。
    Created {
        /// 新文件路径。
        path: DocPath,
    },
    /// 文件内容或元数据被修改。
    Modified {
        /// 被修改的文件路径。
        path: DocPath,
    },
    /// 文件被删除。
    Removed {
        /// 被删除的文件路径。
        path: DocPath,
    },
    /// 文件被重命名/移动（仅 notify 同时给出旧/新路径时产生）。
    Renamed {
        /// 重命名前的路径。
        from: DocPath,
        /// 重命名后的路径。
        to: DocPath,
    },
}

impl WatchEvent {
    /// 事件的粗粒度种类。
    pub fn kind(&self) -> WatchEventKind {
        match self {
            Self::Created { .. } => WatchEventKind::Create,
            Self::Modified { .. } => WatchEventKind::Modify,
            Self::Removed { .. } => WatchEventKind::Remove,
            Self::Renamed { .. } => WatchEventKind::Rename,
        }
    }

    /// 事件关注的文件路径（Rename 取新路径 `to`，便于上层按当前路径索引）。
    pub fn path(&self) -> &DocPath {
        match self {
            Self::Created { path }
            | Self::Modified { path }
            | Self::Removed { path }
            | Self::Renamed { to: path, .. } => path,
        }
    }
}

/// 监听句柄：可安全跨线程持有；**drop 即停机**并释放 OS watch（D91）。
pub trait WatchHandle: Send {
    /// 阻塞等待下一个事件；监听停止或通道关闭时返回 `None`。
    fn recv(&self) -> Option<WatchEvent>;

    /// 非阻塞尝试取一个事件；暂无事件或已停止时返回 `None`。
    fn try_recv(&self) -> Option<WatchEvent>;
}

/// 把 notify 报告的路径转成工作区相对 [`DocPath`]。
///
/// 工作区外、非 UTF-8、含非法段的路径一律返回 `None`（`strip_prefix` 同时挡住 `..` 逃逸）。
fn to_doc_path(root: &Path, path: &Path) -> Option<DocPath> {
    let rel = path.strip_prefix(root).ok()?;
    let raw = rel.to_str()?;
    // Windows 上 notify 用 `\` 分隔，而 DocPath 统一用 `/`；Unix 文件名可含 `\`，不能替换
    #[cfg(windows)]
    let normalized = raw.replace('\\', "/");
    #[cfg(not(windows))]
    let normalized = raw.to_owned();
    DocPath::new(normalized).ok()
}

/// 单个路径事件的映射：先过滤目录与非 Markdown，再按种类产出零或一个事件。
fn map_single_event(root: &Path, path: &Path, kind: WatchEventKind) -> Option<WatchEvent> {
    let doc = to_doc_path(root, path)?;
    // 目录事件与无关文件一律忽略：只上报 Markdown 文档（D91）
    if !doc.is_markdown() {
        return None;
    }
    Some(match kind {
        WatchEventKind::Create => WatchEvent::Created { path: doc },
        WatchEventKind::Modify => WatchEvent::Modified { path: doc },
        WatchEventKind::Remove => WatchEvent::Removed { path: doc },
        // 配对的重命名由 map_event 单独处理；单个 From/To 已在此前降级
        WatchEventKind::Rename => WatchEvent::Renamed {
            from: doc.clone(),
            to: doc,
        },
    })
}

/// 把一个 notify 事件规范化为零到多个工作区事件（纯函数，无 IO/线程）。
///
/// - `Create` → [`WatchEvent::Created`]；`Remove` → [`WatchEvent::Removed`]；
/// - `Modify::{Data,Metadata,Any,…}` 与 `EventKind::Any` → [`WatchEvent::Modified`]；
/// - `Modify::Name(Both)` → [`WatchEvent::Renamed`]（旧→新）；`From`/`To` 因拿不到成对路径，
///   分别降级为 Removed / Created，不跨事件猜测配对（D91）；
/// - `Access`/`Other`、目录、非 Markdown、非 UTF-8、工作区外路径被忽略。
pub fn normalize_watch_events(root: &Path, event: &Event) -> Vec<WatchEvent> {
    match event.kind {
        // 重命名同时给出旧/新路径：notify 约定顺序为 (from, to)
        EventKind::Modify(notify::event::ModifyKind::Name(RenameMode::Both))
            if event.paths.len() >= 2 =>
        {
            let from = to_doc_path(root, &event.paths[0]);
            let to = to_doc_path(root, &event.paths[1]);
            match (from, to) {
                (Some(from), Some(to)) if from.is_markdown() && to.is_markdown() => {
                    vec![WatchEvent::Renamed { from, to }]
                }
                // 只有一端是 Markdown：另一端对编辑器不可见，按消失/出现上报
                (Some(from), _) if from.is_markdown() => {
                    vec![WatchEvent::Removed { path: from }]
                }
                (_, Some(to)) if to.is_markdown() => vec![WatchEvent::Created { path: to }],
                _ => Vec::new(),
            }
        }
        EventKind::Modify(notify::event::ModifyKind::Name(RenameMode::From)) => event
            .paths
            .iter()
            .filter_map(|p| map_single_event(root, p, WatchEventKind::Remove))
            .collect(),
        EventKind::Modify(notify::event::ModifyKind::Name(RenameMode::To)) => event
            .paths
            .iter()
            .filter_map(|p| map_single_event(root, p, WatchEventKind::Create))
            .collect(),
        // 显式目录事件忽略（即使目录名带 .md 后缀也不能当成文档）；
        // Access/Other 不表示内容变化。其余 Name 变体（Any/Other）落入下面的
        // Modify(_) 作为普通修改上报，避免漏掉可能的重命名。
        EventKind::Create(CreateKind::Folder)
        | EventKind::Remove(RemoveKind::Folder)
        | EventKind::Access(_)
        | EventKind::Other => Vec::new(),
        EventKind::Create(_) => event
            .paths
            .iter()
            .filter_map(|p| map_single_event(root, p, WatchEventKind::Create))
            .collect(),
        EventKind::Modify(_) | EventKind::Any => event
            .paths
            .iter()
            .filter_map(|p| map_single_event(root, p, WatchEventKind::Modify))
            .collect(),
        EventKind::Remove(_) => event
            .paths
            .iter()
            .filter_map(|p| map_single_event(root, p, WatchEventKind::Remove))
            .collect(),
    }
}

/// 按到达顺序折叠同一路径短时间内的重复事件（纯逻辑状态机，无线程/IO）。
struct WatchCoalescer {
    /// 每个路径最近一次**已发出**事件的时刻与种类；Rename 以 `to` 为键。
    last: HashMap<DocPath, (Instant, WatchEventKind)>,
    window: Duration,
}

impl WatchCoalescer {
    /// 以给定窗口创建合并器。
    fn new(window: Duration) -> Self {
        Self {
            last: HashMap::new(),
            window,
        }
    }

    /// 接受一个带时间戳的事件；`None` 表示被折叠丢弃，`Some` 表示应投递。
    fn push(&mut self, at: Instant, event: WatchEvent) -> Option<WatchEvent> {
        let key = event.path().clone();
        let kind = event.kind();
        if kind == WatchEventKind::Modify
            && let Some((prev_at, prev_kind)) = self.last.get(&key)
        {
            // 删除后的修改必须上报（文件确实又出现了）；其余重复修改只保留首次
            if *prev_kind != WatchEventKind::Remove
                && at.saturating_duration_since(*prev_at) <= self.window
            {
                return None;
            }
        }
        self.last.insert(key, (at, kind));
        Some(event)
    }
}

/// 纯函数版事件合并：按到达顺序折叠 `window` 内同一路径的重复修改。
///
/// 首次发出的事件优先（例如新建后紧随的写入只保留 `Created`）；删除后的修改、
/// 重命名本身与窗口外的修改原样透传。内部监听线程用同一规则的 [`WatchCoalescer`] 增量执行。
pub fn coalesce_watch_events(
    events: &[(Instant, WatchEvent)],
    window: Duration,
) -> Vec<WatchEvent> {
    let mut coalescer = WatchCoalescer::new(window);
    events
        .iter()
        .filter_map(|(at, event)| coalescer.push(*at, event.clone()))
        .collect()
}

/// `notify` 后端驱动的内部监听线程句柄。
struct NotifyWatchHandle {
    /// 已规范化、已去重的事件接收端。
    events: Receiver<WatchEvent>,
    /// 停机信号发送端（句柄 drop 时触发）。
    stop: mpsc::Sender<()>,
    /// 持有 `notify::RecommendedWatcher` 的工作线程；join 保证资源真正释放。
    worker: Option<JoinHandle<()>>,
}

impl WatchHandle for NotifyWatchHandle {
    fn recv(&self) -> Option<WatchEvent> {
        // 工作线程退出时会 drop 发送端，阻塞的 recv 随之返回 Err → None
        self.events.recv().ok()
    }

    fn try_recv(&self) -> Option<WatchEvent> {
        self.events.try_recv().ok()
    }
}

impl Drop for NotifyWatchHandle {
    fn drop(&mut self) {
        // 先通知线程停机（线程退出时 drop watcher 释放 OS watch），再 join 保证不留后台线程
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// 启动递归本地监听：内部线程收 notify 事件 → 打时间戳 → 规范化 → 合并 → 投递。
///
/// `root` 是工作区根（用于把事件路径转为相对 `DocPath`），`watch_path` 是实际注册的监听目录，
/// 二者可以不同（监听子目录时事件路径仍相对工作区根）。
///
/// # Errors
/// `notify` 后端创建、注册监听或工作线程启动失败时返回 [`crate::StoreError::Watch`]。
pub(crate) fn spawn_watch(root: PathBuf, watch_path: &Path) -> StoreResult<Box<dyn WatchHandle>> {
    let (raw_tx, raw_rx) = mpsc::channel::<(Instant, WatchEvent)>();
    let (event_tx, event_rx) = mpsc::sync_channel::<WatchEvent>(256);
    let (stop_tx, stop_rx) = mpsc::channel::<()>();

    let mut watcher = notify::recommended_watcher(move |result: notify::Result<Event>| {
        // 回调运行在 notify 内部线程：只做纯规范化与时间戳；运行期后端错误不入事件流（D91）
        if let Ok(event) = result {
            let now = Instant::now();
            for normalized in normalize_watch_events(&root, &event) {
                if raw_tx.send((now, normalized)).is_err() {
                    break;
                }
            }
        }
    })
    .map_err(|e| crate::StoreError::Watch(e.to_string()))?;

    watcher
        .watch(watch_path, RecursiveMode::Recursive)
        .map_err(|e| crate::StoreError::Watch(e.to_string()))?;

    let worker = thread::Builder::new()
        .name("mazha-store-watch".to_owned())
        .spawn(move || {
            // watcher 随线程存活；线程退出（停机/接收端关闭）即释放 OS watch
            let _watcher = watcher;
            let mut coalescer = WatchCoalescer::new(COALESCE_WINDOW);
            loop {
                // 周期性检查停机信号，避免事件密集时无法响应 drop
                match stop_rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                while let Ok((at, event)) = raw_rx.try_recv() {
                    if let Some(emitted) = coalescer.push(at, event) {
                        // 接收端已 drop 则立即退出，不阻塞停机
                        if send_event(&event_tx, emitted) {
                            return;
                        }
                    }
                }
            }
        })
        .map_err(|e| crate::StoreError::Watch(e.to_string()))?;

    Ok(Box::new(NotifyWatchHandle {
        events: event_rx,
        stop: stop_tx,
        worker: Some(worker),
    }))
}

/// 向接收端投事件；返回 `true` 表示通道已关闭（调用方应结束线程）。
fn send_event(tx: &SyncSender<WatchEvent>, event: WatchEvent) -> bool {
    tx.send(event).is_err()
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, DataChange, EventAttributes, ModifyKind, RemoveKind};
    use std::path::PathBuf;

    /// 构造一条 notify 事件（只填 kind/paths，其余属性用默认值）。
    fn event(kind: EventKind, paths: &[&str]) -> Event {
        Event {
            kind,
            paths: paths.iter().map(PathBuf::from).collect(),
            attrs: EventAttributes::new(),
        }
    }

    fn md(kind: WatchEventKind, path: &str) -> (Instant, WatchEvent) {
        let doc = DocPath::new(path).unwrap();
        let event = match kind {
            WatchEventKind::Create => WatchEvent::Created { path: doc },
            WatchEventKind::Modify => WatchEvent::Modified { path: doc },
            WatchEventKind::Remove => WatchEvent::Removed { path: doc },
            WatchEventKind::Rename => WatchEvent::Renamed {
                from: DocPath::new("old.md").unwrap(),
                to: doc,
            },
        };
        (Instant::now(), event)
    }

    #[test]
    fn normalizes_create_modify_and_remove() {
        let root = Path::new("/ws");
        assert_eq!(
            normalize_watch_events(
                root,
                &event(EventKind::Create(CreateKind::File), &["/ws/notes/a.md"])
            ),
            vec![WatchEvent::Created {
                path: DocPath::new("notes/a.md").unwrap()
            }]
        );
        assert_eq!(
            normalize_watch_events(
                root,
                &event(
                    EventKind::Modify(ModifyKind::Data(DataChange::Content)),
                    &["/ws/notes/A.MARKDOWN"]
                )
            ),
            vec![WatchEvent::Modified {
                path: DocPath::new("notes/A.MARKDOWN").unwrap()
            }]
        );
        assert_eq!(
            normalize_watch_events(
                root,
                &event(EventKind::Remove(RemoveKind::File), &["/ws/a.md"])
            ),
            vec![WatchEvent::Removed {
                path: DocPath::new("a.md").unwrap()
            }]
        );
    }

    #[test]
    fn ignores_non_markdown_dirs_access_and_outside_root() {
        let root = Path::new("/ws");
        assert!(
            normalize_watch_events(
                root,
                &event(EventKind::Create(CreateKind::File), &["/ws/x.txt"])
            )
            .is_empty()
        );
        assert!(
            normalize_watch_events(
                root,
                &event(EventKind::Create(CreateKind::Folder), &["/ws/dir.md"])
            )
            .is_empty()
        );
        assert!(
            normalize_watch_events(
                root,
                &event(
                    EventKind::Access(notify::event::AccessKind::Read),
                    &["/ws/a.md"]
                )
            )
            .is_empty()
        );
        assert!(
            normalize_watch_events(
                root,
                &event(EventKind::Create(CreateKind::File), &["/elsewhere/a.md"])
            )
            .is_empty()
        );
    }

    #[test]
    fn normalizes_rename_with_both_paths_and_degrades_single_side() {
        let root = Path::new("/ws");
        assert_eq!(
            normalize_watch_events(
                root,
                &event(
                    EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
                    &["/ws/dir/old.md", "/ws/new.md"]
                )
            ),
            vec![WatchEvent::Renamed {
                from: DocPath::new("dir/old.md").unwrap(),
                to: DocPath::new("new.md").unwrap(),
            }]
        );
        // 只有一端是 Markdown：按消失/出现上报，不丢事件
        assert_eq!(
            normalize_watch_events(
                root,
                &event(
                    EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
                    &["/ws/a.md", "/ws/a.txt"]
                )
            ),
            vec![WatchEvent::Removed {
                path: DocPath::new("a.md").unwrap()
            }]
        );
        assert_eq!(
            normalize_watch_events(
                root,
                &event(
                    EventKind::Modify(ModifyKind::Name(RenameMode::To)),
                    &["/ws/new.md"]
                )
            ),
            vec![WatchEvent::Created {
                path: DocPath::new("new.md").unwrap()
            }]
        );
    }

    #[test]
    fn self_writes_are_exposed_as_modified_events() {
        // D91：store 不抑制自身写入产生的事件，上层按脏状态比对过滤
        let events = normalize_watch_events(
            Path::new("/ws"),
            &event(
                EventKind::Modify(ModifyKind::Data(DataChange::Content)),
                &["/ws/a.md"],
            ),
        );
        assert_eq!(
            events,
            vec![WatchEvent::Modified {
                path: DocPath::new("a.md").unwrap()
            }]
        );
    }

    #[test]
    fn coalesces_repeated_modifies_within_window() {
        let base = Instant::now();
        let events = vec![
            (
                base,
                WatchEvent::Modified {
                    path: DocPath::new("a.md").unwrap(),
                },
            ),
            (
                base + Duration::from_millis(20),
                WatchEvent::Modified {
                    path: DocPath::new("a.md").unwrap(),
                },
            ),
            (
                base + Duration::from_millis(160),
                WatchEvent::Modified {
                    path: DocPath::new("a.md").unwrap(),
                },
            ),
        ];
        assert_eq!(coalesce_watch_events(&events, COALESCE_WINDOW).len(), 2);
    }

    #[test]
    fn coalesces_created_followed_by_modify_but_keeps_removed_then_modify() {
        let base = Instant::now();
        let created_then_modified = vec![
            md(WatchEventKind::Create, "a.md"),
            (
                base + Duration::from_millis(10),
                WatchEvent::Modified {
                    path: DocPath::new("a.md").unwrap(),
                },
            ),
        ];
        assert_eq!(
            coalesce_watch_events(&created_then_modified, COALESCE_WINDOW),
            vec![WatchEvent::Created {
                path: DocPath::new("a.md").unwrap()
            }]
        );

        let removed_then_modified = vec![
            md(WatchEventKind::Remove, "a.md"),
            (
                base + Duration::from_millis(10),
                WatchEvent::Modified {
                    path: DocPath::new("a.md").unwrap(),
                },
            ),
        ];
        assert_eq!(
            coalesce_watch_events(&removed_then_modified, COALESCE_WINDOW).len(),
            2
        );
    }

    #[test]
    fn coalescing_is_per_path_and_rename_dedups_on_to_path() {
        let base = Instant::now();
        let events = vec![
            md(WatchEventKind::Modify, "a.md"),
            md(WatchEventKind::Modify, "b.md"),
            (
                base + Duration::from_millis(10),
                WatchEvent::Modified {
                    path: DocPath::new("a.md").unwrap(),
                },
            ),
            md(WatchEventKind::Rename, "new.md"),
            (
                base + Duration::from_millis(20),
                WatchEvent::Modified {
                    path: DocPath::new("new.md").unwrap(),
                },
            ),
        ];
        let out = coalesce_watch_events(&events, COALESCE_WINDOW);
        assert_eq!(out.len(), 3);
        assert_eq!(
            out[2],
            WatchEvent::Renamed {
                from: DocPath::new("old.md").unwrap(),
                to: DocPath::new("new.md").unwrap(),
            }
        );
    }

    #[test]
    fn watch_event_accessors() {
        let renamed = WatchEvent::Renamed {
            from: DocPath::new("old.md").unwrap(),
            to: DocPath::new("new.md").unwrap(),
        };
        assert_eq!(renamed.kind(), WatchEventKind::Rename);
        assert_eq!(renamed.path().as_str(), "new.md");
        assert_eq!(
            md(WatchEventKind::Create, "a.md").1.kind(),
            WatchEventKind::Create
        );
    }
}
