#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test ergonomics"
)]

use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use conduit::daemon::config::toml::Settings;
use conduit::daemon::config::SharedSettings;
use conduit::daemon::db::types::{ApprovalStatus, McpServer, PendingApproval};
use conduit::daemon::db::Db;
use conduit::daemon::downstream::approval::ApprovalCenter;
use conduit::daemon::events::{Bus, Event};
use conduit::os::notification_router::{dispatch_action, NotificationAction};
use tokio_util::sync::CancellationToken;

fn default_settings() -> SharedSettings {
    Arc::new(ArcSwap::from_pointee(Settings::default()))
}

fn sample_server(name: &str) -> McpServer {
    McpServer {
        name: name.to_string(),
        transport: "stdio".to_string(),
        command: Some("echo".to_string()),
        args: vec![],
        env: vec![],
        url: None,
        lifecycle_seconds: 300,
        reported_name: None,
        reported_version: None,
        last_probed_at: None,
        last_probe_error: None,
        last_auth_error_msg: None,
    }
}

async fn fresh_pending(db: &Db, id: &str, server: &str, tool: &str) {
    use conduit::daemon::db::types::McpTool;
    // Seed the server + the advertised tool so a later set_tool_decision
    // (UPDATE on the mcp_tool row) actually persists.
    db.upsert_server_with_tools(
        sample_server(server),
        vec![McpTool {
            server: server.to_string(),
            name: tool.to_string(),
            description: None,
            input_schema_json: "{}".to_string(),
            discovered_at: 0,
            decision: None,
        }],
    )
    .await
    .unwrap();
    db.create_pending_approval(PendingApproval {
        id: id.to_string(),
        mcp_server: server.to_string(),
        tool: tool.to_string(),
        args_json: "{}".to_string(),
        client_key: "k1234567".to_string(),
        created_at: 0,
        status: ApprovalStatus::Pending,
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn approve_action_resolves_pending() {
    let db = Db::spawn_in_memory().unwrap();
    let bus = Bus::new();
    let approvals = Arc::new(ApprovalCenter::new(
        db.clone(),
        bus.clone(),
        default_settings(),
        CancellationToken::new(),
    ));
    fresh_pending(&db, "uuid-1", "github", "create_issue").await;

    dispatch_action(
        NotificationAction::Approve("uuid-1".to_string()),
        &approvals,
        &bus,
    )
    .await;

    let row = db.get_pending_approval("uuid-1").await.unwrap().unwrap();
    assert_eq!(row.status, ApprovalStatus::Approved);
}

#[tokio::test]
async fn deny_action_resolves_pending() {
    let db = Db::spawn_in_memory().unwrap();
    let bus = Bus::new();
    let approvals = Arc::new(ApprovalCenter::new(
        db.clone(),
        bus.clone(),
        default_settings(),
        CancellationToken::new(),
    ));
    fresh_pending(&db, "uuid-2", "github", "delete_repo").await;

    dispatch_action(
        NotificationAction::Deny("uuid-2".to_string()),
        &approvals,
        &bus,
    )
    .await;

    let row = db.get_pending_approval("uuid-2").await.unwrap().unwrap();
    assert_eq!(row.status, ApprovalStatus::Denied);
}

#[tokio::test]
async fn focus_action_publishes_focus_approval_event() {
    let db = Db::spawn_in_memory().unwrap();
    let bus = Bus::new();
    let mut rx = bus.subscribe();
    let approvals = Arc::new(ApprovalCenter::new(
        db.clone(),
        bus.clone(),
        default_settings(),
        CancellationToken::new(),
    ));

    dispatch_action(
        NotificationAction::Focus("uuid-4".to_string()),
        &approvals,
        &bus,
    )
    .await;

    match tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .unwrap()
        .unwrap()
    {
        Event::FocusApproval { id } => assert_eq!(id, "uuid-4"),
        other => panic!("expected FocusApproval, got {other:?}"),
    }
}

#[tokio::test]
async fn stale_action_is_a_noop() {
    let db = Db::spawn_in_memory().unwrap();
    let bus = Bus::new();
    let approvals = Arc::new(ApprovalCenter::new(
        db.clone(),
        bus.clone(),
        default_settings(),
        CancellationToken::new(),
    ));
    fresh_pending(&db, "uuid-5", "github", "x").await;
    approvals
        .resolve("uuid-5", ApprovalStatus::Approved)
        .await
        .unwrap();

    // Now dispatch Deny on the already-resolved id: should be a no-op.
    dispatch_action(
        NotificationAction::Deny("uuid-5".to_string()),
        &approvals,
        &bus,
    )
    .await;

    let row = db.get_pending_approval("uuid-5").await.unwrap().unwrap();
    assert_eq!(
        row.status,
        ApprovalStatus::Approved,
        "stale deny must not overwrite Approved"
    );
}
