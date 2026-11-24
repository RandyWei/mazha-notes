//! S3 真实 HTTP 全链路集成测试（D100，契约见 `docs/SYNC-SPEC.md` §7.4）。
//!
//! 用 `127.0.0.1:0` 的临时端口启动真实 `tiny_http` 服务，客户端侧用
//! `mazha-sync-http::HttpTransport` + `SyncSession` 走完全部四个端点，
//! 覆盖 hello/pull/push/snapshot、重复批次幂等、401/400 映射、
//! 断线 outbox 重放与 CJK 标识/正文。全部只用本地回环，不依赖外部网络。

use std::collections::BTreeMap;
use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::thread;

use mazha_crdt::{CrdtDoc, CrdtRoster};
use mazha_sync::{
    BatchId, DeviceId, HelloPayload, PushPayload, SyncCapability, SyncEnvelope, SyncErrorCode,
    SyncSession, SyncTransport, TransportError, WorkspaceId,
};
use mazha_sync_http::HttpTransport;
use mazha_sync_server::{FileSyncStore, SyncService, SyncStore, TokenAuth, TokenRule, serve};

/// 每个测试独立的临时存储目录（进程 ID + 测试名避免并发冲突）。
fn temp_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "mazha-http-{name}-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("创建临时存储目录");
    root
}

/// 启动真实本地 HTTP 服务，返回 base URL。
///
/// `serve` 阻塞处理请求直到进程结束；测试不 join 该线程，进程退出时一并终止。
/// 端口由内核分配，避免测试间抢占固定端口导致 flaky。
fn start_server(root: &Path) -> String {
    let auth = TokenAuth::new([TokenRule {
        token: "token-a".to_owned(),
        workspaces: vec![WorkspaceId::new("工作区甲").unwrap()],
    }]);
    let service = SyncService::new(FileSyncStore::new(root.to_path_buf()), auth);
    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定本地临时端口");
    let addr = listener.local_addr().expect("读取监听地址");
    thread::spawn(move || {
        let _ = serve(&service, listener);
    });
    format!("http://{addr}")
}

/// 构造端到端测试用的类型化 ID。
fn ids() -> (WorkspaceId, DeviceId, DeviceId) {
    (
        WorkspaceId::new("工作区甲").unwrap(),
        DeviceId::new("设备甲").unwrap(),
        DeviceId::new("设备乙").unwrap(),
    )
}

#[test]
fn http_roundtrip_syncs_cjk_documents_and_persists_across_reload() {
    let root = temp_root("roundtrip");
    let base_url = start_server(&root);
    let (workspace, device_a, device_b) = ids();

    // 设备甲：离线暂存中文文档，再一次性握手 + 拉取 + 推送。
    let mut transport_a =
        HttpTransport::new(&base_url, workspace.clone(), device_a.clone(), "token-a");
    let mut session_a = SyncSession::new(workspace.clone(), device_a, CrdtRoster::new());
    session_a
        .stage_text("笔记/甲.md", "中文正文甲")
        .expect("暂存成功");
    let once = session_a.sync_once(&mut transport_a).expect("同步成功");
    assert_eq!(once.push.expect("已推送").confirmed.len(), 1);
    assert_eq!(session_a.last_ack_cursor().get(), 1);

    // 设备乙：新设备同步，应看到甲推送的中文文档。
    let mut transport_b =
        HttpTransport::new(&base_url, workspace.clone(), device_b.clone(), "token-a");
    let mut session_b = SyncSession::new(workspace.clone(), device_b, CrdtRoster::new());
    session_b
        .sync_once(&mut transport_b)
        .expect("新设备同步成功");
    let doc = session_b
        .roster()
        .get("笔记/甲.md")
        .expect("设备乙应看到甲的文档");
    assert_eq!(doc.text(), "中文正文甲");

    // 直接重新加载磁盘状态，验证重启后游标与内容帧仍在。
    let reloaded = FileSyncStore::new(root.clone())
        .load(&workspace)
        .expect("磁盘状态可加载");
    assert_eq!(reloaded.cursor.get(), 1);
    assert!(reloaded.frames.contains_key("笔记/甲.md"));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn http_unauthorized_and_bad_frame_map_to_protocol_errors_without_mutation() {
    let root = temp_root("errors");
    let base_url = start_server(&root);
    let (workspace, device, _) = ids();

    // 错误 token：HTTP 401 经客户端映射为 Unauthorized 协议错误。
    // 凭证只出现在 ASCII HTTP header 中，故用 ASCII 错误值；CJK 身份/正文由其它用例覆盖。
    let mut bad_token =
        HttpTransport::new(&base_url, workspace.clone(), device.clone(), "wrong-token");
    let hello = SyncEnvelope::new(
        workspace.clone(),
        device.clone(),
        HelloPayload {
            capabilities: vec![SyncCapability::ContentV1],
        },
    );
    let error = bad_token.hello(hello).expect_err("错误凭证应被拒绝");
    assert!(
        matches!(
            error,
            TransportError::Server {
                code: SyncErrorCode::Unauthorized,
                ..
            }
        ),
        "实际错误：{error:?}"
    );

    // 合法 token 但内容帧损坏：HTTP 400 映射为 BadFrame，服务端状态零改动。
    let before = FileSyncStore::new(root.clone())
        .load(&workspace)
        .expect("读取初始状态");
    let mut frames = BTreeMap::new();
    frames.insert("坏.md".to_owned(), vec![0xff, 0xff, 0xff]);
    let bad_push = SyncEnvelope::new(
        workspace.clone(),
        device.clone(),
        PushPayload {
            batch_id: BatchId::new(&device, 1).unwrap(),
            frames,
            meta_changes: Vec::new(),
        },
    );
    let mut transport = HttpTransport::new(&base_url, workspace.clone(), device, "token-a");
    let error = transport.push(bad_push).expect_err("坏帧应被拒绝");
    assert!(matches!(
        error,
        TransportError::Server {
            code: SyncErrorCode::BadFrame,
            ..
        }
    ));
    let after = FileSyncStore::new(root.clone())
        .load(&workspace)
        .expect("读取失败后状态");
    assert_eq!(after, before);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn http_duplicate_batch_returns_first_ack_and_adds_no_log_entry() {
    let root = temp_root("duplicate");
    let base_url = start_server(&root);
    let (workspace, device, _) = ids();
    let mut frames = BTreeMap::new();
    frames.insert(
        "幂等.md".to_owned(),
        CrdtDoc::from_text("同一批次").encode_update(),
    );
    let request = SyncEnvelope::new(
        workspace.clone(),
        device.clone(),
        PushPayload {
            batch_id: BatchId::new(&device, 7).unwrap(),
            frames,
            meta_changes: Vec::new(),
        },
    );
    let mut transport = HttpTransport::new(&base_url, workspace.clone(), device, "token-a");
    let first = transport.push(request.clone()).expect("首次推送成功");
    let second = transport.push(request).expect("重复推送返回首次 ack");
    assert!(!first.duplicate);
    assert!(second.duplicate);
    assert_eq!(first.cursor, second.cursor);

    let state = FileSyncStore::new(root.clone())
        .load(&workspace)
        .expect("读取状态");
    assert_eq!(state.log.len(), 1);
    assert_eq!(state.dedup.len(), 1);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn http_outbox_replays_after_reconnect() {
    let root = temp_root("reconnect");
    let base_url = start_server(&root);
    let (workspace, device, other) = ids();

    // 故意指向几乎不可能有服务的端口：握手失败，本地改动留在 outbox。
    let mut offline = HttpTransport::new(
        "http://127.0.0.1:1",
        workspace.clone(),
        device.clone(),
        "token-a",
    );
    let mut session = SyncSession::new(workspace.clone(), device.clone(), CrdtRoster::new());
    session
        .stage_text("离线/草稿.md", "离线写的中文")
        .expect("离线暂存成功");
    assert!(session.sync_once(&mut offline).is_err());
    assert_eq!(session.pending_batches().len(), 1);

    // 重连到真实服务：重新握手并重放 outbox，远端状态与新会话一致。
    let mut online = HttpTransport::new(&base_url, workspace.clone(), device, "token-a");
    let once = session.sync_once(&mut online).expect("重连后同步成功");
    assert_eq!(once.push.expect("已重放").confirmed.len(), 1);
    assert!(session.pending_batches().is_empty());

    let mut peer_transport =
        HttpTransport::new(&base_url, workspace.clone(), other.clone(), "token-a");
    let mut peer = SyncSession::new(workspace.clone(), other, CrdtRoster::new());
    peer.sync_once(&mut peer_transport).expect("对端拉取成功");
    assert_eq!(
        peer.roster()
            .get("离线/草稿.md")
            .expect("对端应看到重放内容")
            .text(),
        "离线写的中文"
    );
    let _ = fs::remove_dir_all(&root);
}
