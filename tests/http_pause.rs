#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test ergonomics"
)]

use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use conduit::daemon::config::toml::Settings;
use conduit::daemon::config::SharedSettings;
use conduit::daemon::db::Db;
use conduit::daemon::downstream::approval::ApprovalCenter;
use conduit::daemon::downstream::http_server::{serve, AppState};
use conduit::daemon::events::Bus;
use conduit::daemon::paths::Paths;
use conduit::daemon::upstream::manager::UpstreamManager;
use tokio_util::sync::CancellationToken;

fn default_settings() -> SharedSettings {
    Arc::new(ArcSwap::from_pointee(Settings::default()))
}

fn pick_free_port() -> u16 {
    let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    l.local_addr().unwrap().port()
}

#[tokio::test]
async fn paused_router_returns_daemon_paused() {
    let db = Db::spawn_in_memory().unwrap();
    db.set_settings_kv("routing_paused", "true").await.unwrap();

    // Key validation now precedes the routing_paused check (DS-6 second half).
    // Insert a real client so the key lookup succeeds and we reach the paused check.
    let client = db
        .project_builtin(
            "test-ide",
            "Test IDE",
            "/tmp/test.json",
            None,
            "{}",
            None,
            None,
        )
        .await
        .unwrap();

    let port = pick_free_port();
    let bus = Bus::new();
    let cancel = CancellationToken::new();
    let settings = default_settings();
    let approvals = Arc::new(ApprovalCenter::new(
        db.clone(),
        bus.clone(),
        settings.clone(),
        cancel.clone(),
    ));
    let dir = std::env::temp_dir().join(format!("mcp-pause-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let upstream = Arc::new(UpstreamManager::new(Paths::under(&dir), cancel.clone()));
    let state = AppState {
        db,
        approvals,
        upstream,
        bus: bus.clone(),
        port,
        settings,
    };

    let serve_cancel = cancel.clone();
    let serve_handle = tokio::spawn(async move { serve(state, serve_cancel).await });
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Use the real mcp_key so key validation passes and we reach the paused check.
    let url = format!("http://127.0.0.1:{port}/{}/whoever/mcp", client.mcp_key);
    let resp = reqwest::Client::new()
        .post(&url)
        .header("host", format!("127.0.0.1:{port}"))
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
