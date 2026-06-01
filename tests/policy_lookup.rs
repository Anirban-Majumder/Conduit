//! D2 gate: the two-tier policy resolver (§2).
//!
//! Resolution is a single path: per-tool `mcp_tool.decision` (source `User`),
//! else the global `settings_kv.default_unknown_tool_policy` (source `Default`,
//! falling back to `Ask`). There is NO per-server tier and no double round-trip.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test ergonomics")]

use conduit::daemon::db::types::{Decision, DecisionSource, McpServer, McpTool};
use conduit::daemon::db::Db;

fn server(name: &str) -> McpServer {
    McpServer {
        name: name.into(),
        transport: "stdio".into(),
        command: Some("echo".into()),
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

fn tool(server: &str, name: &str) -> McpTool {
    McpTool {
        server: server.into(),
        name: name.into(),
        description: None,
        input_schema_json: "{}".into(),
        discovered_at: 0,
        // Fresh probe seeds NULL decision (inherit) — never 'ask'/'probe'.
        decision: None,
    }
}

async fn seed_server(db: &Db, name: &str, tools: &[&str]) {
    let tools = tools.iter().map(|t| tool(name, t)).collect();
    db.upsert_server_with_tools(server(name), tools)
        .await
        .unwrap();
}

#[tokio::test]
async fn tier1_per_tool_decision_wins_with_user_source() {
    let db = Db::spawn_in_memory().unwrap();
    seed_server(&db, "gmail", &["send"]).await;
    db.set_tool_decision("gmail", "send", Decision::Allow, DecisionSource::User)
        .await
        .unwrap();

    let (decision, source) = db.resolve_policy("gmail", "send").await.unwrap();
    assert_eq!(decision, Decision::Allow);
    assert_eq!(source, DecisionSource::User);
}

#[tokio::test]
async fn freshly_probed_tool_inherits_global_default_not_ask_seed() {
    let db = Db::spawn_in_memory().unwrap();
    db.set_settings_kv("default_unknown_tool_policy", "deny")
        .await
        .unwrap();
    // A just-probed tool has decision = NULL; it must fall through to the
    // global default rather than a per-tool 'ask' seed.
    seed_server(&db, "gmail", &["send"]).await;

    let (decision, source) = db.resolve_policy("gmail", "send").await.unwrap();
    assert_eq!(decision, Decision::Deny);
    assert_eq!(source, DecisionSource::Default);
}

#[tokio::test]
async fn tier2_falls_through_to_settings_kv_default() {
    let db = Db::spawn_in_memory().unwrap();
    db.set_settings_kv("default_unknown_tool_policy", "deny")
        .await
        .unwrap();

    // No tool row at all → straight to the global default.
    let (decision, source) = db.resolve_policy("none", "none").await.unwrap();
    assert_eq!(decision, Decision::Deny);
    assert_eq!(source, DecisionSource::Default);
}

#[tokio::test]
async fn default_ask_when_settings_kv_garbage() {
    let db = Db::spawn_in_memory().unwrap();
    db.set_settings_kv("default_unknown_tool_policy", "garbage")
        .await
        .unwrap();

    let (decision, source) = db.resolve_policy("none", "none").await.unwrap();
    assert_eq!(decision, Decision::Ask);
    assert_eq!(source, DecisionSource::Default);
}

#[tokio::test]
async fn reprobe_drops_decision_for_vanished_tool() {
    let db = Db::spawn_in_memory().unwrap();
    seed_server(&db, "gmail", &["send", "read"]).await;
    db.set_tool_decision("gmail", "send", Decision::Allow, DecisionSource::User)
        .await
        .unwrap();

    // Re-probe without "send": its row (and its decision) is deleted.
    seed_server(&db, "gmail", &["read"]).await;
    db.set_settings_kv("default_unknown_tool_policy", "ask")
        .await
        .unwrap();

    let (decision, source) = db.resolve_policy("gmail", "send").await.unwrap();
    assert_eq!(
        decision,
        Decision::Ask,
        "vanished tool re-resolves to default"
    );
    assert_eq!(source, DecisionSource::Default);
}

#[tokio::test]
async fn reprobe_preserves_decision_for_surviving_tool() {
    // The re-probe upsert must NOT overwrite a user's decision on a tool that is
    // still advertised. (Guards the `ON CONFLICT DO UPDATE` set-list in
    // query_upsert_server_with_tools from regressing to `decision = excluded.decision`.)
    let db = Db::spawn_in_memory().unwrap();
    db.set_settings_kv("default_unknown_tool_policy", "ask")
        .await
        .unwrap();
    seed_server(&db, "gmail", &["send", "read"]).await;
    db.set_tool_decision("gmail", "send", Decision::Allow, DecisionSource::User)
        .await
        .unwrap();

    // Re-probe with "send" STILL present (metadata refresh / discovered_at bump).
    seed_server(&db, "gmail", &["send", "read"]).await;

    let (decision, source) = db.resolve_policy("gmail", "send").await.unwrap();
    assert_eq!(
        decision,
        Decision::Allow,
        "surviving tool keeps its user decision"
    );
    assert_eq!(source, DecisionSource::User);
}
