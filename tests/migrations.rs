#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test ergonomics"
)]

use conduit::daemon::db::{
    types::{Decision, DecisionSource, Server},
    Db,
};

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
    }
}

#[tokio::test]
async fn v01_creates_server_tools_table() {
    let db = Db::spawn_in_memory().expect("spawn db actor");
    let tables = db.list_tables().await.expect("list tables");
    assert!(
        tables.iter().any(|t| t == "server_tools"),
        "expected server_tools table after v0.1 migration, got {tables:?}"
    );
}

#[tokio::test]
async fn v01_servers_table_has_new_columns() {
    let db = Db::spawn_in_memory().expect("spawn db actor");
    let cols = db.table_columns("servers").await.expect("table_columns");
    for expected in [
        "server_name",
        "server_version",
        "last_probed_at",
        "last_probe_error",
        "oauth_authority",
        "oauth_client_id",
        "oauth_client_secret_ref",
        "oauth_scopes",
    ] {
        assert!(
            cols.iter().any(|c| c == expected),
            "missing column {expected} on servers, got {cols:?}"
        );
    }
}

#[tokio::test]
async fn set_settings_kv_updates_value() {
    let db = Db::spawn_in_memory().expect("spawn");
    db.set_settings_kv("routing_paused", "true").await.unwrap();
    let v = db.get_settings_kv("routing_paused").await.unwrap();
    assert_eq!(v.as_deref(), Some("true"));

    db.set_settings_kv("routing_paused", "false").await.unwrap();
    let v = db.get_settings_kv("routing_paused").await.unwrap();
    assert_eq!(v.as_deref(), Some("false"));
}

#[tokio::test]
async fn set_settings_kv_inserts_unknown_key() {
    let db = Db::spawn_in_memory().expect("spawn");
    db.set_settings_kv("new_key", "yes").await.unwrap();
    let v = db.get_settings_kv("new_key").await.unwrap();
    assert_eq!(v.as_deref(), Some("yes"));
}

#[tokio::test]
async fn migrations_idempotent_on_re_open() {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let mut path = std::env::temp_dir();
    path.push(format!(
        "conduit-mig-test-{}-{n}.db",
        std::process::id()
    ));
    let _ignored = std::fs::remove_file(&path);

    {
        let db = Db::spawn_at(&path).expect("first spawn");
        db.ping().await.expect("first ping");
        assert_eq!(db.schema_version().await.unwrap(), 3);
    }
    {
        let db = Db::spawn_at(&path).expect("second spawn re-runs migrations");
        db.ping().await.expect("second ping");
        assert_eq!(db.schema_version().await.unwrap(), 3);
        let cols = db.table_columns("servers").await.unwrap();
        assert!(cols.iter().any(|c| c == "oauth_authority"));
    }

    let _ignored = std::fs::remove_file(&path);
    let _ignored = std::fs::remove_file(path.with_extension("db-wal"));
    let _ignored = std::fs::remove_file(path.with_extension("db-shm"));
}

#[tokio::test]
async fn delete_server_cascades_to_policies() {
    let db = Db::spawn_in_memory().expect("spawn db actor");
    db.upsert_server(server_row("gmail")).await.unwrap();
    db.set_policy("gmail", "send", Decision::Allow, DecisionSource::User)
        .await
        .unwrap();
    assert!(db.get_policy("gmail", "send").await.unwrap().is_some());

    db.delete_server("gmail").await.unwrap();

    assert!(
        db.get_policy("gmail", "send").await.unwrap().is_none(),
        "expected policy to cascade-delete with its server"
    );
}

#[tokio::test]
async fn v01_policies_table_has_scope_column() {
    let db = Db::spawn_in_memory().expect("spawn db actor");
    let cols = db.table_columns("policies").await.expect("table_columns");
    assert!(
        cols.iter().any(|c| c == "scope"),
        "expected policies.scope column after v0.1 migration, got {cols:?}"
    );
}

#[tokio::test]
async fn v01_seeds_settings_kv_with_defaults() {
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
