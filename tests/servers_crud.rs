#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test ergonomics"
)]

use conduit::daemon::db::types::{EnvEntry, EnvValue, McpServer, TRANSPORT_HTTP, TRANSPORT_STDIO};
use conduit::daemon::db::Db;

fn sample_server(name: &str) -> McpServer {
    McpServer {
        name: name.to_string(),
        transport: TRANSPORT_STDIO.to_string(),
        command: Some("node".to_string()),
        args: vec!["index.js".to_string()],
        env: vec![
            EnvEntry {
                name: "API_KEY".to_string(),
                value: EnvValue::Secret("uuid-1".to_string()),
            },
            EnvEntry {
                name: "DEBUG".to_string(),
                value: EnvValue::Plain("1".to_string()),
            },
        ],
        url: None,
        lifecycle_seconds: 300,
        reported_name: None,
        reported_version: None,
        last_probed_at: None,
        last_probe_error: None,
        last_auth_error_msg: None,
    }
}

/// Register a built-in client row so `managed_mcp` inserts satisfy the FK.
async fn seed_client(db: &Db, id: &str) {
    db.project_builtin(id, id, "/tmp/cfg.json", None, "{}", None, None)
        .await
        .unwrap();
}

#[tokio::test]
async fn upsert_then_get_round_trip() {
    let db = Db::spawn_in_memory().expect("spawn");
    let s = sample_server("gmail");
    db.upsert_server(s.clone()).await.unwrap();
    let got = db.get_server("gmail").await.unwrap().unwrap();
    assert_eq!(got, s);
}

#[tokio::test]
async fn upsert_same_name_updates_in_place() {
    let db = Db::spawn_in_memory().expect("spawn");
    let mut s = sample_server("gmail");
    db.upsert_server(s.clone()).await.unwrap();

    s.lifecycle_seconds = 600;
    s.args.push("--debug".to_string());
    db.upsert_server(s.clone()).await.unwrap();

    let got = db.get_server("gmail").await.unwrap().unwrap();
    assert_eq!(got.lifecycle_seconds, 600);
    assert_eq!(
        got.args,
        vec!["index.js".to_string(), "--debug".to_string()]
    );
    assert_eq!(db.list_servers().await.unwrap().len(), 1);
}

#[tokio::test]
async fn get_unknown_returns_none() {
    let db = Db::spawn_in_memory().expect("spawn");
    assert!(db.get_server("nope").await.unwrap().is_none());
}

#[tokio::test]
async fn list_servers_sorted_by_name() {
    let db = Db::spawn_in_memory().expect("spawn");
    db.upsert_server(sample_server("zeta")).await.unwrap();
    db.upsert_server(sample_server("alpha")).await.unwrap();
    db.upsert_server(sample_server("mid")).await.unwrap();
    let names: Vec<String> = db
        .list_servers()
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, vec!["alpha", "mid", "zeta"]);
}

#[tokio::test]
async fn delete_removes_row() {
    let db = Db::spawn_in_memory().expect("spawn");
    db.upsert_server(sample_server("gmail")).await.unwrap();
    db.delete_server("gmail").await.unwrap();
    assert!(db.get_server("gmail").await.unwrap().is_none());
}

#[tokio::test]
async fn http_transport_preserves_url() {
    let db = Db::spawn_in_memory().expect("spawn");
    let s = McpServer {
        name: "linear".to_string(),
        transport: TRANSPORT_HTTP.to_string(),
        command: None,
        args: vec![],
        env: vec![],
        url: Some("https://mcp.linear.app/sse".to_string()),
        lifecycle_seconds: 300,
        reported_name: None,
        reported_version: None,
        last_probed_at: None,
        last_probe_error: None,
        last_auth_error_msg: None,
    };
    db.upsert_server(s.clone()).await.unwrap();
    assert_eq!(db.get_server("linear").await.unwrap().unwrap(), s);
}

#[test]
fn invalid_server_names_rejected() {
    use conduit::daemon::wizard_input::is_valid_server_name;

    for bad in [
        "",
        "linear:access",
        "../foo",
        "Foo", // uppercase
        "name with space",
        "naïve", // non-ascii
        "x\0null",
        &"a".repeat(65),
    ] {
        assert!(!is_valid_server_name(bad), "should reject {bad:?}");
    }

    for good in [
        "a",
        "github",
        "linear-mcp",
        "server_1",
        "x".repeat(64).as_str(),
    ] {
        assert!(is_valid_server_name(good), "should accept {good:?}");
    }
}

#[test]
fn lifecycle_idle_clamped_to_1_to_86400() {
    use conduit::daemon::wizard_input::{
        clamp_idle_lifecycle, lifecycle_from_wizard, LIFECYCLE_EPHEMERAL, LIFECYCLE_PERSISTENT,
    };

    assert_eq!(clamp_idle_lifecycle(0), 1);
    assert_eq!(clamp_idle_lifecycle(-100), 1);
    assert_eq!(clamp_idle_lifecycle(300), 300);
    assert_eq!(clamp_idle_lifecycle(100_000), 86_400);
    assert_eq!(clamp_idle_lifecycle(86_400), 86_400);

    assert_eq!(lifecycle_from_wizard("ephemeral", 99), LIFECYCLE_EPHEMERAL);
    assert_eq!(
        lifecycle_from_wizard("persistent", 99),
        LIFECYCLE_PERSISTENT
    );
    assert_eq!(lifecycle_from_wizard("idle", 300), 300);
    assert_eq!(lifecycle_from_wizard("idle", -5), 1);
}

/// D2 gate: a server rename must FK-cascade to `managed_mcp` (the join table
/// that replaced the denormalized IDE blob) AND to `mcp_tool` decisions.
#[tokio::test]
async fn rename_server_cascades_to_managed_mcp_and_tool_decisions() {
    use conduit::daemon::db::types::{Decision, DecisionSource, McpTool};

    let db = Db::spawn_in_memory().unwrap();
    seed_client(&db, "Cursor").await;
    db.upsert_server_with_tools(
        sample_server("github"),
        vec![McpTool {
            server: "github".into(),
            name: "create_issue".into(),
            description: None,
            input_schema_json: "{}".into(),
            discovered_at: 1,
            decision: None,
        }],
    )
    .await
    .unwrap();
    db.set_tool_decision(
        "github",
        "create_issue",
        Decision::Allow,
        DecisionSource::User,
    )
    .await
    .unwrap();
    db.set_managed_servers("Cursor", "global", vec!["github".to_string()])
        .await
        .unwrap();

    db.rename_server("github", "github-org").await.unwrap();

    // managed_mcp followed the rename (ON UPDATE CASCADE).
    let managing = db.list_clients_managing_server("github-org").await.unwrap();
    assert_eq!(managing, vec![("Cursor".to_string(), "global".to_string())]);
    assert!(db
        .list_clients_managing_server("github")
        .await
        .unwrap()
        .is_empty());

    // The per-tool decision survived under the new server name.
    let (decision, _src) = db
        .resolve_policy("github-org", "create_issue", Decision::Ask)
        .await
        .unwrap();
    assert_eq!(decision, Decision::Allow);

    assert!(db.get_server("github").await.unwrap().is_none());
    assert!(db.get_server("github-org").await.unwrap().is_some());
}

/// Fresh probe stores tools with NULL decision (inherit) — no 'ask'/'probe'
/// seed. Re-probe deletes vanished tools (delete-on-absent).
#[tokio::test]
async fn probe_stores_null_decision_and_reprobe_drops_vanished() {
    use conduit::daemon::db::types::McpTool;

    let make = |name: &str, at: i64| McpTool {
        server: "github".into(),
        name: name.into(),
        description: None,
        input_schema_json: "{}".into(),
        discovered_at: at,
        decision: None,
    };

    let db = Db::spawn_in_memory().unwrap();
    db.upsert_server_with_tools(sample_server("github"), vec![make("a", 1), make("b", 1)])
        .await
        .unwrap();

    // Fresh tools inherit (NULL decision) — none carries a stored decision.
    let tools = db.list_server_tools("github").await.unwrap();
    assert_eq!(tools.len(), 2);
    assert!(tools.iter().all(|t| t.decision.is_none()));

    // Re-probe without "b": its row is deleted.
    db.upsert_server_with_tools(sample_server("github"), vec![make("a", 2)])
        .await
        .unwrap();
    let names: Vec<String> = db
        .list_server_tools("github")
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert_eq!(names, vec!["a".to_string()]);
    assert!(db.get_tool_decision("github", "b").await.unwrap().is_none());
}

#[tokio::test]
async fn env_secret_and_plain_serialise_correctly() {
    let db = Db::spawn_in_memory().expect("spawn");
    let s = sample_server("gmail");
    db.upsert_server(s.clone()).await.unwrap();
    let got = db.get_server("gmail").await.unwrap().unwrap();
    assert!(matches!(&got.env[0].value, EnvValue::Secret(u) if u == "uuid-1"));
    assert!(matches!(&got.env[1].value, EnvValue::Plain(v) if v == "1"));
}
