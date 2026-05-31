#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test ergonomics"
)]

use conduit::daemon::db::{types::Server, Db};

fn server_row(name: &str) -> Server {
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
        last_auth_error_msg: None,
        last_auth_error_at: None,
    }
}

#[tokio::test]
async fn ping_round_trips_through_db_actor() {
    let db = Db::spawn_in_memory().expect("spawn db actor");
    db.ping().await.expect("ping reply");
}

#[tokio::test]
async fn schema_version_four_after_ides_migration() {
    let db = Db::spawn_in_memory().expect("spawn db actor");
    let version = db.schema_version().await.expect("schema version query");
    assert_eq!(version, 4);
}

#[tokio::test]
async fn all_v0_tables_created_by_migration() {
    let db = Db::spawn_in_memory().expect("spawn db actor");
    let tables = db.list_tables().await.expect("list tables");

    for expected in [
        "audit_log",
        "ides",
        "pending_approvals",
        "policies",
        "schema_version",
        "servers",
        "skill_repos",
        "skill_targets",
        "skills",
    ] {
        assert!(
            tables.iter().any(|t| t == expected),
            "missing table {expected}, got {tables:?}"
        );
    }

    for legacy in ["ide_keys", "managed_ide_configs"] {
        assert!(
            !tables.iter().any(|t| t == legacy),
            "legacy table {legacy} should be gone, got {tables:?}"
        );
    }
}

#[tokio::test]
async fn upsert_detected_ide_inserts_then_resurrects_stale() {
    use conduit::daemon::db::types::IdeStatus;

    let db = Db::spawn_in_memory().expect("spawn");
    let row = db
        .upsert_detected_ide("Cursor", "Cursor", Some("path:/usr/bin/cursor".to_string()))
        .await
        .unwrap();
    assert_eq!(row.ide_kind, "Cursor");
    assert_eq!(row.display_name, "Cursor");
    assert_eq!(row.ide_key.len(), 8);
    assert!(matches!(row.status, IdeStatus::Active));

    db.mark_ides_stale(vec![]).await.unwrap();
    let stale = db.get_ide_by_kind("Cursor").await.unwrap().unwrap();
    assert!(matches!(stale.status, IdeStatus::Stale));

    let resurrected = db
        .upsert_detected_ide("Cursor", "Cursor", Some("path:/usr/bin/cursor".to_string()))
        .await
        .unwrap();
    assert_eq!(resurrected.ide_key, row.ide_key);
    assert!(matches!(resurrected.status, IdeStatus::Active));
}

#[tokio::test]
async fn lookup_ide_key_skips_not_tracked() {
    let db = Db::spawn_in_memory().expect("spawn");
    let row = db
        .upsert_detected_ide("Cursor", "Cursor", None)
        .await
        .unwrap();
    assert!(db.lookup_ide_key(&row.ide_key).await.unwrap().is_some());

    db.mark_ide_not_tracked("Cursor").await.unwrap();
    assert!(db.lookup_ide_key(&row.ide_key).await.unwrap().is_none());

    db.retrack_ide("Cursor").await.unwrap();
    assert!(db.lookup_ide_key(&row.ide_key).await.unwrap().is_some());
}

#[tokio::test]
async fn mark_ides_stale_preserves_not_tracked() {
    use conduit::daemon::db::types::IdeStatus;

    let db = Db::spawn_in_memory().expect("spawn");
    db.upsert_detected_ide("Cursor", "Cursor", None)
        .await
        .unwrap();
    db.upsert_detected_ide("ClaudeCode", "Claude Code", None)
        .await
        .unwrap();
    db.mark_ide_not_tracked("Cursor").await.unwrap();

    db.mark_ides_stale(vec![]).await.unwrap();

    let cursor = db.get_ide_by_kind("Cursor").await.unwrap().unwrap();
    let claude = db.get_ide_by_kind("ClaudeCode").await.unwrap().unwrap();
    assert!(matches!(cursor.status, IdeStatus::NotTracked));
    assert!(matches!(claude.status, IdeStatus::Stale));
}

#[tokio::test]
async fn update_ide_managed_entries_round_trip() {
    use std::path::PathBuf;

    let db = Db::spawn_in_memory().expect("spawn");
    db.upsert_detected_ide("Cursor", "Cursor", None)
        .await
        .unwrap();

    let path = PathBuf::from("/home/u/.cursor/mcp.json");
    db.update_ide_managed_entries(
        "Cursor",
        path.clone(),
        vec!["gmail".to_string(), "github".to_string()],
    )
    .await
    .unwrap();

    let row = db.get_ide_by_kind("Cursor").await.unwrap().unwrap();
    assert_eq!(row.config_path.as_deref(), Some(path.as_path()));
    assert_eq!(
        row.managed_entries,
        vec!["gmail".to_string(), "github".to_string()]
    );
}

#[tokio::test]
async fn mark_not_tracked_clears_bindings() {
    let db = Db::spawn_in_memory().expect("spawn");
    db.upsert_detected_ide("Cursor", "Cursor", None)
        .await
        .unwrap();
    db.update_ide_managed_entries(
        "Cursor",
        std::path::PathBuf::from("/tmp/mcp.json"),
        vec!["gmail".to_string()],
    )
    .await
    .unwrap();
    db.mark_ide_not_tracked("Cursor").await.unwrap();

    let row = db.get_ide_by_kind("Cursor").await.unwrap().unwrap();
    assert!(row.config_path.is_none());
    assert!(row.managed_entries.is_empty());
    assert!(row.not_tracked_at.is_some());
}

#[tokio::test]
async fn policy_set_then_get_round_trip() {
    use conduit::daemon::db::types::{Decision, DecisionSource};

    let db = Db::spawn_in_memory().expect("spawn");
    db.upsert_server(server_row("gmail")).await.unwrap();
    let id = db
        .set_policy(
            "gmail",
            "send_email",
            Decision::Ask,
            DecisionSource::Default,
        )
        .await
        .unwrap();
    assert!(id > 0);

    let p = db.get_policy("gmail", "send_email").await.unwrap().unwrap();
    assert_eq!(p.server, "gmail");
    assert_eq!(p.tool, "send_email");
    assert_eq!(p.decision, Decision::Ask);
    assert_eq!(p.decision_source, DecisionSource::Default);
}

#[tokio::test]
async fn policy_set_twice_upserts() {
    use conduit::daemon::db::types::{Decision, DecisionSource};

    let db = Db::spawn_in_memory().expect("spawn");
    db.upsert_server(server_row("gmail")).await.unwrap();
    let id1 = db
        .set_policy(
            "gmail",
            "send_email",
            Decision::Ask,
            DecisionSource::Default,
        )
        .await
        .unwrap();
    let id2 = db
        .set_policy("gmail", "send_email", Decision::Allow, DecisionSource::User)
        .await
        .unwrap();
    assert_eq!(id1, id2, "should update same row, not insert");

    let p = db.get_policy("gmail", "send_email").await.unwrap().unwrap();
    assert_eq!(p.decision, Decision::Allow);
    assert_eq!(p.decision_source, DecisionSource::User);
}

#[tokio::test]
async fn policy_get_unknown_returns_none() {
    let db = Db::spawn_in_memory().expect("spawn");
    assert!(db.get_policy("nope", "nope").await.unwrap().is_none());
}

#[tokio::test]
async fn policy_list_for_server_filters() {
    use conduit::daemon::db::types::{Decision, DecisionSource};

    let db = Db::spawn_in_memory().expect("spawn");
    db.upsert_server(server_row("gmail")).await.unwrap();
    db.upsert_server(server_row("github")).await.unwrap();
    db.set_policy("gmail", "send", Decision::Allow, DecisionSource::User)
        .await
        .unwrap();
    db.set_policy("gmail", "read", Decision::Ask, DecisionSource::Default)
        .await
        .unwrap();
    db.set_policy(
        "github",
        "create_issue",
        Decision::Deny,
        DecisionSource::User,
    )
    .await
    .unwrap();

    let gmail = db.list_policies_for_server("gmail").await.unwrap();
    let all = db.list_all_policies().await.unwrap();
    assert_eq!(gmail.len(), 2);
    assert_eq!(all.len(), 3);
}

#[tokio::test]
async fn policy_delete_for_server_clears() {
    use conduit::daemon::db::types::{Decision, DecisionSource};

    let db = Db::spawn_in_memory().expect("spawn");
    db.upsert_server(server_row("gmail")).await.unwrap();
    db.set_policy("gmail", "send", Decision::Allow, DecisionSource::User)
        .await
        .unwrap();
    db.delete_policies_for_server("gmail").await.unwrap();
    assert!(db
        .list_policies_for_server("gmail")
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn pending_approval_create_get_resolve() {
    use conduit::daemon::db::types::{ApprovalStatus, PendingApproval};

    let db = Db::spawn_in_memory().expect("spawn");
    let approval = PendingApproval {
        id: "uuid-1".to_string(),
        server: "gmail".to_string(),
        tool: "send".to_string(),
        args_json: "{}".to_string(),
        ide_key: "abcd1234".to_string(),
        created_at: 100,
        status: ApprovalStatus::Pending,
    };
    db.create_pending_approval(approval.clone()).await.unwrap();

    let got = db.get_pending_approval("uuid-1").await.unwrap().unwrap();
    assert_eq!(got.status, ApprovalStatus::Pending);

    db.resolve_pending_approval("uuid-1", ApprovalStatus::Approved)
        .await
        .unwrap();
    let got2 = db.get_pending_approval("uuid-1").await.unwrap().unwrap();
    assert_eq!(got2.status, ApprovalStatus::Approved);
}

#[tokio::test]
async fn pending_approval_list_only_returns_pending() {
    use conduit::daemon::db::types::{ApprovalStatus, PendingApproval};

    let db = Db::spawn_in_memory().expect("spawn");
    for (id, status) in [
        ("a", ApprovalStatus::Pending),
        ("b", ApprovalStatus::Approved),
        ("c", ApprovalStatus::Pending),
    ] {
        db.create_pending_approval(PendingApproval {
            id: id.to_string(),
            server: "x".to_string(),
            tool: "y".to_string(),
            args_json: "{}".to_string(),
            ide_key: "k".to_string(),
            created_at: 1,
            status: ApprovalStatus::Pending,
        })
        .await
        .unwrap();
        if status != ApprovalStatus::Pending {
            db.resolve_pending_approval(id, status).await.unwrap();
        }
    }

    let pending = db.list_pending_approvals().await.unwrap();
    assert_eq!(pending.len(), 2);
}

#[tokio::test]
async fn pending_approval_sweep_stale_flips_old_to_timeout() {
    use conduit::daemon::db::types::{ApprovalStatus, PendingApproval};

    let db = Db::spawn_in_memory().expect("spawn");
    db.create_pending_approval(PendingApproval {
        id: "old".to_string(),
        server: "x".to_string(),
        tool: "y".to_string(),
        args_json: "{}".to_string(),
        ide_key: "k".to_string(),
        created_at: 50,
        status: ApprovalStatus::Pending,
    })
    .await
    .unwrap();
    db.create_pending_approval(PendingApproval {
        id: "new".to_string(),
        server: "x".to_string(),
        tool: "y".to_string(),
        args_json: "{}".to_string(),
        ide_key: "k".to_string(),
        created_at: 200,
        status: ApprovalStatus::Pending,
    })
    .await
    .unwrap();

    let swept = db.sweep_stale_pending_approvals(100).await.unwrap();
    assert_eq!(swept, vec!["old".to_string()]);

    assert_eq!(
        db.get_pending_approval("old")
            .await
            .unwrap()
            .unwrap()
            .status,
        ApprovalStatus::Timeout
    );
    assert_eq!(
        db.get_pending_approval("new")
            .await
            .unwrap()
            .unwrap()
            .status,
        ApprovalStatus::Pending
    );
}

#[tokio::test]
async fn audit_insert_then_query_round_trip() {
    use conduit::daemon::db::types::{AuditDecision, AuditEntry, AuditFilter};

    let db = Db::spawn_in_memory().expect("spawn");
    let entry = AuditEntry {
        id: None,
        ts: 1000,
        ide_key: Some("k1".to_string()),
        ide_name: Some("Cursor".to_string()),
        server: "gmail".to_string(),
        tool: Some("send".to_string()),
        decision: AuditDecision::Allow,
        decision_source: None,
        latency_ms: Some(42),
        request_args: Some("{}".to_string()),
        response_status: Some("ok".to_string()),
        response_length: Some(123),
        error_code: None,
    };
    let id = db.insert_audit_entry(entry).await.unwrap();
    assert!(id > 0);

    let rows = db.query_audit_log(AuditFilter::default()).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].server, "gmail");
    assert_eq!(rows[0].latency_ms, Some(42));
}

#[tokio::test]
async fn audit_query_filters_by_ide_and_server() {
    use conduit::daemon::db::types::{AuditDecision, AuditEntry, AuditFilter};

    let db = Db::spawn_in_memory().expect("spawn");
    for (ide, server) in [("k1", "gmail"), ("k2", "gmail"), ("k1", "github")] {
        db.insert_audit_entry(AuditEntry {
            id: None,
            ts: 1000,
            ide_key: Some(ide.to_string()),
            ide_name: None,
            server: server.to_string(),
            tool: None,
            decision: AuditDecision::Allow,
            decision_source: None,
            latency_ms: None,
            request_args: None,
            response_status: None,
            response_length: None,
            error_code: None,
        })
        .await
        .unwrap();
    }

    let only_k1 = db
        .query_audit_log(AuditFilter {
            ide_key: Some("k1".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(only_k1.len(), 2);

    let only_gmail = db
        .query_audit_log(AuditFilter {
            server: Some("gmail".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(only_gmail.len(), 2);
}

#[tokio::test]
async fn audit_filter_decision_and_text() {
    use conduit::daemon::db::types::{AuditDecision, AuditEntry, AuditFilter};

    let db = Db::spawn_in_memory().expect("spawn");
    let entries = [
        ("github", "create_issue", AuditDecision::Allow),
        ("github", "delete_repo", AuditDecision::Deny),
        ("slack", "send_message", AuditDecision::Allow),
    ];
    for (server, tool, decision) in entries {
        db.insert_audit_entry(AuditEntry {
            id: None,
            ts: 1_000,
            ide_key: None,
            ide_name: None,
            server: server.to_string(),
            tool: Some(tool.to_string()),
            decision,
            decision_source: None,
            latency_ms: None,
            request_args: None,
            response_status: None,
            response_length: None,
            error_code: None,
        })
        .await
        .unwrap();
    }

    // decision filter
    let denied = db
        .query_audit_log(AuditFilter {
            decision: Some(AuditDecision::Deny),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(denied.len(), 1);
    assert_eq!(denied[0].tool.as_deref(), Some("delete_repo"));

    // text filter (partial server name)
    let slack_rows = db
        .query_audit_log(AuditFilter {
            text: Some("slack".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(slack_rows.len(), 1);
    assert_eq!(slack_rows[0].server, "slack");

    // text filter (partial tool name)
    let issue_rows = db
        .query_audit_log(AuditFilter {
            text: Some("issue".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(issue_rows.len(), 1);
    assert_eq!(issue_rows[0].tool.as_deref(), Some("create_issue"));

    // combined decision + text — no match
    let none_rows = db
        .query_audit_log(AuditFilter {
            decision: Some(AuditDecision::Deny),
            text: Some("slack".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(none_rows.is_empty());

    // LIKE metacharacter escaping: '%' in search term must not wildcard-match everything
    let percent_rows = db
        .query_audit_log(AuditFilter {
            text: Some("%".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        percent_rows.is_empty(),
        "literal '%' should not match any server/tool name"
    );
}

#[tokio::test]
async fn audit_purge_old_removes_pre_cutoff() {
    use conduit::daemon::db::types::{AuditDecision, AuditEntry, AuditFilter};

    let db = Db::spawn_in_memory().expect("spawn");
    for ts in [100, 200, 300] {
        db.insert_audit_entry(AuditEntry {
            id: None,
            ts,
            ide_key: None,
            ide_name: None,
            server: "x".to_string(),
            tool: None,
            decision: AuditDecision::Allow,
            decision_source: None,
            latency_ms: None,
            request_args: None,
            response_status: None,
            response_length: None,
            error_code: None,
        })
        .await
        .unwrap();
    }

    let purged = db.purge_old_audit_entries(250).await.unwrap();
    assert_eq!(purged, 2);
    let remaining = db.query_audit_log(AuditFilter::default()).await.unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].ts, 300);
}

#[tokio::test]
async fn enum_db_str_round_trips() {
    use conduit::daemon::db::types::{ApprovalStatus, AuditDecision, Decision, DecisionSource};

    for d in [Decision::Allow, Decision::Deny, Decision::Ask] {
        assert_eq!(Decision::from_db_str(d.as_db_str()), Some(d));
    }
    assert_eq!(Decision::from_db_str("garbage"), None);

    for s in [
        DecisionSource::Default,
        DecisionSource::User,
        DecisionSource::ToolListChanged,
        DecisionSource::Probe,
        DecisionSource::Notification,
    ] {
        assert_eq!(DecisionSource::from_db_str(s.as_db_str()), Some(s));
    }

    for s in [
        ApprovalStatus::Pending,
        ApprovalStatus::Approved,
        ApprovalStatus::Denied,
        ApprovalStatus::Timeout,
    ] {
        assert_eq!(ApprovalStatus::from_db_str(s.as_db_str()), Some(s));
    }

    for d in [
        AuditDecision::Allow,
        AuditDecision::Deny,
        AuditDecision::AskApproved,
        AuditDecision::AskDenied,
        AuditDecision::AskTimeout,
        AuditDecision::Error,
        AuditDecision::Cancelled,
    ] {
        assert_eq!(AuditDecision::from_db_str(d.as_db_str()), Some(d));
    }
}

#[tokio::test]
async fn list_all_ide_configs_skips_not_tracked() {
    use std::path::PathBuf;

    let db = Db::spawn_in_memory().expect("spawn");
    db.upsert_detected_ide("Cursor", "Cursor", None)
        .await
        .unwrap();
    db.update_ide_managed_entries("Cursor", PathBuf::from("/x"), vec!["gmail".into()])
        .await
        .unwrap();
    assert_eq!(db.list_all_ide_configs().await.unwrap().len(), 1);

    db.mark_ide_not_tracked("Cursor").await.unwrap();
    assert!(db.list_all_ide_configs().await.unwrap().is_empty());
}
