//! Schema baseline + forward-seam tests against the single `0001_init.sql`.
//!
//! There is one compiled baseline (`schema_version` = 1); the legacy `servers` /
//! `server_tools` / `policies` / `ides` tables are gone, replaced by
//! `mcp_server` / `mcp_tool` / `client` / `managed_mcp` / `managed_skill`. The
//! migration runner is idempotent on re-open.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test ergonomics"
)]

use conduit::daemon::db::Db;

#[tokio::test]
async fn baseline_creates_new_table_set() {
    let db = Db::spawn_in_memory().expect("spawn db actor");
    let tables = db.list_tables().await.expect("list tables");
    for expected in [
        "mcp_server",
        "mcp_tool",
        "client",
        "managed_mcp",
        "managed_skill",
        "skills",
        "skill_repos",
        "pending_approvals",
        "audit_log",
        "settings_kv",
        "schema_version",
    ] {
        assert!(
            tables.iter().any(|t| t == expected),
            "missing table {expected}, got {tables:?}"
        );
    }
}

#[tokio::test]
async fn legacy_tables_are_gone() {
    let db = Db::spawn_in_memory().expect("spawn db actor");
    let tables = db.list_tables().await.expect("list tables");
    for dead in [
        "servers",
        "server_tools",
        "policies",
        "ides",
        "skill_targets",
    ] {
        assert!(
            !tables.iter().any(|t| t == dead),
            "legacy table {dead} should not exist, got {tables:?}"
        );
    }
}

#[tokio::test]
async fn mcp_server_has_trimmed_columns() {
    let db = Db::spawn_in_memory().expect("spawn db actor");
    let cols = db.table_columns("mcp_server").await.expect("columns");
    for expected in ["reported_name", "reported_version", "last_auth_error_msg"] {
        assert!(
            cols.iter().any(|c| c == expected),
            "missing {expected}: {cols:?}"
        );
    }
    for dead in [
        "last_init_duration_ms",
        "package_version",
        "mcp_self_version",
        "created_at",
        "last_auth_error_at",
        "default_tool_policy",
    ] {
        assert!(
            !cols.iter().any(|c| c == dead),
            "{dead} should be dropped: {cols:?}"
        );
    }
}

#[tokio::test]
async fn mcp_tool_merges_metadata_and_decision() {
    let db = Db::spawn_in_memory().expect("spawn db actor");
    let cols = db.table_columns("mcp_tool").await.expect("columns");
    for expected in [
        "mcp_server",
        "name",
        "description",
        "discovered_at",
        "decision",
    ] {
        assert!(
            cols.iter().any(|c| c == expected),
            "missing {expected}: {cols:?}"
        );
    }
}

#[tokio::test]
async fn schema_version_is_one() {
    let db = Db::spawn_in_memory().expect("spawn db actor");
    assert_eq!(db.schema_version().await.unwrap(), 1);
}

#[tokio::test]
async fn client_has_project_scope_columns() {
    // The baseline carries the two repo-relative path columns used by directory-scope.
    let db = Db::spawn_in_memory().expect("spawn db actor");
    let cols = db.table_columns("client").await.expect("columns");
    for expected in ["project_config_rel", "project_skills_rel"] {
        assert!(
            cols.iter().any(|c| c == expected),
            "missing {expected}: {cols:?}"
        );
    }
}

#[tokio::test]
async fn set_settings_kv_updates_value() {
    let db = Db::spawn_in_memory().expect("spawn");
    db.set_settings_kv("routing_paused", "true").await.unwrap();
    assert_eq!(
        db.get_settings_kv("routing_paused")
            .await
            .unwrap()
            .as_deref(),
        Some("true")
    );
    db.set_settings_kv("routing_paused", "false").await.unwrap();
    assert_eq!(
        db.get_settings_kv("routing_paused")
            .await
            .unwrap()
            .as_deref(),
        Some("false")
    );
}

#[tokio::test]
async fn set_settings_kv_inserts_unknown_key() {
    let db = Db::spawn_in_memory().expect("spawn");
    db.set_settings_kv("new_key", "yes").await.unwrap();
    assert_eq!(
        db.get_settings_kv("new_key").await.unwrap().as_deref(),
        Some("yes")
    );
}

#[tokio::test]
async fn migrations_idempotent_on_re_open() {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let mut path = std::env::temp_dir();
    path.push(format!("conduit-mig-test-{}-{n}.db", std::process::id()));
    let _ignored = std::fs::remove_file(&path);

    {
        let db = Db::spawn_at(&path).expect("first spawn");
        db.ping().await.expect("first ping");
        assert_eq!(db.schema_version().await.unwrap(), 1);
    }
    {
        let db = Db::spawn_at(&path).expect("second spawn re-runs migrations");
        db.ping().await.expect("second ping");
        // Re-running migrations is a no-op (already at the recorded version).
        assert_eq!(db.schema_version().await.unwrap(), 1);
        let cols = db.table_columns("mcp_server").await.unwrap();
        assert!(cols.iter().any(|c| c == "last_auth_error_msg"));
    }

    let _ignored = std::fs::remove_file(&path);
    let _ignored = std::fs::remove_file(path.with_extension("db-wal"));
    let _ignored = std::fs::remove_file(path.with_extension("db-shm"));
}

#[tokio::test]
async fn delete_server_cascades_to_tool_decisions() {
    use conduit::daemon::db::types::{Decision, DecisionSource, McpServer, McpTool};

    let db = Db::spawn_in_memory().expect("spawn db actor");
    let server = McpServer {
        name: "gmail".to_string(),
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
    };
    db.upsert_server_with_tools(
        server,
        vec![McpTool {
            server: "gmail".into(),
            name: "send".into(),
            description: None,
            input_schema_json: "{}".into(),
            discovered_at: 0,
            decision: None,
        }],
    )
    .await
    .unwrap();
    db.set_tool_decision("gmail", "send", Decision::Allow, DecisionSource::User)
        .await
        .unwrap();
    assert!(db
        .get_tool_decision("gmail", "send")
        .await
        .unwrap()
        .is_some());

    db.delete_server("gmail").await.unwrap();

    // mcp_tool rows cascade-delete with the server (FK ON DELETE CASCADE).
    assert!(db.list_server_tools("gmail").await.unwrap().is_empty());
    assert!(db
        .get_tool_decision("gmail", "send")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn seeds_settings_kv_with_defaults() {
    let db = Db::spawn_in_memory().expect("spawn db actor");
    for (key, expected) in [
        ("default_unknown_tool_policy", "ask"),
        ("open_window_at_startup", "false"),
        ("routing_paused", "false"),
        ("has_completed_first_run", "false"),
        ("shown_background_hint", "false"),
    ] {
        let got = db.get_settings_kv(key).await.expect("settings_kv query");
        assert_eq!(got.as_deref(), Some(expected), "wrong seed for {key}");
    }
}
