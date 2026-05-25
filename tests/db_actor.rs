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
