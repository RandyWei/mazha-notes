//! `mazha-sync-server` 二进制入口：最小持久化同步中继（D100）。
//!
//! 用法示例：
//! ```text
//! mazha-sync-server --bind 127.0.0.1:8787 --store .mazha-sync \
//!     --token ws-1=dev-token --token 工作区甲=另一个token
//! ```
//! `--token <workspace>=<token>` 可重复，表示某 token 可访问的工作区。
//! 公网部署请在前面放反向代理终结 TLS；本进程只监听明文 HTTP。

use std::path::PathBuf;

use mazha_sync::WorkspaceId;
use mazha_sync_server::{FileSyncStore, SyncService, TokenAuth, TokenRule, run_server};

/// 解析命令行并启动阻塞式 HTTP 服务。
fn main() {
    if let Err(error) = run() {
        eprintln!("启动同步服务端失败：{error}");
        std::process::exit(1);
    }
}

/// 实际入口：参数解析 → 构造存储/鉴权/服务 → 阻塞监听。
///
/// # Errors
/// 参数缺失或非法、地址绑定失败时返回错误字符串。
fn run() -> Result<(), String> {
    let mut bind = "127.0.0.1:8787".to_owned();
    let mut store_root = PathBuf::from(".mazha-sync");
    let mut rules = Vec::new();
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--bind" => bind = args.next().ok_or("--bind 缺少地址")?,
            "--store" => store_root = PathBuf::from(args.next().ok_or("--store 缺少目录")?),
            "--token" => {
                // 单个 token 绑定单个工作区；多个工作区可重复传多个 --token。
                let spec = args.next().ok_or("--token 缺少 <workspace>=<token>")?;
                let (workspace, token) = spec.split_once('=').ok_or_else(|| {
                    format!("--token 格式应为 <workspace>=<token>，实际是 {spec}")
                })?;
                let workspace = WorkspaceId::new(workspace)
                    .map_err(|error| format!("工作区 ID 非法：{error}"))?;
                rules.push(TokenRule {
                    token: token.to_owned(),
                    workspaces: vec![workspace],
                });
            }
            "--help" | "-h" => {
                println!(
                    "用法：mazha-sync-server [--bind <addr>] [--store <dir>] --token <workspace>=<token> ..."
                );
                return Ok(());
            }
            other => return Err(format!("未知参数：{other}")),
        }
    }
    if rules.is_empty() {
        return Err("至少需要一个 --token <workspace>=<token>".to_owned());
    }

    let service = SyncService::new(FileSyncStore::new(store_root), TokenAuth::new(rules));
    eprintln!("码札同步服务端监听 {bind}（TLS 由反向代理终结）");
    run_server(&service, &bind)
}
