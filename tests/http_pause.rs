#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test ergonomics"
)]

use std::sync::Arc;
use std::time::Duration;

use mcp_router::daemon::approval::ApprovalCenter;
use mcp_router::daemon::db::Db;
use mcp_router::daemon::events::Bus;
use mcp_router::daemon::http_server::{serve, AppState};
use mcp_router::daemon::upstream::manager::UpstreamManager;
use mcp_router::paths::Paths;
use tokio_util::sync::CancellationToken;

fn pick_free_port() -> u16 {
    let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    l.local_addr().unwrap().port()
}

#[tokio::test]
async fn paused_router_returns_daemon_paused() {
    let db = Db::spawn_in_memory().unwrap();
    db.set_settings_kv("routing_paused", "true").await.unwrap();

    let port = pick_free_port();
    let bus = Bus::new();
    let cancel = CancellationToken::new();
    let approvals = Arc::new(ApprovalCenter::new(
        db.clone(),
        bus.clone(),
        Duration::from_secs(30),
    ));
    let dir = std::env::temp_dir().join(format!("mcp-pause-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let upstream = Arc::new(UpstreamManager::new(Paths::under(&dir), cancel.clone()));
    let state = AppState {
        db,
        approvals,
        upstream,
        port,
    };

    let serve_cancel = cancel.clone();
    let serve_handle = tokio::spawn(async move { serve(state, serve_cancel).await });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let url = format!("http://127.0.0.1:{port}/whatever/whoever/mcp");
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize"}))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], -32004);
    assert_eq!(body["error"]["message"], "daemon_paused");

    cancel.cancel();
    let _ignored = tokio::time::timeout(Duration::from_secs(2), serve_handle).await;
    let _ignored = std::fs::remove_dir_all(&dir);
}
