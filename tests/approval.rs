#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test ergonomics"
)]

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use conduit::daemon::db::types::{ApprovalStatus, PendingApproval};
use conduit::daemon::db::Db;
use conduit::daemon::downstream::approval::{
    spawn_timeout_sweeper, sweep_and_publish_timeouts, ApprovalCenter, ApprovalOutcome,
    ApprovalRequest,
};
use conduit::daemon::events::{Bus, Event};
use tokio::time::timeout as wait_for;
use tokio_util::sync::CancellationToken;

const SHORT: Duration = Duration::from_millis(200);

fn now_unix() -> i64 {
    let s = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    i64::try_from(s).unwrap()
}

fn sample_req() -> ApprovalRequest {
    ApprovalRequest {
        mcp_server: "gmail".into(),
        tool: "send_email".into(),
        args_json: "{\"to\":\"x@y\"}".into(),
        client_key: "abc12345".into(),
    }
}

async fn first_pending_id(center: &ApprovalCenter) -> String {
    // poll briefly; request() inserts the row synchronously before publishing,
    // but the publish is fire-and-forget across tokio tasks.
    for _ in 0..50 {
        let rows = center.db().list_pending_approvals().await.unwrap();
        if let Some(row) = rows.first() {
            return row.id.clone();
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("no pending approval appeared");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_returns_approved_when_resolved_approve() {
    let db = Db::spawn_in_memory().unwrap();
    let bus = Bus::new();
    let center = ApprovalCenter::new(db, bus, Duration::from_secs(5));

    let req_fut = {
        let center_ref = &center;
        async move { center_ref.request(sample_req()).await }
    };

    let resolver = async {
        let id = first_pending_id(&center).await;
        center.resolve(&id, ApprovalStatus::Approved).await.unwrap();
    };

    let (outcome, ()) = tokio::join!(req_fut, resolver);
    assert_eq!(outcome.unwrap(), ApprovalOutcome::Approved);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_returns_denied_when_resolved_deny() {
    let db = Db::spawn_in_memory().unwrap();
    let bus = Bus::new();
    let center = ApprovalCenter::new(db, bus, Duration::from_secs(5));

    let req_fut = {
        let center_ref = &center;
        async move { center_ref.request(sample_req()).await }
    };

    let resolver = async {
        let id = first_pending_id(&center).await;
        center.resolve(&id, ApprovalStatus::Denied).await.unwrap();
    };

    let (outcome, ()) = tokio::join!(req_fut, resolver);
    assert_eq!(outcome.unwrap(), ApprovalOutcome::Denied);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_returns_timeout_when_no_resolution() {
    let db = Db::spawn_in_memory().unwrap();
    let bus = Bus::new();
    let center = ApprovalCenter::new(db, bus, SHORT);

    let outcome = wait_for(Duration::from_secs(2), center.request(sample_req()))
        .await
        .expect("request did not return within hard deadline")
        .unwrap();
    assert_eq!(outcome, ApprovalOutcome::Timeout);

    // DB row should now be Timeout, not Pending.
    let rows = center.db().list_pending_approvals().await.unwrap();
    assert!(
        rows.is_empty(),
        "list_pending_approvals should only return status=pending"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_persists_pending_row_before_resolution() {
    let db = Db::spawn_in_memory().unwrap();
    let bus = Bus::new();
    let center = ApprovalCenter::new(db, bus, Duration::from_secs(5));

    let req_fut = {
        let center_ref = &center;
        async move { center_ref.request(sample_req()).await }
    };

    let inspector = async {
        let id = first_pending_id(&center).await;
        let row = center
            .db()
            .get_pending_approval(&id)
            .await
            .unwrap()
            .expect("row present");
        assert_eq!(row.status, ApprovalStatus::Pending);
        assert_eq!(row.mcp_server, "gmail");
        assert_eq!(row.tool, "send_email");
        center.resolve(&id, ApprovalStatus::Approved).await.unwrap();
    };

    let (outcome, ()) = tokio::join!(req_fut, inspector);
    assert_eq!(outcome.unwrap(), ApprovalOutcome::Approved);
}

#[tokio::test]
async fn resolve_unknown_id_is_ok() {
    let db = Db::spawn_in_memory().unwrap();
    let bus = Bus::new();
    let center = ApprovalCenter::new(db, bus, Duration::from_secs(5));

    center
        .resolve("does-not-exist", ApprovalStatus::Approved)
        .await
        .expect("idempotent resolve");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approval_pending_event_published_before_waiting() {
    let db = Db::spawn_in_memory().unwrap();
    let bus = Bus::new();
    let mut rx = bus.subscribe();
    let center = ApprovalCenter::new(db, bus, Duration::from_secs(5));

    let req_fut = {
        let center_ref = &center;
        async move { center_ref.request(sample_req()).await }
    };

    let watcher = async {
        // The pending event must arrive before any resolution.
        let evt = wait_for(Duration::from_secs(2), rx.recv())
            .await
            .expect("event received in time")
            .expect("bus open");
        match evt {
            Event::ApprovalPending {
                id,
                mcp_server,
                tool,
                client_key,
            } => {
                assert_eq!(mcp_server, "gmail");
                assert_eq!(tool, "send_email");
                assert_eq!(client_key, "abc12345");
                center.resolve(&id, ApprovalStatus::Approved).await.unwrap();
            }
            other => panic!("expected ApprovalPending, got {other:?}"),
        }
    };

    let (outcome, ()) = tokio::join!(req_fut, watcher);
    assert_eq!(outcome.unwrap(), ApprovalOutcome::Approved);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approval_resolved_event_fires_on_resolve() {
    let db = Db::spawn_in_memory().unwrap();
    let bus = Bus::new();
    let mut rx = bus.subscribe();
    let center = ApprovalCenter::new(db, bus, Duration::from_secs(5));

    let req_fut = {
        let center_ref = &center;
        async move { center_ref.request(sample_req()).await }
    };

    let watcher = async {
        let mut resolved_seen = false;
        let mut resolved_id: Option<String> = None;
        for _ in 0..10 {
            let evt = wait_for(Duration::from_secs(2), rx.recv())
                .await
                .expect("event received in time")
                .expect("bus open");
            match evt {
                Event::ApprovalPending { id, .. } => {
                    center.resolve(&id, ApprovalStatus::Denied).await.unwrap();
                    resolved_id = Some(id);
                }
                Event::ApprovalResolved { id, status } => {
                    assert_eq!(status, ApprovalStatus::Denied);
                    assert_eq!(Some(id), resolved_id);
                    resolved_seen = true;
                    break;
                }
                Event::ConfigReloaded { .. }
                | Event::ServerHealthChanged { .. }
                | Event::FocusApproval { .. }
                | Event::SkillsChanged
                | Event::AuthFailure { .. }
                | Event::AuthCleared { .. }
                | Event::AuditAppended => {}
            }
        }
        assert!(resolved_seen, "no ApprovalResolved event observed");
    };

    let (outcome, ()) = tokio::join!(req_fut, watcher);
    assert_eq!(outcome.unwrap(), ApprovalOutcome::Denied);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolve_to_pending_rejected() {
    let db = Db::spawn_in_memory().unwrap();
    let bus = Bus::new();
    let center = ApprovalCenter::new(db, bus, Duration::from_secs(5));
    let err = center
        .resolve("whatever", ApprovalStatus::Pending)
        .await
        .expect_err("resolving to Pending is invalid");
    assert!(matches!(
        err,
        conduit::daemon::downstream::approval::ApprovalError::InvalidStatus
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sweeper_flips_pre_existing_stale_rows_to_timeout() {
    let db = Db::spawn_in_memory().unwrap();

    // Plant a stale pending row created 1h ago.
    let stale_id = "stale-1".to_string();
    db.create_pending_approval(PendingApproval {
        id: stale_id.clone(),
        mcp_server: "s".into(),
        tool: "t".into(),
        args_json: "{}".into(),
        client_key: "k1234567".into(),
        created_at: now_unix() - 3600,
        status: ApprovalStatus::Pending,
    })
    .await
    .unwrap();

    // And a fresh one created "now" — should NOT be flipped.
    let fresh_id = "fresh-1".to_string();
    db.create_pending_approval(PendingApproval {
        id: fresh_id.clone(),
        mcp_server: "s".into(),
        tool: "t".into(),
        args_json: "{}".into(),
        client_key: "k1234567".into(),
        created_at: now_unix(),
        status: ApprovalStatus::Pending,
    })
    .await
    .unwrap();

    // Sanity: both visible as pending.
    let before = db.list_pending_approvals().await.unwrap();
    assert_eq!(before.len(), 2);

    // Drive the sweep directly to avoid depending on the 30s interval.
    let cutoff = now_unix() - 60;
    db.sweep_stale_pending_approvals(cutoff).await.unwrap();

    let stale = db
        .get_pending_approval(&stale_id)
        .await
        .unwrap()
        .expect("stale row");
    assert_eq!(stale.status, ApprovalStatus::Timeout);

    let fresh = db
        .get_pending_approval(&fresh_id)
        .await
        .unwrap()
        .expect("fresh row");
    assert_eq!(fresh.status, ApprovalStatus::Pending);

    // And confirm the spawn_timeout_sweeper task itself is wired up: it should
    // exit cleanly when its cancellation token fires.
    let cancel = CancellationToken::new();
    let handle = spawn_timeout_sweeper(
        Db::spawn_in_memory().unwrap(),
        Bus::new(),
        Duration::from_secs(60),
        cancel.clone(),
    );
    cancel.cancel();
    wait_for(Duration::from_secs(2), handle)
        .await
        .expect("sweeper exited")
        .expect("sweeper task ok");
}

#[tokio::test]
async fn sweep_publishes_one_resolved_event_per_swept_id() {
    let db = Db::spawn_in_memory().unwrap();
    let bus = Bus::new();
    let mut rx = bus.subscribe();

    for id in ["a", "b"] {
        db.create_pending_approval(PendingApproval {
            id: id.to_string(),
            mcp_server: "s".into(),
            tool: "t".into(),
            args_json: "{}".into(),
            client_key: "k1234567".into(),
            created_at: 50,
            status: ApprovalStatus::Pending,
        })
        .await
        .unwrap();
    }

    let n = sweep_and_publish_timeouts(&db, &bus, 100).await.unwrap();
    assert_eq!(n, 2);

    let mut got = std::collections::HashSet::new();
    for _ in 0..2 {
        match wait_for(Duration::from_secs(1), rx.recv()).await.unwrap() {
            Ok(Event::ApprovalResolved { id, status }) => {
                assert_eq!(status, ApprovalStatus::Timeout);
                got.insert(id);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }
    assert_eq!(got.len(), 2, "expected one event per id");
    assert!(got.contains("a"));
    assert!(got.contains("b"));
}
