#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test ergonomics")]

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
