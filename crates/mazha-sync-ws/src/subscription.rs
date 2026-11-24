//! 订阅 trait、配置与内存假实现（D108）。
//!
//! `SyncSubscription` 与 `SyncTransport` 完全解耦：它没有请求/应答语义，也不认识
//! 九种同步消息；唯一职责是“阻塞等下一条变更提示”。`InMemorySubscription` 供
//! 桌面/服务端做无网络单测，绝不会碰 socket。

use std::collections::VecDeque;
use std::fmt;
use std::time::{Duration, Instant};

use crate::event::SyncEvent;

/// 订阅错误：统一转成中文文案给状态栏，绝不 panic。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SubscriptionError {
    /// URL/scheme/host/端口非法。
    #[error("订阅地址无效：{0}")]
    InvalidUrl(String),
    /// HTTP Upgrade 握手失败（含校验和 Accept 计算）。
    #[error("订阅握手失败：{0}")]
    Handshake(String),
    /// 阻塞 socket 读写失败。
    #[error("订阅连接失败：{0}")]
    Io(String),
    /// 帧/事件协议非法；坏帧必须断连，不做猜测恢复。
    #[error("订阅协议错误：{0}")]
    Protocol(String),
    /// 等待超时（只由带超时的读取产生）。
    #[error("等待订阅事件超时")]
    Timeout,
    /// 对端或本地已关闭订阅。
    #[error("订阅已关闭")]
    Closed,
}

/// 阻塞式订阅客户端的最小配置。
#[derive(Clone, PartialEq, Eq)]
pub struct SubscriptionConfig {
    /// 已推导好的 ws(s) 订阅 URL（token 绝不放这里）。
    pub url: String,
    /// 订阅的工作区 ID（写入 query，百分号编码）。
    pub workspace: String,
    /// Bearer token（只在内存，绝不写盘/日志）。
    pub token: String,
    /// 单次 socket 读取超时；到期返回 [`SubscriptionError::Timeout`]。
    pub read_timeout: Duration,
    /// 心跳间隔；0 表示不主动发 Ping。
    pub heartbeat: Duration,
}

impl fmt::Debug for SubscriptionConfig {
    /// 手写 Debug 以免 token 被日志/断言输出。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SubscriptionConfig")
            .field("url", &self.url)
            .field("workspace", &self.workspace)
            .field("token", &"<redacted>")
            .field("read_timeout", &self.read_timeout)
            .field("heartbeat", &self.heartbeat)
            .finish()
    }
}

/// 可替换的同步变更订阅（同步阻塞、无 async runtime）。
///
/// `next_event_timeout` 让桌面订阅线程可在等待期间检查停止标志；`None` 只表示
/// “超时内没有变更”，不是连接关闭。
pub trait SyncSubscription: Send {
    /// 阻塞等一条事件；`None` 仅在底层用「关闭」表达正常结束时返回。
    ///
    /// # Errors
    /// 连接/协议/握手错误返回对应 [`SubscriptionError`]。
    fn next_event(&mut self) -> Result<Option<SyncEvent>, SubscriptionError>;

    /// 带截止时间的阻塞等待。
    ///
    /// # Errors
    /// 超时返回 [`SubscriptionError::Timeout`]，其它同 [`Self::next_event`]。
    fn next_event_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<SyncEvent>, SubscriptionError>;

    /// 主动关闭订阅（幂等）；析构时也必须释放 socket/线程资源。
    fn close(&mut self);
}

/// 内存假订阅：按入队顺序交付事件，供桌面/服务端做纯 Rust 单测。
#[derive(Debug, Default)]
pub struct InMemorySubscription {
    /// 待交付事件队列。
    events: VecDeque<SyncEvent>,
    /// 已关闭标志；关闭后再读返回 [`SubscriptionError::Closed`]。
    closed: bool,
}

impl InMemorySubscription {
    /// 用给定事件序列构造假订阅。
    #[must_use]
    pub fn new(events: impl IntoIterator<Item = SyncEvent>) -> Self {
        Self {
            events: events.into_iter().collect(),
            closed: false,
        }
    }

    /// 追加一条事件（可模拟运行中服务端下发）。
    pub fn push(&mut self, event: SyncEvent) {
        self.events.push_back(event);
    }

    /// 当前是否已关闭（诊断用）。
    #[must_use]
    pub const fn is_closed(&self) -> bool {
        self.closed
    }
}

impl SyncSubscription for InMemorySubscription {
    fn next_event(&mut self) -> Result<Option<SyncEvent>, SubscriptionError> {
        if self.closed {
            return Err(SubscriptionError::Closed);
        }
        Ok(self.events.pop_front())
    }

    fn next_event_timeout(
        &mut self,
        _timeout: Duration,
    ) -> Result<Option<SyncEvent>, SubscriptionError> {
        // 假实现不真的等待：有事件立即返回，无事件按“超时”处理，测试无需 sleep。
        if self.closed {
            return Err(SubscriptionError::Closed);
        }
        match self.events.pop_front() {
            Some(event) => Ok(Some(event)),
            None => Err(SubscriptionError::Timeout),
        }
    }

    fn close(&mut self) {
        self.closed = true;
        self.events.clear();
    }
}

/// 内部辅助：判断 `Instant` 是否已到截止时间（便于超时逻辑单测复用）。
#[must_use]
pub fn deadline_reached(started: Instant, timeout: Duration) -> bool {
    started.elapsed() >= timeout
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SyncEvent {
        SyncEvent::WorkspaceChanged {
            workspaces: vec!["ws-1".into()],
            doc_ids: vec!["a.md".into()],
            cursor_hint: Some(9),
        }
    }

    #[test]
    fn in_memory_subscription_delivers_events_in_order() {
        let mut subscription = InMemorySubscription::new([sample(), sample()]);
        assert_eq!(subscription.next_event().unwrap(), Some(sample()));
        subscription.push(sample());
        assert_eq!(
            subscription
                .next_event_timeout(Duration::from_millis(10))
                .unwrap(),
            Some(sample())
        );
        assert_eq!(
            subscription
                .next_event_timeout(Duration::from_millis(10))
                .unwrap(),
            Some(sample())
        );
        assert_eq!(
            subscription.next_event_timeout(Duration::from_millis(10)),
            Err(SubscriptionError::Timeout)
        );
        subscription.close();
        assert!(subscription.is_closed());
        assert_eq!(subscription.next_event(), Err(SubscriptionError::Closed));
    }

    #[test]
    fn config_debug_redacts_token() {
        let config = SubscriptionConfig {
            url: "ws://host/sync/v0/subscribe".into(),
            workspace: "ws-1".into(),
            token: "super-secret".into(),
            read_timeout: Duration::from_secs(1),
            heartbeat: Duration::ZERO,
        };
        let text = format!("{config:?}");
        assert!(!text.contains("super-secret"));
        assert!(text.contains("<redacted>"));
    }
}
