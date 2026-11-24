//! 桌面同步接入（Phase 5 S5a / D102 + S5b 第一批 / D103-D104，契约见
//! `docs/SYNC-SPEC.md` §5.10/§5.11/§9.6/§9.7/§11.4/§11.5）。
//!
//! 本模块分两层：
//! - **纯函数层**：配置解析（URL/ID/间隔/token）、状态与横幅文案、事件转发过滤、
//!   三选一标签与破坏性确认文案；不依赖 GPUI、不做网络，沿用 `main.rs` 的
//!   `#[cfg(test)]` 单测风格锁定契约。
//! - **线程层**：`LocalSyncEngine` + `HttpTransport` 只在专用同步线程创建/调用；
//!   GPUI 主线程只经 `mpsc` 发命令、收状态，UI 回写由 `main.rs` 的
//!   `cx.spawn` + `WeakView::update` 完成，绝不跨线程改 GPUI 状态。
//!
//! 绝不静默覆盖：网络失败保留 outbox 下轮重试；S5b 的“合并/覆盖”必须经横幅三选一，
//! 破坏性选择需二次确认后才经 `SyncHandle` 发命令，引擎结果由主线程回写文档。

use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use mazha_store::{DocPath, FileSystemProvider, StorageProvider, WatchEvent};
use mazha_sync::{DeviceId, WorkspaceId};
use mazha_sync_http::HttpTransport;
use mazha_sync_local::{
    ConflictChoice, LocalSyncEngine, LocalSyncStatus, ResolveOutcome, SnapshotChoice,
    doc_id_from_path, is_syncable_md,
};
use mazha_sync_ws::{
    SubscriptionConfig, SubscriptionError, SyncEvent, SyncSubscription, TcpSubscription,
    subscription_url,
};

use crate::settings::{DEFAULT_SYNC_INTERVAL_SECS, Settings, clamp_sync_interval};
use crate::token_store::{InMemoryTokenStore, resolve_token};

/// token 的默认环境变量名（D97/D109）：secret 只在内存，绝不写入 `settings.json`、日志或命令行。
pub const SYNC_TOKEN_ENV: &str = "MAZHA_SYNC_TOKEN";
/// hostname 也不可用时使用的固定设备回退串（仍经 `DeviceId::new` 校验）。
const DEVICE_FALLBACK: &str = "mazha-device";

/// 同步配置错误：全部转成中文文案给状态栏，绝不 panic。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncSetupError {
    /// 没填服务端地址（这是“未配置”，不是错误）。
    NotConfigured,
    /// 服务端地址不是 `http(s)://` 或缺少 host。
    InvalidServerUrl(String),
    /// 工作区 ID 非法（空/控制字符）。
    InvalidWorkspaceId(String),
    /// 设备 ID 非法（空/控制字符）。
    InvalidDeviceId(String),
    /// 密钥链与环境变量都没有 token（D109 保持原枚举语义）。
    MissingToken,
    /// token 空白/含控制字符/超长（D109）。
    ///
    /// 实际校验发生在 `token_store::validate_token`；该变体保留给未来的配置入口，
    /// 当前生产路径只展示存储层错误，避免重复校验改变既有配置语义。
    #[allow(dead_code)]
    InvalidToken(String),
    /// 工作区 Provider 打不开。
    Provider(String),
    /// 引擎初始化失败（含本地状态文件损坏，必须显性提示）。
    Engine(String),
}

impl SyncSetupError {
    /// 状态栏/横幅用的中文说明。
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::NotConfigured => "未设置服务端地址（settings.json 的 sync_server_url）".into(),
            Self::InvalidServerUrl(url) => {
                format!("同步地址无效：{url}（只支持 http:// 或 https://）")
            }
            Self::InvalidWorkspaceId(id) => format!("同步工作区 ID 无效：{id}"),
            Self::InvalidDeviceId(id) => format!("同步设备 ID 无效：{id}"),
            Self::MissingToken => {
                format!("缺少同步令牌（请存入系统密钥链或设置环境变量 {SYNC_TOKEN_ENV}）")
            }
            Self::InvalidToken(reason) => format!("同步令牌无效：{reason}"),
            Self::Provider(reason) => format!("同步工作区打开失败：{reason}"),
            Self::Engine(reason) => format!("同步引擎启动失败：{reason}"),
        }
    }
}

/// 同步线程的生命周期阶段（UI 状态栏展示用，纯数据）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncPhase {
    /// 无配置/无 token/启动失败，线程未运行。
    NotConfigured {
        /// 具体原因。
        reason: String,
    },
    /// 线程就绪，等待下一轮同步。
    Idle {
        /// 已确认的服务端游标。
        last_cursor: u64,
    },
    /// 正在执行 `sync_once`。
    Running,
    /// 上一轮成功。
    Success {
        /// 已确认的服务端游标。
        last_cursor: u64,
    },
    /// 上一轮失败（离线/服务端错误/快照确认被拒）；线程仍存活，下轮继续重试。
    Failed {
        /// 失败原因（保留底层错误文本）。
        reason: String,
    },
}

/// 解析后的同步配置；`generated_device_id` 非空时调用方应写回 `Settings` 持久化。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncConfig {
    /// 服务端根 URL（已去尾部 `/` 并校验）。
    pub server_url: String,
    /// 工作区身份。
    pub workspace_id: WorkspaceId,
    /// 设备身份。
    pub device_id: DeviceId,
    /// 周期同步间隔。
    pub interval: Duration,
    /// Bearer token（只在内存中，不写盘）。
    pub token: String,
    /// 由 hostname 回退生成、需要写回设置文件的设备 ID。
    pub generated_device_id: Option<String>,
    /// D108：从 http(s) 根 URL 推导出的 ws(s) 订阅 URL。
    pub subscription_url: String,
    /// D109：非阻断诊断（如平台密钥链不可用、env 回退中）。
    pub token_diagnostic: Option<String>,
}

/// 只接受 `http://` / `https://` + 非空 host；路径/查询可比照普通 URL，不作限制。
///
/// 空白与控制字符会让 HTTP header/URL 拼接变得不可控，因此在入口直接拒绝；
/// 生产 TLS 由反向代理终结（D100），本函数不判断证书与端口。
pub fn validate_server_url(url: &str) -> Result<(), SyncSetupError> {
    // 控制字符（含首尾换行）不可被 trim 掩盖：它可能注入 header 或日志，入口直接拒绝。
    if url.chars().any(char::is_control) {
        return Err(SyncSetupError::InvalidServerUrl(url.to_owned()));
    }
    // 仅允许首尾普通空白被忽略；中间的空白仍视为非法 host/URL。
    let url = url.trim();
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or_else(|| SyncSetupError::InvalidServerUrl(url.to_owned()))?;
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if host.is_empty() || url.chars().any(char::is_whitespace) {
        return Err(SyncSetupError::InvalidServerUrl(url.to_owned()));
    }
    Ok(())
}

/// 未配置工作区 ID 时的确定性回退：规范化工作区根路径字符串（CJK 原样保留）。
#[must_use]
pub fn workspace_fallback_id(root: &Path) -> String {
    let normalized = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let raw = normalized
        .to_string_lossy()
        .trim_end_matches(['/', '\\'])
        .to_owned();
    if raw.is_empty() {
        "mazha-workspace".to_owned()
    } else {
        raw
    }
}

/// hostname 回退串的清洗：只保留可安全放进 ID 的字符，非法段换成 `-`。
#[must_use]
pub fn sanitize_device_fallback(raw: &str) -> String {
    let cleaned: String = raw
        .trim()
        .chars()
        .map(|c| {
            if c.is_control() || c.is_whitespace() {
                '-'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('-').to_owned();
    if cleaned.is_empty() {
        DEVICE_FALLBACK.to_owned()
    } else {
        cleaned
    }
}

/// 读取本机 hostname（macOS/Linux 用 `/etc/hostname`，Windows 用环境变量）。
///
/// 只在用户未配置 `sync_device_id` 时调用一次；读不到时退回固定串。
fn hostname() -> String {
    #[cfg(not(target_os = "windows"))]
    if let Ok(raw) = std::fs::read_to_string("/etc/hostname") {
        let raw = raw.trim().to_owned();
        if !raw.is_empty() {
            return sanitize_device_fallback(&raw);
        }
    }
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .map(|raw| sanitize_device_fallback(&raw))
        .unwrap_or_else(|_| DEVICE_FALLBACK.to_owned())
}

/// 把 `Settings` + 工作区根 + token 解析为可启动同步线程的配置（保留原签名供测试）。
///
/// 配置缺失或非法一律返回错误由 UI 提示，不 panic、不用空串硬造 ID。
///
/// 生产 UI 走带诊断的变体；此签名保留供既有测试与兼容调用，不应删除。
#[allow(dead_code)]
pub fn resolve_config(
    settings: &Settings,
    root: &Path,
    token: Option<&str>,
) -> Result<SyncConfig, SyncSetupError> {
    resolve_config_with_diagnostic(settings, root, token, None)
}

/// D108/D109：与 [`resolve_config`] 相同，但携带 token 来源诊断并推导订阅 URL。
///
/// # Errors
/// 配置缺失或非法返回对应 [`SyncSetupError`]；订阅 URL 推导失败视为服务端地址非法。
pub fn resolve_config_with_diagnostic(
    settings: &Settings,
    root: &Path,
    token: Option<&str>,
    token_diagnostic: Option<String>,
) -> Result<SyncConfig, SyncSetupError> {
    let server_url = settings
        .sync_server_url
        .as_deref()
        .ok_or(SyncSetupError::NotConfigured)?;
    validate_server_url(server_url)?;
    let server_url = server_url.trim().trim_end_matches('/').to_owned();
    let workspace_raw = settings
        .sync_workspace_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| workspace_fallback_id(root));
    let workspace_id = WorkspaceId::new(workspace_raw.clone())
        .map_err(|_| SyncSetupError::InvalidWorkspaceId(workspace_raw))?;
    let (device_raw, generated_device_id) = match settings
        .sync_device_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        Some(id) => (id.to_owned(), None),
        None => {
            let generated = hostname();
            (generated.clone(), Some(generated))
        }
    };
    let device_id = DeviceId::new(device_raw.clone())
        .map_err(|_| SyncSetupError::InvalidDeviceId(device_raw))?;
    let token = token
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or(SyncSetupError::MissingToken)?
        .to_owned();
    // token 已进入内存配置；此处只推导 ws 地址，不再读取任何文件/环境变量。
    let subscription_url = subscription_url(&server_url)
        .map_err(|error| SyncSetupError::InvalidServerUrl(error.to_string()))?;
    let interval = Duration::from_secs(
        settings
            .sync_interval_secs
            .map(clamp_sync_interval)
            .unwrap_or(DEFAULT_SYNC_INTERVAL_SECS),
    );
    Ok(SyncConfig {
        server_url,
        workspace_id,
        device_id,
        interval,
        token,
        generated_device_id,
        subscription_url,
        token_diagnostic,
    })
}

/// 兼容入口：经 [`resolve_token`] 的优先级链取 token（D109）。
///
/// 使用空内存后端时等价于“只从环境变量读”，但多了一层坏 token 校验；
/// 生产路径应由调用方直接传 `resolve_token(platform_store())` 的结果。
#[must_use]
#[allow(dead_code)]
pub fn token_from_env() -> Option<String> {
    resolve_token(&InMemoryTokenStore::new())
        .ok()
        .map(|resolution| resolution.token)
}

/// D91/D93 事件是否需要转发给同步线程（只标脏）。
///
/// 隐藏目录/文件与非 Markdown 由 `is_syncable_md` 统一过滤，与 D101 的
/// `doc_id_from_path` 使用同一套规则，避免同步线程标脏后又被静默丢弃。
#[must_use]
pub fn forwardable_watch_event(event: &WatchEvent) -> bool {
    let syncable = |path: &DocPath| is_syncable_md(path);
    match event {
        WatchEvent::Created { path }
        | WatchEvent::Modified { path }
        | WatchEvent::Removed { path } => syncable(path),
        WatchEvent::Renamed { from, to } => syncable(from) || syncable(to),
    }
}

/// 从当前文档绝对路径算同步 `DocId`（工作区相对路径），供冲突横幅标注“当前文档”。
#[must_use]
pub fn doc_id_for_path(root: &Path, path: &Path) -> Option<String> {
    doc_id_from_path(root, path).map(|id| id.to_string())
}

/// 纯函数：把阶段转成状态栏基础文案（不含冲突/需确认叠加）。
#[must_use]
pub fn sync_phase_text(phase: &SyncPhase) -> String {
    match phase {
        SyncPhase::NotConfigured { reason } => format!("同步未配置：{reason}"),
        SyncPhase::Idle { last_cursor } => format!("同步空闲 · 游标 {last_cursor}"),
        SyncPhase::Running => "正在同步…".to_owned(),
        SyncPhase::Success { last_cursor } => format!("上次同步成功 · 游标 {last_cursor}"),
        SyncPhase::Failed { reason } => format!("同步失败：{reason}"),
    }
}

/// 冲突 `DocId` 的展示文案（升序、中文路径原样；当前文档额外标注）。
#[must_use]
pub fn conflict_docs_text(status: &LocalSyncStatus, current_doc_id: Option<&str>) -> String {
    status
        .conflict_docs
        .iter()
        .map(|id| {
            if current_doc_id == Some(id.as_str()) {
                format!("{id}（当前文档）")
            } else {
                id.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("、")
}

/// 纯函数：状态栏同步文案（阶段 + 脏数 + 需确认 + 冲突计数）。
#[must_use]
pub fn sync_status_text(phase: &SyncPhase, status: Option<&LocalSyncStatus>) -> String {
    let mut text = sync_phase_text(phase);
    if let Some(status) = status {
        if !status.dirty_docs.is_empty() {
            text.push_str(&format!(" · 待同步 {}", status.dirty_docs.len()));
        }
        // 需确认优先明确写出“数据已保留”，否则用户会误以为同步已经丢弃了本地改动。
        if status.needs_confirmation {
            text.push_str(
                " · ⚠ 快照需确认：本地有未推送改动，已保留文件与 outbox；请先处理冲突或稍后重试",
            );
        }
        if !status.conflict_docs.is_empty() {
            text.push_str(&format!(" · 冲突 {}", status.conflict_docs.len()));
        }
    }
    text
}

/// 带 D109 token 诊断的状态栏文案；原 [`sync_status_text`] 语义保持不变。
///
/// 桌面渲染走 `main.rs` 的拼接函数；此纯函数保留供契约测试与复用。
#[must_use]
#[allow(dead_code)]
pub fn sync_status_text_with_diagnostic(
    phase: &SyncPhase,
    status: Option<&LocalSyncStatus>,
    token_diagnostic: Option<&str>,
    subscription_status: Option<&str>,
) -> String {
    let mut text = sync_status_text(phase, status);
    if let Some(diagnostic) = token_diagnostic {
        text.push_str(&format!(" · {diagnostic}"));
    }
    if let Some(subscription) = subscription_status {
        text.push_str(&format!(" · {subscription}"));
    }
    text
}

/// 状态栏提示是否为“需要强调”的异常态（失败/需确认/冲突）。
#[must_use]
pub fn sync_status_is_alert(phase: &SyncPhase, status: Option<&LocalSyncStatus>) -> bool {
    matches!(phase, SyncPhase::Failed { .. })
        || status.is_some_and(|s| s.needs_confirmation || !s.conflict_docs.is_empty())
}

/// 非模态同步横幅种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncBannerKind {
    /// 墓碑冲突副本可见（数据未丢）。
    Conflict,
    /// 快照回退被拒，需要用户确认。
    Confirmation,
}

/// 同步横幅上的动作（S5a 的“重试同步”保留原语义，三选一走 `choices`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncBannerAction {
    /// 重新发一条 `SyncNow`，绝不自动丢弃 outbox/本地文件。
    Retry,
}

/// S5b 横幅三选一：桌面展示用类型，发命令时再映射到引擎的
/// `ConflictChoice` / `SnapshotChoice`（D104）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncChoice {
    /// 保留本地：冲突场景用副本文本替换正文，快照场景只清除提示。
    KeepLocal,
    /// 采用远端：冲突场景接受删除，快照场景清空 outbox。
    UseRemote,
    /// 合并：冲突场景确定性拼接，快照场景保留 outbox 交给下一轮 CRDT 合并。
    Merge,
}

impl From<SyncChoice> for ConflictChoice {
    fn from(choice: SyncChoice) -> Self {
        match choice {
            SyncChoice::KeepLocal => Self::KeepLocal,
            SyncChoice::UseRemote => Self::UseRemote,
            SyncChoice::Merge => Self::Merge,
        }
    }
}

impl From<SyncChoice> for SnapshotChoice {
    fn from(choice: SyncChoice) -> Self {
        match choice {
            SyncChoice::KeepLocal => Self::KeepLocal,
            SyncChoice::UseRemote => Self::UseRemote,
            SyncChoice::Merge => Self::Merge,
        }
    }
}

/// 选择按钮的中文标签（纯函数，UI 只渲染，不自行拼业务文案）。
#[must_use]
pub fn sync_choice_label(choice: SyncChoice) -> &'static str {
    match choice {
        SyncChoice::KeepLocal => "保留本地",
        SyncChoice::UseRemote => "采用远端",
        SyncChoice::Merge => "合并",
    }
}

/// 破坏性选择的二次确认文案；非破坏性选择返回 `None`（可直接发送命令）。
///
/// 只按“横幅种类 + 选择”判定，不读 GPUI 状态：这样主线程第一次点击只显示文案，
/// 第二次点击才发命令，且任何覆盖语义都在文案里写明，绝不静默丢弃数据。
#[must_use]
pub fn destructive_choice_confirmation_text(
    kind: SyncBannerKind,
    choice: SyncChoice,
) -> Option<String> {
    match (kind, choice) {
        (SyncBannerKind::Conflict, SyncChoice::KeepLocal) => Some(
            "将用同步状态中的冲突副本文本替换当前文档正文，未同步的本地改动会被替换。确认执行？"
                .to_owned(),
        ),
        (SyncBannerKind::Conflict, SyncChoice::UseRemote) => Some(
            "将接受远端删除：保留墓碑与冲突副本作审计，不写回文档正文。确认执行？"
                .to_owned(),
        ),
        (SyncBannerKind::Confirmation, SyncChoice::UseRemote) => Some(
            "将清空未推送 outbox，放弃本地未推送批次；本地文件暂不改动，下一轮同步才由服务端快照覆盖。确认执行？"
                .to_owned(),
        ),
        _ => None,
    }
}

/// 同步横幅的完整展示数据（纯数据，GPUI 只负责渲染）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncBanner {
    /// 横幅种类（决定颜色/是否常驻，也用于二次确认文案）。
    pub kind: SyncBannerKind,
    /// 主标题。
    pub title: String,
    /// 详细说明（含冲突 DocId）。
    pub detail: String,
    /// S5a 的“重试同步”按钮，保持 D102 原语义。
    pub action: Option<SyncBannerAction>,
    /// S5b 三选一按钮（复用 D92 横幅样式，不替换 `action`）。
    pub choices: Vec<SyncChoice>,
    /// 本次选择针对的文档 ID；快照横幅取当前文档，仅用于回写路径定位。
    pub target_doc_id: Option<String>,
}

/// 横幅三选一的固定顺序（保留本地 / 采用远端 / 合并）。
const ALL_SYNC_CHOICES: [SyncChoice; 3] = [
    SyncChoice::KeepLocal,
    SyncChoice::UseRemote,
    SyncChoice::Merge,
];

/// 冲突横幅的目标文档：当前文档在冲突列表中时优先处理它，否则取升序第一项。
#[must_use]
pub fn conflict_target_doc_id(
    status: &LocalSyncStatus,
    current_doc_id: Option<&str>,
) -> Option<String> {
    status
        .conflict_docs
        .iter()
        .find(|id| current_doc_id == Some(id.as_str()))
        .or_else(|| status.conflict_docs.first())
        .cloned()
}

/// 依据引擎状态生成横幅：需确认优先，其次冲突；都没有则不显示。
#[must_use]
pub fn sync_banner(status: &LocalSyncStatus, current_doc_id: Option<&str>) -> Option<SyncBanner> {
    let docs = conflict_docs_text(status, current_doc_id);
    if status.needs_confirmation {
        let detail = if docs.is_empty() {
            "本地有未推送改动，已保留文件与 outbox；数据未静默删除。请先处理冲突或稍后重试。"
                .to_owned()
        } else {
            format!(
                "本地有未推送改动，已保留文件与 outbox；数据未静默删除。冲突文档：{docs}。请先处理冲突或稍后重试。"
            )
        };
        return Some(SyncBanner {
            kind: SyncBannerKind::Confirmation,
            title: "同步快照需用户确认".to_owned(),
            detail,
            action: Some(SyncBannerAction::Retry),
            choices: ALL_SYNC_CHOICES.to_vec(),
            target_doc_id: current_doc_id.map(ToOwned::to_owned),
        });
    }
    if !status.conflict_docs.is_empty() {
        return Some(SyncBanner {
            kind: SyncBannerKind::Conflict,
            title: format!("同步发现 {} 个冲突文档", status.conflict_docs.len()),
            detail: format!("{docs}：数据已保存在同步状态中，未静默删除。"),
            action: None,
            choices: ALL_SYNC_CHOICES.to_vec(),
            target_doc_id: conflict_target_doc_id(status, current_doc_id),
        });
    }
    None
}

/// 同步线程收到的命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncCommand {
    /// 立即执行一轮同步（手动入口或周期超时）。
    SyncNow,
    /// 工作区文件事件；同步线程只标脏，不做 D92 的 reload/横幅判定。
    WatchEvents(Vec<WatchEvent>),
    /// S5b：显式解决一个墓碑冲突；`local_text` 是当前文档正文（合并用）。
    ResolveConflict {
        /// 目标文档 ID（工作区相对路径）。
        doc_id: String,
        /// 用户三选一（已过二次确认）。
        choice: SyncChoice,
        /// 合并时使用的本地正文。
        local_text: String,
    },
    /// S5b：显式解决快照确认门槛；引擎按选择保留或清空 outbox。
    ResolveSnapshot {
        /// 用户三选一（已过二次确认）。
        choice: SyncChoice,
    },
    /// 停止线程；当前同步调用结束后退出。
    Shutdown,
}

/// 同步线程回传主线程的 UI 状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncReply {
    /// 阶段变化。
    Phase(SyncPhase),
    /// 引擎状态快照（`dirty_docs` / `needs_confirmation` / `conflict_docs` / 游标）。
    Status(LocalSyncStatus),
    /// 引擎启动失败：主线程显示“未配置/失败”原因，不保留空引擎。
    SetupFailed(String),
    /// S5b：解决动作的引擎结果；`doc_id` 供主线程把正文写回正确文档。
    Resolved {
        /// 目标文档 ID；快照解决为 `None`（引擎不回写正文）。
        doc_id: Option<String>,
        /// 引擎返回的解决结果（含待写回正文）。
        outcome: ResolveOutcome,
    },
    /// D108：订阅线程收到远端变更提示，主线程只触发非破坏性 `SyncNow`。
    RemoteChanged {
        /// 有变更的工作区 ID（仅提示，不做合并决策）。
        workspaces: Vec<String>,
        /// 有变更的文档 ID（仅提示）。
        doc_ids: Vec<String>,
        /// 服务端游标提示（不得据此跳过拉取）。
        cursor_hint: Option<u64>,
    },
    /// D108：订阅失败/断线；周期同步仍继续，状态栏展示回退提示。
    SubscriptionFailed(String),
}

/// 同步线程句柄：drop 即发 `Shutdown` 并在独立等待线程 join，不阻塞 GPUI 主线程。
pub struct SyncHandle {
    /// 命令发送端；收端在同步线程。
    tx: mpsc::Sender<SyncCommand>,
    /// 同步线程；`None` 表示已被 join/回收。
    thread: Option<JoinHandle<()>>,
    /// D108：订阅线程的停止标志；与同步线程同生命周期。
    subscription_stop: Option<Arc<AtomicBool>>,
    /// D108：专用订阅线程；不参与文档合并。
    subscription_thread: Option<JoinHandle<()>>,
}

impl SyncHandle {
    /// 请求立即同步；通道断开（线程已退出）时静默忽略，由状态栏显示失败原因。
    pub fn sync_now(&self) {
        let _ = self.tx.send(SyncCommand::SyncNow);
    }

    /// 转发工作区事件；同步线程只调用 `handle_watch_events` 标脏。
    pub fn forward_events(&self, events: Vec<WatchEvent>) {
        if events.is_empty() {
            return;
        }
        let _ = self.tx.send(SyncCommand::WatchEvents(events));
    }

    /// 请求解决一个墓碑冲突；只发命令，不触碰 GPUI 状态或本地文件。
    pub fn resolve_conflict(&self, doc_id: String, choice: SyncChoice, local_text: String) {
        let _ = self.tx.send(SyncCommand::ResolveConflict {
            doc_id,
            choice,
            local_text,
        });
    }

    /// 请求解决快照确认门槛；只发命令，不触碰 GPUI 状态或本地文件。
    pub fn resolve_snapshot(&self, choice: SyncChoice) {
        let _ = self.tx.send(SyncCommand::ResolveSnapshot { choice });
    }
}

impl Drop for SyncHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(SyncCommand::Shutdown);
        // 先停订阅线程：阻塞读取有超时，收到停止标志即退出，不留残留 socket。
        if let Some(stop) = self.subscription_stop.take() {
            stop.store(true, Ordering::SeqCst);
        }
        // ureq / TCP 调用可能还在进行中；join 放到独立等待线程，保证关窗时 UI 不卡住。
        if let Some(thread) = self.thread.take() {
            std::thread::spawn(move || {
                let _ = thread.join();
            });
        }
        if let Some(thread) = self.subscription_thread.take() {
            std::thread::spawn(move || {
                let _ = thread.join();
            });
        }
    }
}

/// 由同步配置推出订阅配置（纯数据变换，可在无 socket 环境下单测）。
#[must_use]
pub fn subscription_config(config: &SyncConfig) -> SubscriptionConfig {
    SubscriptionConfig {
        url: config.subscription_url.clone(),
        workspace: config.workspace_id.to_string(),
        token: config.token.clone(),
        read_timeout: Duration::from_secs(5),
        heartbeat: Duration::from_secs(15),
    }
}

/// D108：专用阻塞订阅线程；收到事件只回传提示，绝不触碰本地文档/outbox。
///
/// 连接失败或断线只回传 [`SyncReply::SubscriptionFailed`] 后退出；同步线程仍独立运行，
/// 由既有周期重试兜底。停止标志由 `SyncHandle::drop` 置位，读取超时保证能及时回收。
fn spawn_subscription(
    config: SubscriptionConfig,
    replies: mpsc::Sender<SyncReply>,
    stop: Arc<AtomicBool>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("mazha-sync-subscribe".to_owned())
        .spawn(move || {
            let mut subscription = match TcpSubscription::connect(config) {
                Ok(subscription) => subscription,
                Err(error) => {
                    let _ = replies.send(SyncReply::SubscriptionFailed(
                        subscription_error_message(&error),
                    ));
                    return;
                }
            };
            loop {
                if stop.load(Ordering::SeqCst) {
                    subscription.close();
                    break;
                }
                match subscription.next_event_timeout(Duration::from_millis(500)) {
                    Ok(Some(SyncEvent::WorkspaceChanged {
                        workspaces,
                        doc_ids,
                        cursor_hint,
                    })) => {
                        let _ = replies.send(SyncReply::RemoteChanged {
                            workspaces,
                            doc_ids,
                            cursor_hint,
                        });
                    }
                    Ok(None) => {}
                    Err(SubscriptionError::Timeout) => continue,
                    Err(error) => {
                        let _ = replies.send(SyncReply::SubscriptionFailed(
                            subscription_error_message(&error),
                        ));
                        break;
                    }
                }
            }
        })
}

/// 把订阅错误转成状态栏文案（不含 token）。
#[must_use]
pub fn subscription_error_message(error: &SubscriptionError) -> String {
    error.to_string()
}

/// 启动专用同步线程：引擎与 HTTP 传输都只在该线程上创建/调用。
///
/// 周期由 `recv_timeout` 实现：收到 `SyncNow` 或超时执行 `sync_once`；网络失败
/// 只回传错误并继续下一轮，outbox 由 D101 引擎保留，绝不自动丢弃。
pub fn spawn_sync(
    config: SyncConfig,
    provider: Arc<dyn StorageProvider>,
    replies: mpsc::Sender<SyncReply>,
) -> SyncHandle {
    let (tx, rx) = mpsc::channel::<SyncCommand>();
    // 线程创建失败时也要能回传原因，因此留发送端副本在调用方。
    let failure_replies = replies.clone();
    let subscription_replies = replies.clone();
    // D108：订阅线程与同步线程解耦；订阅建连失败也不影响下面的周期同步启动。
    let sub_config = subscription_config(&config);
    let subscription_stop = Arc::new(AtomicBool::new(false));
    let sub_stop = Arc::clone(&subscription_stop);
    let spawned = std::thread::Builder::new()
        .name("mazha-sync".to_owned())
        .spawn(move || {
            let mut engine = match LocalSyncEngine::open(
                Arc::clone(&provider),
                config.workspace_id.clone(),
                config.device_id.clone(),
            ) {
                Ok(engine) => engine,
                Err(error) => {
                    let _ = replies.send(SyncReply::SetupFailed(error.to_string()));
                    return;
                }
            };
            let mut transport = HttpTransport::new(
                &config.server_url,
                config.workspace_id,
                config.device_id,
                &config.token,
            );
            let _ = replies.send(SyncReply::Phase(SyncPhase::Idle {
                last_cursor: engine.status().last_cursor.get(),
            }));
            let _ = replies.send(SyncReply::Status(engine.status().clone()));
            loop {
                let command = match rx.recv_timeout(config.interval) {
                    Ok(command) => command,
                    Err(mpsc::RecvTimeoutError::Timeout) => SyncCommand::SyncNow,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                match command {
                    SyncCommand::Shutdown => break,
                    SyncCommand::ResolveConflict {
                        doc_id,
                        choice,
                        local_text,
                    } => {
                        // 解决动作不触发网络；结果/状态都经同一回执通道交主线程。
                        match engine.resolve_conflict(&doc_id, &local_text, choice.into()) {
                            Ok(outcome) => {
                                let _ = replies.send(SyncReply::Status(engine.status().clone()));
                                let _ = replies.send(SyncReply::Resolved {
                                    doc_id: Some(doc_id),
                                    outcome,
                                });
                            }
                            Err(error) => {
                                let _ = replies.send(SyncReply::Phase(SyncPhase::Failed {
                                    reason: error.to_string(),
                                }));
                                let _ = replies.send(SyncReply::Status(engine.status().clone()));
                            }
                        }
                    }
                    SyncCommand::ResolveSnapshot { choice } => {
                        match engine.resolve_snapshot_confirmation(choice.into()) {
                            Ok(outcome) => {
                                let _ = replies.send(SyncReply::Status(engine.status().clone()));
                                let _ = replies.send(SyncReply::Resolved {
                                    doc_id: None,
                                    outcome,
                                });
                            }
                            Err(error) => {
                                let _ = replies.send(SyncReply::Phase(SyncPhase::Failed {
                                    reason: error.to_string(),
                                }));
                                let _ = replies.send(SyncReply::Status(engine.status().clone()));
                            }
                        }
                    }
                    SyncCommand::WatchEvents(events) => {
                        if engine.handle_watch_events(&events) > 0 {
                            let _ = replies.send(SyncReply::Status(engine.status().clone()));
                        }
                    }
                    SyncCommand::SyncNow => {
                        let _ = replies.send(SyncReply::Phase(SyncPhase::Running));
                        match engine.sync_once(&mut transport) {
                            Ok(_) => {
                                let _ = replies.send(SyncReply::Phase(SyncPhase::Success {
                                    last_cursor: engine.status().last_cursor.get(),
                                }));
                            }
                            Err(error) => {
                                // 引擎已把错误/冲突状态写盘；线程保留，下个周期继续重试。
                                let _ = replies.send(SyncReply::Phase(SyncPhase::Failed {
                                    reason: error.to_string(),
                                }));
                            }
                        }
                        let _ = replies.send(SyncReply::Status(engine.status().clone()));
                    }
                }
            }
        });
    // 线程创建失败时回传明确原因；绝不能退化为在 GPUI 主线程上跑同步。
    let thread = match spawned {
        Ok(thread) => Some(thread),
        Err(error) => {
            let _ = failure_replies.send(SyncReply::SetupFailed(
                SyncSetupError::Engine(error.to_string()).message(),
            ));
            None
        }
    };
    let (subscription_thread, subscription_stop) = if thread.is_some() {
        match spawn_subscription(sub_config, subscription_replies, sub_stop) {
            Ok(subscription_thread) => (Some(subscription_thread), Some(subscription_stop)),
            Err(error) => {
                let _ = failure_replies.send(SyncReply::SubscriptionFailed(format!(
                    "订阅线程启动失败：{error}"
                )));
                (None, None)
            }
        }
    } else {
        (None, None)
    };
    SyncHandle {
        tx,
        thread,
        subscription_stop,
        subscription_thread,
    }
}

/// 为工作区根目录创建同步 Provider；失败时由调用方显示错误。
pub fn open_provider(root: &Path) -> Result<Arc<dyn StorageProvider>, String> {
    FileSystemProvider::open(root)
        .map(|provider| Arc::new(provider) as Arc<dyn StorageProvider>)
        .map_err(|error| error.to_string())
}

/// `DocPath` 辅助：把同步 `DocId` 转成相对路径（测试事件过滤用）。
#[cfg(test)]
pub(crate) fn doc_path(raw: &str) -> Option<DocPath> {
    DocPath::new(raw).ok()
}

/// 未使用的路径占位（保持 `PathBuf` 在测试中的可构造性提示）。
#[cfg(test)]
pub(crate) fn path_buf(raw: &str) -> PathBuf {
    PathBuf::from(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mazha_sync_local::LocalSyncStatus;

    /// D102：同步引擎必须能跨线程转移（否则专用线程方案不成立）。
    #[test]
    fn engine_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<LocalSyncEngine>();
        assert_send::<HttpTransport>();
    }

    #[test]
    fn server_url_only_allows_http_and_https_with_host() {
        for url in [
            "http://127.0.0.1:9123",
            "https://sync.example.com/path",
            " http://localhost ",
        ] {
            assert!(validate_server_url(url).is_ok(), "{url}");
        }
        for url in [
            "",
            "ftp://host",
            "https://",
            "http:///path",
            "https://a b.com",
            "https://host\n",
        ] {
            assert!(validate_server_url(url).is_err(), "{url}");
        }
    }

    #[test]
    fn resolve_config_falls_back_and_validates_ids() {
        let root = path_buf("/tmp/中文 工作区");
        let mut settings = Settings {
            sync_server_url: Some("https://sync.example.com/".into()),
            sync_interval_secs: Some(9000),
            ..Settings::default()
        };
        let config = resolve_config(&settings, &root, Some("secret-token")).unwrap();
        assert_eq!(config.server_url, "https://sync.example.com");
        assert!(!config.workspace_id.as_str().is_empty());
        assert!(config.workspace_id.as_str().contains("工作区"));
        assert!(!config.device_id.as_str().is_empty());
        assert!(config.generated_device_id.is_some());
        assert_eq!(config.interval, Duration::from_secs(3600));
        // device 一旦写回 settings，重启后不再生成新身份。
        settings.sync_device_id = config.generated_device_id.clone();
        let again = resolve_config(&settings, &root, Some("secret-token")).unwrap();
        assert_eq!(again.device_id, config.device_id);
        assert_eq!(again.generated_device_id, None);
    }

    #[test]
    fn subscription_config_derives_ws_url_and_redacts_token() {
        let settings = Settings {
            sync_server_url: Some("https://sync.example.com/mz".into()),
            sync_workspace_id: Some("ws-1".into()),
            sync_device_id: Some("dev-a".into()),
            ..Settings::default()
        };
        let config = resolve_config_with_diagnostic(
            &settings,
            &path_buf("/tmp/ws"),
            Some("secret-token"),
            Some("平台密钥链不可用".into()),
        )
        .unwrap();
        assert_eq!(
            config.subscription_url,
            "wss://sync.example.com/mz/sync/v0/subscribe"
        );
        assert_eq!(config.token_diagnostic.as_deref(), Some("平台密钥链不可用"));
        let sub = subscription_config(&config);
        assert_eq!(sub.workspace, "ws-1");
        assert!(!format!("{sub:?}").contains("secret-token"));
    }

    #[test]
    fn sync_status_diagnostic_is_additive() {
        let base = sync_status_text(&SyncPhase::Idle { last_cursor: 1 }, None);
        let with = sync_status_text_with_diagnostic(
            &SyncPhase::Idle { last_cursor: 1 },
            None,
            Some("平台密钥链不可用"),
            Some("订阅已断开，已回退周期同步"),
        );
        assert!(with.starts_with(&base));
        assert!(with.contains("平台密钥链不可用"));
        assert!(with.contains("已回退周期同步"));
    }

    #[test]
    fn resolve_config_reports_missing_fields_without_panic() {
        let root = path_buf("/tmp/ws");
        assert_eq!(
            resolve_config(&Settings::default(), &root, Some("t")),
            Err(SyncSetupError::NotConfigured)
        );
        let mut settings = Settings {
            sync_server_url: Some("ftp://host".into()),
            ..Settings::default()
        };
        assert!(matches!(
            resolve_config(&settings, &root, Some("t")),
            Err(SyncSetupError::InvalidServerUrl(_))
        ));
        settings.sync_server_url = Some("https://host".into());
        assert_eq!(
            resolve_config(&settings, &root, None),
            Err(SyncSetupError::MissingToken)
        );
    }

    #[test]
    fn fallback_ids_sanitize_controls_and_cjk_paths() {
        assert_eq!(sanitize_device_fallback("  My Mac \n"), "My-Mac");
        assert_eq!(sanitize_device_fallback(" \t "), DEVICE_FALLBACK);
        assert!(WorkspaceId::new(workspace_fallback_id(&path_buf("/tmp/中文笔记"))).is_ok());
    }

    #[test]
    fn watch_event_forwarding_filters_hidden_and_non_md() {
        use WatchEvent::*;
        let md = doc_path("notes/中文.md").unwrap();
        let markdown = doc_path("a.MARKDOWN").unwrap();
        let hidden = doc_path(".obsidian/config.md").unwrap();
        let text = doc_path("notes/a.txt").unwrap();
        assert!(forwardable_watch_event(&Created { path: md.clone() }));
        assert!(forwardable_watch_event(&Modified {
            path: markdown.clone()
        }));
        assert!(!forwardable_watch_event(&Removed {
            path: hidden.clone()
        }));
        assert!(!forwardable_watch_event(&Created { path: text.clone() }));
        assert!(forwardable_watch_event(&Renamed {
            from: hidden,
            to: md
        }));
        assert!(!forwardable_watch_event(&Renamed {
            from: text,
            to: doc_path(".hidden.md").unwrap()
        }));
    }

    fn status(needs_confirmation: bool, conflicts: &[&str], dirty: &[&str]) -> LocalSyncStatus {
        LocalSyncStatus {
            dirty_docs: dirty.iter().map(|s| (*s).to_owned()).collect(),
            needs_confirmation,
            conflict_docs: conflicts.iter().map(|s| (*s).to_owned()).collect(),
            ..LocalSyncStatus::default()
        }
    }

    #[test]
    fn status_texts_cover_all_phases_and_overlays() {
        let clean = status(false, &[], &[]);
        assert_eq!(
            sync_status_text(
                &SyncPhase::NotConfigured {
                    reason: "无 token".into()
                },
                None
            ),
            "同步未配置：无 token"
        );
        assert_eq!(
            sync_status_text(&SyncPhase::Idle { last_cursor: 7 }, Some(&clean)),
            "同步空闲 · 游标 7"
        );
        assert_eq!(sync_phase_text(&SyncPhase::Running), "正在同步…");
        assert_eq!(
            sync_status_text(&SyncPhase::Success { last_cursor: 9 }, Some(&clean)),
            "上次同步成功 · 游标 9"
        );
        let failed = SyncPhase::Failed {
            reason: "离线".into(),
        };
        assert!(sync_status_text(&failed, Some(&clean)).starts_with("同步失败：离线"));
        assert!(sync_status_is_alert(&failed, Some(&clean)));
        assert!(!sync_status_is_alert(
            &SyncPhase::Idle { last_cursor: 0 },
            Some(&clean)
        ));
        let dirty = status(false, &[], &["a.md"]);
        assert!(
            sync_status_text(&SyncPhase::Idle { last_cursor: 0 }, Some(&dirty))
                .contains("待同步 1")
        );
        let conflict = status(false, &["中文/冲突.md"], &[]);
        let text = sync_status_text(&SyncPhase::Success { last_cursor: 3 }, Some(&conflict));
        assert!(text.ends_with("冲突 1"), "{text}");
        assert!(sync_status_is_alert(
            &SyncPhase::Success { last_cursor: 3 },
            Some(&conflict)
        ));
        let confirm = status(true, &[], &[]);
        let text = sync_status_text(
            &SyncPhase::Failed {
                reason: "重试".into(),
            },
            Some(&confirm),
        );
        assert!(text.contains("已保留文件与 outbox"), "{text}");
        assert!(sync_status_is_alert(&SyncPhase::Running, Some(&confirm)));
    }

    #[test]
    fn banners_show_conflict_docs_and_retry_without_override() {
        let clean = status(false, &[], &[]);
        assert!(sync_banner(&clean, None).is_none());
        let conflict = status(false, &["dir/中文.md", "b.md"], &[]);
        let banner = sync_banner(&conflict, Some("dir/中文.md")).unwrap();
        assert_eq!(banner.kind, SyncBannerKind::Conflict);
        assert!(banner.title.contains("2"));
        assert!(banner.detail.contains("dir/中文.md（当前文档）"));
        assert!(banner.detail.contains("未静默删除"));
        assert!(banner.action.is_none());
        assert!(!format!("{}{}", banner.title, banner.detail).contains("覆盖"));
        let confirm = status(true, &["dir/中文.md"], &[]);
        let banner = sync_banner(&confirm, None).unwrap();
        assert_eq!(banner.kind, SyncBannerKind::Confirmation);
        assert_eq!(banner.action, Some(SyncBannerAction::Retry));
        assert!(banner.detail.contains("已保留文件与 outbox"));
    }

    #[test]
    fn conflict_docs_text_keeps_cjk_and_marks_current() {
        let conflict = status(false, &["笔记/a.md", "b.md"], &[]);
        assert_eq!(
            conflict_docs_text(&conflict, Some("b.md")),
            "笔记/a.md、b.md（当前文档）"
        );
    }

    /// D104：三选一标签/映射固定，桌面展示与引擎选择一一对应。
    #[test]
    fn sync_choices_map_to_engine_choices_deterministically() {
        assert_eq!(sync_choice_label(SyncChoice::KeepLocal), "保留本地");
        assert_eq!(sync_choice_label(SyncChoice::UseRemote), "采用远端");
        assert_eq!(sync_choice_label(SyncChoice::Merge), "合并");
        assert_eq!(
            ConflictChoice::from(SyncChoice::KeepLocal),
            ConflictChoice::KeepLocal
        );
        assert_eq!(
            ConflictChoice::from(SyncChoice::UseRemote),
            ConflictChoice::UseRemote
        );
        assert_eq!(
            ConflictChoice::from(SyncChoice::Merge),
            ConflictChoice::Merge
        );
        assert_eq!(
            SnapshotChoice::from(SyncChoice::KeepLocal),
            SnapshotChoice::KeepLocal
        );
        assert_eq!(
            SnapshotChoice::from(SyncChoice::UseRemote),
            SnapshotChoice::UseRemote
        );
        assert_eq!(
            SnapshotChoice::from(SyncChoice::Merge),
            SnapshotChoice::Merge
        );
    }

    /// D104：只有破坏性选择给出二次确认文案，文案写明后果且含中文。
    #[test]
    fn destructive_choices_require_explicit_confirmation_text() {
        for choice in ALL_SYNC_CHOICES {
            let text = destructive_choice_confirmation_text(SyncBannerKind::Conflict, choice);
            if choice == SyncChoice::Merge {
                assert!(text.is_none());
            } else {
                let text = text.unwrap_or_default();
                assert!(text.contains("确认执行？"), "{choice:?}");
                assert!(!text.is_empty());
            }
        }
        let snapshot = destructive_choice_confirmation_text(
            SyncBannerKind::Confirmation,
            SyncChoice::UseRemote,
        )
        .unwrap();
        assert!(snapshot.contains("清空未推送 outbox"));
        assert!(
            destructive_choice_confirmation_text(
                SyncBannerKind::Confirmation,
                SyncChoice::KeepLocal
            )
            .is_none()
        );
        assert!(
            destructive_choice_confirmation_text(SyncBannerKind::Confirmation, SyncChoice::Merge)
                .is_none()
        );
    }

    /// D104：横幅 additive 带三选一与目标文档；D102 的 action/文案语义保持原样。
    #[test]
    fn banners_expose_three_choices_and_target_without_breaking_s5a() {
        let conflict = status(false, &["b.md", "笔记/a.md"], &[]);
        let banner = sync_banner(&conflict, Some("笔记/a.md")).unwrap();
        assert_eq!(banner.choices, ALL_SYNC_CHOICES);
        assert_eq!(banner.target_doc_id.as_deref(), Some("笔记/a.md"));
        assert_eq!(banner.action, None);
        assert!(!format!("{}{}", banner.title, banner.detail).contains("覆盖"));

        let other = sync_banner(&conflict, Some("未冲突.md")).unwrap();
        assert_eq!(other.target_doc_id.as_deref(), Some("b.md"));

        let confirm = status(true, &[], &[]);
        let banner = sync_banner(&confirm, Some("当前.md")).unwrap();
        assert_eq!(banner.choices, ALL_SYNC_CHOICES);
        assert_eq!(banner.action, Some(SyncBannerAction::Retry));
        assert_eq!(banner.target_doc_id.as_deref(), Some("当前.md"));
    }

    /// D104：`SyncHandle` 的解决入口只发命令，不调用引擎、不碰文件。
    #[test]
    fn resolve_commands_are_sent_through_the_handle() {
        let (tx, rx) = mpsc::channel::<SyncCommand>();
        let handle = SyncHandle {
            tx,
            thread: None,
            subscription_stop: None,
            subscription_thread: None,
        };
        handle.resolve_conflict(
            "笔记/a.md".to_owned(),
            SyncChoice::Merge,
            "本地正文".to_owned(),
        );
        handle.resolve_snapshot(SyncChoice::UseRemote);
        assert_eq!(
            rx.try_recv().unwrap(),
            SyncCommand::ResolveConflict {
                doc_id: "笔记/a.md".to_owned(),
                choice: SyncChoice::Merge,
                local_text: "本地正文".to_owned(),
            }
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            SyncCommand::ResolveSnapshot {
                choice: SyncChoice::UseRemote,
            }
        );
        // Drop 时才发 Shutdown；句柄仍存活时通道应为空。
        assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
        drop(handle);
        assert!(matches!(rx.try_recv(), Ok(SyncCommand::Shutdown)));
    }
}
