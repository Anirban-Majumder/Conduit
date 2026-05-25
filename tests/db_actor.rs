#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test ergonomics"
)]

use mcp_router::daemon::db::Db;

#[tokio::test]
async fn ping_round_trips_through_db_actor() {
    let db = Db::spawn_in_memory().expect("spawn db actor");
    db.ping().await.expect("ping reply");
}

#[tokio::test]
async fn schema_version_one_present_after_startup() {
    let db = Db::spawn_in_memory().expect("spawn db actor");
    let version = db.schema_version().await.expect("schema version query");
    assert_eq!(version, 1);
}

#[tokio::test]
async fn all_v0_tables_created_by_migration() {
    let db = Db::spawn_in_memory().expect("spawn db actor");
    let tables = db.list_tables().await.expect("list tables");

    for expected in [
        "audit_log",
        "ide_keys",
        "managed_ide_configs",
        "pending_approvals",
        "policies",
        "schema_version",
        "servers",
    ] {
        assert!(
            tables.iter().any(|t| t == expected),
            "missing table {expected}, got {tables:?}"
        );
    }
}

#[tokio::test]
async fn ide_key_create_then_lookup_round_trip() {
    let db = Db::spawn_in_memory().expect("spawn");
    let created = db.create_ide_key("Cursor").await.expect("create");
    assert_eq!(created.key.len(), 8);
    assert_eq!(created.ide_name, "Cursor");

    let looked_up = db.lookup_ide_key(&created.key).await.expect("lookup");
    assert_eq!(looked_up, Some(created));
}

#[tokio::test]
async fn ide_key_lookup_unknown_returns_none() {
    let db = Db::spawn_in_memory().expect("spawn");
    let result = db.lookup_ide_key("notarealk").await.expect("lookup");
    assert_eq!(result, None);
}

#[tokio::test]
async fn ide_keys_list_filters_by_name() {
    let db = Db::spawn_in_memory().expect("spawn");
    db.create_ide_key("Cursor").await.unwrap();
    db.create_ide_key("Cursor").await.unwrap();
    db.create_ide_key("Claude Code").await.unwrap();

    let cursor_keys = db.list_ide_keys_for_name("Cursor").await.unwrap();
    let claude_keys = db.list_ide_keys_for_name("Claude Code").await.unwrap();

    assert_eq!(cursor_keys.len(), 2);
    assert_eq!(claude_keys.len(), 1);
    assert!(cursor_keys.iter().all(|k| k.ide_name == "Cursor"));
}

#[tokio::test]
async fn ide_key_delete_removes_row() {
    let db = Db::spawn_in_memory().expect("spawn");
    let k = db.create_ide_key("Cursor").await.unwrap();
    db.delete_ide_key(&k.key).await.unwrap();
    assert_eq!(db.lookup_ide_key(&k.key).await.unwrap(), None);
}

#[tokio::test]
async fn ide_config_write_then_list_round_trip() {
    use mcp_router::daemon::db::types::ManagedIdeConfig;
    use std::path::PathBuf;

    let db = Db::spawn_in_memory().expect("spawn");
    let cfg = ManagedIdeConfig {
        ide_name: "Cursor".to_string(),
        ide_key: "abcd1234".to_string(),
        config_path: PathBuf::from("/home/u/.cursor/mcp.json"),
        managed_entries: vec!["gmail".to_string(), "github".to_string()],
        written_at: 1_700_000_000,
    };
    db.record_ide_config_write(cfg.clone()).await.unwrap();

    let rows = db.list_ide_configs_for_ide("Cursor").await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0], cfg);
}

#[tokio::test]
async fn ide_config_write_upserts_on_same_key() {
    use mcp_router::daemon::db::types::ManagedIdeConfig;
    use std::path::PathBuf;

    let db = Db::spawn_in_memory().expect("spawn");
    let mut cfg = ManagedIdeConfig {
        ide_name: "Cursor".to_string(),
        ide_key: "abcd1234".to_string(),
        config_path: PathBuf::from("/home/u/.cursor/mcp.json"),
        managed_entries: vec!["gmail".to_string()],
        written_at: 1_700_000_000,
    };
    db.record_ide_config_write(cfg.clone()).await.unwrap();

    cfg.managed_entries = vec!["gmail".to_string(), "github".to_string()];
    cfg.written_at = 1_700_000_999;
    db.record_ide_config_write(cfg.clone()).await.unwrap();

    let rows = db.list_ide_configs_for_ide("Cursor").await.unwrap();
    assert_eq!(rows.len(), 1, "should upsert, not duplicate");
    assert_eq!(rows[0].managed_entries.len(), 2);
    assert_eq!(rows[0].written_at, 1_700_000_999);
}

#[tokio::test]
async fn ide_config_delete_for_ide_clears_rows() {
    use mcp_router::daemon::db::types::ManagedIdeConfig;
    use std::path::PathBuf;

    let db = Db::spawn_in_memory().expect("spawn");
    for key in ["a", "b"] {
        db.record_ide_config_write(ManagedIdeConfig {
            ide_name: "Cursor".to_string(),
            ide_key: key.to_string(),
            config_path: PathBuf::from("/x"),
            managed_entries: vec![],
            written_at: 0,
        })
        .await
        .unwrap();
    }
    db.delete_ide_configs_for_ide("Cursor").await.unwrap();
    assert!(db
        .list_ide_configs_for_ide("Cursor")
        .await
        .unwrap()
        .is_empty());
}
