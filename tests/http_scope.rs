//! DS-7: per-IDE server scoping — a client that is NOT in `managed_mcp` for a
//! server must get `server_not_found`, even if the server exists and another
//! client manages it.

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
use conduit::daemon::db::types::McpServer;
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

fn stub_server(name: &str) -> McpServer {
    McpServer {
        name: name.into(),
        transport: "stdio".into(),
        command: Some("echo".into()),
        args: vec![],
        env: vec![],
        url: None,
        lifecycle_seconds: -1,
        reported_name: None,
        reported_version: None,
        last_probed_at: None,
        last_probe_error: None,
        last_auth_error_msg: None,
    }
}

fn make_state(db: Db, port: u16) -> (AppState, CancellationToken) {
    let bus = Bus::new();
    let cancel = CancellationToken::new();
    let settings = default_settings();
    let approvals = Arc::new(ApprovalCenter::new(
        db.clone(),
        bus.clone(),
        settings.clone(),
    ));
    let dir = std::env::temp_dir().join(format!("mcp-scope-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let upstream = Arc::new(UpstreamManager::new(Paths::under(&dir), cancel.clone()));
    let state = AppState {
        db,
        approvals,
        upstream,
        bus,
        port,
        settings,
    };
    (state, cancel)
}

/// A client that manages the server is allowed through to routing.
/// The upstream won't be running so it returns an INTERNAL error, but the
/// point is that it does NOT return `server_not_found`.
#[tokio::test]
async fn client_that_manages_server_is_not_blocked() {
    let db = Db::spawn_in_memory().unwrap();

    db.upsert_server_with_tools(stub_server("github"), vec![])
        .await
        .unwrap();

    // Project a client and assign it the server.
    let schema_json = serde_json::to_string(&serde_json::json!({})).unwrap();
    let client_row = db
        .project_builtin(
            "Cursor",
            "Cursor",
            "/tmp/mcp.json",
            None,
            &schema_json,
            None,
            None,
        )
        .await
        .unwrap();
    db.set_managed_servers("Cursor", "global", vec!["github".to_string()])
        .await
        .unwrap();

    let port = pick_free_port();
    let (state, cancel) = make_state(db, port);
    let serve_cancel = cancel.clone();
    let _handle = tokio::spawn(async move { serve(state, serve_cancel).await });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let url = format!("http://127.0.0.1:{port}/{}/github/mcp", client_row.mcp_key);
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();

    // server_not_found (-32002) must NOT be returned — the client manages this server.
    assert_ne!(
        body["error"]["code"], -32002,
        "client that manages server should not get server_not_found; got: {body}"
    );

    cancel.cancel();
}

/// A client that does NOT manage a server gets `server_not_found`, even though
/// the server exists (managed by a different client).
#[tokio::test]
async fn client_that_does_not_manage_server_gets_not_found() {
    let db = Db::spawn_in_memory().unwrap();

    db.upsert_server_with_tools(stub_server("github"), vec![])
        .await
        .unwrap();

    // Project two clients. Only "Cursor" manages "github"; "VSCode" does not.
    let schema_json = serde_json::to_string(&serde_json::json!({})).unwrap();
    db.project_builtin(
        "Cursor",
        "Cursor",
        "/tmp/mcp.json",
        None,
        &schema_json,
        None,
        None,
    )
    .await
    .unwrap();
    db.set_managed_servers("Cursor", "global", vec!["github".to_string()])
        .await
        .unwrap();

    let vscode_row = db
        .project_builtin(
            "VSCode",
            "VSCode",
            "/tmp/vscode-mcp.json",
            None,
            &schema_json,
            None,
            None,
        )
        .await
        .unwrap();
    // VSCode manages no servers.

    let port = pick_free_port();
    let (state, cancel) = make_state(db, port);
    let serve_cancel = cancel.clone();
    let _handle = tokio::spawn(async move { serve(state, serve_cancel).await });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let url = format!("http://127.0.0.1:{port}/{}/github/mcp", vscode_row.mcp_key);
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();

    assert_eq!(
        body["error"]["code"], -32002,
        "client that does not manage server must get server_not_found; got: {body}"
    );
    assert_eq!(body["error"]["message"], "server_not_found");

    cancel.cancel();
}
