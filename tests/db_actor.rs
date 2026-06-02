#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test ergonomics"
)]

use conduit::daemon::db::{types::McpServer, Db};

fn server_row(name: &str) -> McpServer {
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

#[tokio::test]
async fn ping_round_trips_through_db_actor() {
    let db = Db::spawn_in_memory().expect("spawn db actor");
    db.ping().await.expect("ping reply");
}

#[tokio::test]
async fn schema_version_is_one() {
    let db = Db::spawn_in_memory().expect("spawn db actor");
    assert_eq!(db.schema_version().await.expect("schema version"), 1);
}

#[tokio::test]
async fn all_v0_tables_created_by_migration() {
    let db = Db::spawn_in_memory().expect("spawn db actor");
    let tables = db.list_tables().await.expect("list tables");

    for expected in [
        "audit_log",
        "client",
        "managed_mcp",
        "managed_skill",
        "mcp_server",
        "mcp_tool",
        "pending_approvals",
        "schema_version",
        "skill_repos",
        "skills",
    ] {
        assert!(
            tables.iter().any(|t| t == expected),
            "missing table {expected}, got {tables:?}"
        );
    }
    for legacy in [
        "ides",
        "ide_keys",
        "managed_ide_configs",
        "policies",
        "server_tools",
    ] {
        assert!(
            !tables.iter().any(|t| t == legacy),
            "legacy table {legacy} should be gone, got {tables:?}"
        );
    }
}

// ── Client projection + managed servers ─────────────────────────────────────

#[tokio::test]
async fn project_builtin_inserts_then_preserves_key_on_reproject() {
    let db = Db::spawn_in_memory().expect("spawn");
    let row = db
        .project_builtin(
            "Cursor",
            "Cursor",
            "/home/u/.cursor/mcp.json",
            None,
            "{}",
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(row.id, "Cursor");
    assert_eq!(row.name, "Cursor");
    assert_eq!(row.mcp_key.len(), 8);
    assert!(row.is_builtin);
    assert!(row.tracked);

    // Re-projecting keeps the same mcp_key and tracked flag.
    let again = db
        .project_builtin(
            "Cursor",
            "Cursor Renamed",
            "/new/path.json",
            None,
            "{}",
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        again.mcp_key, row.mcp_key,
        "mcp_key preserved across reproject"
    );
    assert_eq!(again.name, "Cursor Renamed", "identity columns overwritten");
    assert_eq!(again.mcp_config_path, "/new/path.json");
}

#[tokio::test]
async fn lookup_client_key_skips_not_tracked() {
    let db = Db::spawn_in_memory().expect("spawn");
    let row = db
        .project_builtin("Cursor", "Cursor", "/x", None, "{}", None, None)
        .await
        .unwrap();
    assert!(db.lookup_client_key(&row.mcp_key).await.unwrap().is_some());

    db.set_client_tracked("Cursor", false).await.unwrap();
    assert!(db.lookup_client_key(&row.mcp_key).await.unwrap().is_none());

    db.set_client_tracked("Cursor", true).await.unwrap();
    assert!(db.lookup_client_key(&row.mcp_key).await.unwrap().is_some());
}

#[tokio::test]
async fn managed_servers_round_trip_and_reverse_lookup() {
    let db = Db::spawn_in_memory().expect("spawn");
    db.upsert_server(server_row("gmail")).await.unwrap();
    db.upsert_server(server_row("github")).await.unwrap();
    db.project_builtin("Cursor", "Cursor", "/x", None, "{}", None, None)
        .await
        .unwrap();

    db.set_managed_servers("Cursor", "global", vec!["gmail".into(), "github".into()])
        .await
        .unwrap();
    // Returned sorted by server_name.
    assert_eq!(
        db.list_managed_servers("Cursor", "global").await.unwrap(),
        vec!["github".to_string(), "gmail".to_string()]
    );

    // Reverse lookup feeds the cascade.
    let managing = db.list_clients_managing_server("gmail").await.unwrap();
    assert_eq!(managing, vec![("Cursor".to_string(), "global".to_string())]);

    db.set_managed_servers("Cursor", "global", vec!["github".into()])
        .await
        .unwrap();
    assert!(db
        .list_clients_managing_server("gmail")
        .await
        .unwrap()
        .is_empty());
}

// ── Tool decisions (replaces policies) ──────────────────────────────────────

#[tokio::test]
async fn tool_decision_set_then_get_round_trip() {
    use conduit::daemon::db::types::{Decision, DecisionSource, McpTool};

    let db = Db::spawn_in_memory().expect("spawn");
    db.upsert_server_with_tools(
        server_row("gmail"),
        vec![McpTool {
            server: "gmail".into(),
            name: "send_email".into(),
            description: None,
            input_schema_json: "{}".into(),
            discovered_at: 0,
            decision: None,
        }],
    )
    .await
    .unwrap();

    db.set_tool_decision("gmail", "send_email", Decision::Allow, DecisionSource::User)
        .await
        .unwrap();
    assert_eq!(
        db.get_tool_decision("gmail", "send_email").await.unwrap(),
        Some(Decision::Allow)
    );
}

#[tokio::test]
async fn tool_decision_get_unknown_returns_none() {
    let db = Db::spawn_in_memory().expect("spawn");
    assert!(db
        .get_tool_decision("nope", "nope")
        .await
        .unwrap()
        .is_none());
}

// ── Approvals (renamed fields) ──────────────────────────────────────────────

#[tokio::test]
async fn pending_approval_create_get_resolve() {
    use conduit::daemon::db::types::{ApprovalStatus, PendingApproval};

    let db = Db::spawn_in_memory().expect("spawn");
    db.create_pending_approval(PendingApproval {
        id: "uuid-1".to_string(),
        mcp_server: "gmail".to_string(),
        tool: "send".to_string(),
        args_json: "{}".to_string(),
        client_key: "abcd1234".to_string(),
        created_at: 100,
        status: ApprovalStatus::Pending,
    })
    .await
    .unwrap();

    let got = db.get_pending_approval("uuid-1").await.unwrap().unwrap();
    assert_eq!(got.status, ApprovalStatus::Pending);
    assert_eq!(got.mcp_server, "gmail");

    db.resolve_pending_approval("uuid-1", ApprovalStatus::Approved)
        .await
        .unwrap();
    assert_eq!(
        db.get_pending_approval("uuid-1")
            .await
            .unwrap()
            .unwrap()
            .status,
        ApprovalStatus::Approved
    );
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
            mcp_server: "x".to_string(),
            tool: "y".to_string(),
            args_json: "{}".to_string(),
            client_key: "k".to_string(),
            created_at: 1,
            status: ApprovalStatus::Pending,
        })
        .await
        .unwrap();
        if status != ApprovalStatus::Pending {
            db.resolve_pending_approval(id, status).await.unwrap();
        }
    }
    assert_eq!(db.list_pending_approvals().await.unwrap().len(), 2);
}

#[tokio::test]
async fn pending_approval_sweep_stale_flips_old_to_timeout() {
    use conduit::daemon::db::types::{ApprovalStatus, PendingApproval};

    let db = Db::spawn_in_memory().expect("spawn");
    for (id, created_at) in [("old", 50), ("new", 200)] {
        db.create_pending_approval(PendingApproval {
            id: id.to_string(),
            mcp_server: "x".to_string(),
            tool: "y".to_string(),
            args_json: "{}".to_string(),
            client_key: "k".to_string(),
            created_at,
            status: ApprovalStatus::Pending,
        })
        .await
        .unwrap();
    }

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

// ── Audit (renamed fields) ──────────────────────────────────────────────────

#[tokio::test]
async fn audit_insert_then_query_round_trip() {
    use conduit::daemon::db::types::{AuditDecision, AuditEntry, AuditFilter};

    let db = Db::spawn_in_memory().expect("spawn");
    let id = db
        .insert_audit_entry(AuditEntry {
            id: None,
            ts: 1000,
            client_key: Some("k1".to_string()),
            client_name: Some("Cursor".to_string()),
            mcp_server: "gmail".to_string(),
            tool: Some("send".to_string()),
            decision: AuditDecision::Allow,
            decision_source: None,
            latency_ms: Some(42),
            request_args: Some("{}".to_string()),
            response_status: Some("ok".to_string()),
            response_length: Some(123),
            error_code: None,
        })
        .await
        .unwrap();
    assert!(id > 0);

    let rows = db.query_audit_log(AuditFilter::default()).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].mcp_server, "gmail");
    assert_eq!(rows[0].latency_ms, Some(42));
}

#[tokio::test]
async fn audit_query_filters_by_client_and_server() {
    use conduit::daemon::db::types::{AuditDecision, AuditEntry, AuditFilter};

    let db = Db::spawn_in_memory().expect("spawn");
    for (client, server) in [("k1", "gmail"), ("k2", "gmail"), ("k1", "github")] {
        db.insert_audit_entry(AuditEntry {
            id: None,
            ts: 1000,
            client_key: Some(client.to_string()),
            client_name: None,
            mcp_server: server.to_string(),
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
            client_key: Some("k1".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(only_k1.len(), 2);

    let only_gmail = db
        .query_audit_log(AuditFilter {
            mcp_server: Some("gmail".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(only_gmail.len(), 2);
}

#[tokio::test]
async fn audit_purge_old_removes_pre_cutoff() {
    use conduit::daemon::db::types::{AuditDecision, AuditEntry, AuditFilter};

    let db = Db::spawn_in_memory().expect("spawn");
    for ts in [100, 200, 300] {
        db.insert_audit_entry(AuditEntry {
            id: None,
            ts,
            client_key: None,
            client_name: None,
            mcp_server: "x".to_string(),
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

    assert_eq!(db.purge_old_audit_entries(250).await.unwrap(), 2);
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
