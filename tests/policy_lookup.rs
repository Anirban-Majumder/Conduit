#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test ergonomics")]

use conduit::daemon::db::types::{Decision, DecisionSource, Server, ToolMeta};
use conduit::daemon::db::Db;

fn server(name: &str) -> Server {
    Server {
        name: name.into(),
        transport: "stdio".into(),
        command: Some("echo".into()),
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

async fn with_seeded_server(db: &Db, name: &str) {
    db.upsert_server_with_tools(
        server(name),
        vec![ToolMeta {
            name: "send".into(),
            description: None,
            input_schema_json: "{}".into(),
            discovered_at: 0,
        }],
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn resolve_returns_tool_scope_when_present() {
    let db = Db::spawn_in_memory().unwrap();
    with_seeded_server(&db, "gmail").await;
    db.set_policy("gmail", "send", Decision::Allow, DecisionSource::User)
        .await
        .unwrap();
    let d = db.resolve_policy("gmail", "send").await.unwrap();
    assert_eq!(d, Decision::Allow);
}

#[tokio::test]
async fn resolve_falls_back_to_server_scope() {
    let db = Db::spawn_in_memory().unwrap();
    with_seeded_server(&db, "gmail").await;
    // upsert_server_with_tools seeds a per-tool 'ask' row by default; remove it
    // to force fallback.
    db.delete_policies_for_server("gmail").await.unwrap();
    // Recreate just the server-scope fallback.
    db.upsert_server_with_tools(server("gmail"), vec![])
        .await
        .unwrap();
    let d = db.resolve_policy("gmail", "unknown_tool").await.unwrap();
    assert_eq!(d, Decision::Ask);
}

#[tokio::test]
async fn resolve_falls_through_to_settings_kv_default() {
    let db = Db::spawn_in_memory().unwrap();
    db.set_settings_kv("default_unknown_tool_policy", "deny")
        .await
        .unwrap();
    let d = db.resolve_policy("none", "none").await.unwrap();
    assert_eq!(d, Decision::Deny);
}

#[tokio::test]
async fn resolve_default_ask_when_settings_kv_missing_or_garbage() {
    let db = Db::spawn_in_memory().unwrap();
    db.set_settings_kv("default_unknown_tool_policy", "garbage")
        .await
        .unwrap();
    let d = db.resolve_policy("none", "none").await.unwrap();
    assert_eq!(d, Decision::Ask);
}
