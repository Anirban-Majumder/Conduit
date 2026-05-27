#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test ergonomics"
)]

use std::sync::Arc;
use std::time::Duration;

use conduit::daemon::approval::{ApprovalCenter, ApprovalRequest};
use conduit::daemon::db::types::{
    ApprovalStatus, Decision, DecisionSource, PendingApproval, Server,
};
use conduit::daemon::db::Db;
use conduit::daemon::events::{Bus, Event};
use conduit::gui::notification_router::{dispatch_action, NotificationAction};

fn sample_server(name: &str) -> Server {
    Server {
        name: name.to_string(),
        transport: "stdio".to_string(),
        command: Some("echo".to_string()),
        args: vec![],
        env: vec![],
        url: None,
        lifecycle_seconds: 300,
        last_init_duration_ms: None,
        package_version: None,
        mcp_self_version: None,
        created_at: 0,
        server_name: None,
        server_version: None,
        last_probed_at: None,
        last_probe_error: None,
    }
}

async fn fresh_pending(db: &Db, id: &str, server: &str, tool: &str) {
    db.upsert_server(sample_server(server)).await.unwrap();
    db.create_pending_approval(PendingApproval {
        id: id.to_string(),
        server: server.to_string(),
        tool: tool.to_string(),
        args_json: "{}".to_string(),
        ide_key: "k1234567".to_string(),
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
        Duration::from_secs(60),
    ));
    fresh_pending(&db, "uuid-1", "github", "create_issue").await;

    dispatch_action(
        NotificationAction::Approve("uuid-1".to_string()),
        &db,
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
        Duration::from_secs(60),
    ));
    fresh_pending(&db, "uuid-2", "github", "delete_repo").await;

    dispatch_action(
        NotificationAction::Deny("uuid-2".to_string()),
        &db,
        &approvals,
        &bus,
    )
    .await;

    let row = db.get_pending_approval("uuid-2").await.unwrap().unwrap();
    assert_eq!(row.status, ApprovalStatus::Denied);
}

#[tokio::test]
async fn approve_always_resolves_then_writes_policy_with_notification_source() {
    let db = Db::spawn_in_memory().unwrap();
    let bus = Bus::new();
    let approvals = Arc::new(ApprovalCenter::new(
        db.clone(),
        bus.clone(),
        Duration::from_secs(60),
    ));
    fresh_pending(&db, "uuid-3", "github", "list_issues").await;

    dispatch_action(
        NotificationAction::ApproveAlways {
            id: "uuid-3".to_string(),
            server: "github".to_string(),
            tool: "list_issues".to_string(),
        },
        &db,
        &approvals,
        &bus,
    )
    .await;

    let row = db.get_pending_approval("uuid-3").await.unwrap().unwrap();
    assert_eq!(row.status, ApprovalStatus::Approved);

    let policy = db
        .get_policy("github", "list_issues")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(policy.decision, Decision::Allow);
    assert_eq!(policy.decision_source, DecisionSource::Notification);
}

#[tokio::test]
async fn focus_action_publishes_focus_approval_event() {
    let db = Db::spawn_in_memory().unwrap();
    let bus = Bus::new();
    let mut rx = bus.subscribe();
    let approvals = Arc::new(ApprovalCenter::new(
        db.clone(),
        bus.clone(),
        Duration::from_secs(60),
    ));

    dispatch_action(
        NotificationAction::Focus("uuid-4".to_string()),
        &db,
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
        Duration::from_secs(60),
    ));
    fresh_pending(&db, "uuid-5", "github", "x").await;
    approvals
        .resolve("uuid-5", ApprovalStatus::Approved)
        .await
        .unwrap();

    // Now dispatch Deny on the already-resolved id: should be a no-op.
    dispatch_action(
        NotificationAction::Deny("uuid-5".to_string()),
        &db,
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
    // Helper so unused-import lint doesn't fire on ApprovalRequest:
    let _ = std::marker::PhantomData::<ApprovalRequest>;
}
