//! 服务端订阅总线（Phase 5 S5b 第三批 / D108，见 `docs/SYNC-SPEC.md` §7.4.6）。
//!
//! 只在既有 `SyncService::push` 成功提交后 additive 发布“工作区已变更”提示；
//! 订阅者只会收到轻量 `SyncEvent`，服务端不会替客户端决定合并/覆盖，也不碰九种同步消息。

use std::collections::BTreeMap;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

use mazha_sync::WorkspaceId;
use mazha_sync_ws::SyncEvent;

/// 订阅总线：工作区 → 订阅者通道。
///
/// 用 `BTreeMap` 让订阅 ID 有序、测试可预测；事件发送是非阻塞的 `try_send`，
/// 慢订阅者不会被同步请求拖住（断线者直接摘除）。
#[derive(Debug, Default)]
pub struct SubscriptionHub {
    /// 订阅 ID → (工作区, 事件发送端)。
    subscribers: BTreeMap<u64, (WorkspaceId, Sender<SyncEvent>)>,
    /// 单调递增的订阅 ID（重置只发生在服务重启）。
    next_id: u64,
}

impl SubscriptionHub {
    /// 登记一个订阅者并返回其 ID。
    pub fn subscribe(&mut self, workspace: WorkspaceId, sender: Sender<SyncEvent>) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.subscribers.insert(id, (workspace, sender));
        id
    }

    /// 摘除订阅者；幂等，重复调用无副作用。
    pub fn unsubscribe(&mut self, id: u64) {
        self.subscribers.remove(&id);
    }

    /// 发布变更提示：只发给同工作区且仍在线的订阅者。
    pub fn publish(&mut self, workspace: &WorkspaceId, event: &SyncEvent) {
        self.subscribers.retain(|_, (owner, sender)| {
            if owner != workspace {
                return true;
            }
            // 通道断开说明订阅线程已退出；顺手清理，避免总线随连接数膨胀。
            sender.send(event.clone()).is_ok()
        });
    }

    /// 当前订阅数（诊断/测试用）。
    #[must_use]
    pub fn len(&self) -> usize {
        self.subscribers.len()
    }

    /// 当前是否没有订阅者。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.subscribers.is_empty()
    }
}

/// 活跃订阅的生命周期句柄：`Drop` 即注销，保证 WS 断开后无残留通道。
#[derive(Debug)]
pub struct ActiveSubscription {
    /// 与 `SyncService` 共享的总线。
    hub: Arc<Mutex<SubscriptionHub>>,
    /// 本订阅在总线中的 ID。
    id: u64,
}

impl Drop for ActiveSubscription {
    fn drop(&mut self) {
        // Mutex 被其它线程 panic 污染时仍要尽力注销：这里取回内部数据继续操作。
        let mut hub = self
            .hub
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        hub.unsubscribe(self.id);
    }
}

/// 一次订阅会话：事件接收端 + 生命周期句柄（句柄 `Drop` 即注销）。
#[derive(Debug)]
pub struct SubscriptionSession {
    /// WS 升级循环从这里取事件并编码成文本帧。
    pub receiver: Receiver<SyncEvent>,
    /// 不公开句柄，但必须持有以在会话结束时注销。
    _handle: ActiveSubscription,
}

impl SubscriptionSession {
    /// 由已鉴权的订阅入口构造（仅 crate 内部使用）。
    pub(crate) fn new(receiver: Receiver<SyncEvent>, handle: ActiveSubscription) -> Self {
        Self {
            receiver,
            _handle: handle,
        }
    }
}

/// 由已鉴权的 `SyncService::subscribe` 构造生命周期句柄。
///
/// 不对外暴露构造入口，避免绕过鉴权直接登记订阅。
pub(crate) fn new_subscription(hub: &Arc<Mutex<SubscriptionHub>>, id: u64) -> ActiveSubscription {
    ActiveSubscription {
        hub: Arc::clone(hub),
        id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event() -> SyncEvent {
        SyncEvent::WorkspaceChanged {
            workspaces: vec!["ws-1".into()],
            doc_ids: vec!["a.md".into()],
            cursor_hint: Some(3),
        }
    }

    #[test]
    fn publish_only_reaches_matching_workspace() {
        let mut hub = SubscriptionHub::default();
        let (tx1, rx1) = std::sync::mpsc::channel();
        let (tx2, rx2) = std::sync::mpsc::channel();
        let ws1 = WorkspaceId::new("ws-1").unwrap();
        let ws2 = WorkspaceId::new("ws-2").unwrap();
        let id1 = hub.subscribe(ws1.clone(), tx1);
        let id2 = hub.subscribe(ws2.clone(), tx2);
        let notification = event();
        hub.publish(&ws1, &notification);
        assert!(rx1.try_recv().is_ok());
        assert!(rx2.try_recv().is_err());
        hub.unsubscribe(id1);
        hub.unsubscribe(id2);
        assert!(hub.is_empty());
    }

    #[test]
    fn disconnected_receiver_is_cleaned_on_publish() {
        let mut hub = SubscriptionHub::default();
        let ws = WorkspaceId::new("ws-1").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        hub.subscribe(ws.clone(), tx);
        drop(rx);
        let notification = event();
        hub.publish(&ws, &notification);
        assert!(hub.is_empty());
    }
}
