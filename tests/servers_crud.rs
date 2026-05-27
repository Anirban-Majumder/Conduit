#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test ergonomics"
)]

use conduit::daemon::db::types::{EnvEntry, EnvValue, Server, TRANSPORT_HTTP, TRANSPORT_STDIO};
use conduit::daemon::db::Db;

fn sample_server(name: &str) -> Server {
    Server {
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
        last_init_duration_ms: Some(123),
        package_version: Some("0.1.0".to_string()),
        mcp_self_version: Some("0.2.0".to_string()),
        created_at: 1_700_000_000,
        server_name: None,
        server_version: None,
        last_probed_at: None,
        last_probe_error: None,
    }
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
    let s = Server {
        name: "linear".to_string(),
        transport: TRANSPORT_HTTP.to_string(),
        command: None,
        args: vec![],
        env: vec![],
        url: Some("https://mcp.linear.app/sse".to_string()),
        lifecycle_seconds: 300,
        last_init_duration_ms: None,
        package_version: None,
        mcp_self_version: None,
        created_at: 0,
        server_name: None,
        server_version: None,
        last_probed_at: None,
        last_probe_error: None,
    };
    db.upsert_server(s.clone()).await.unwrap();
    assert_eq!(db.get_server("linear").await.unwrap().unwrap(), s);
}

#[test]
fn invalid_server_names_rejected() {
    use conduit::daemon::server_validators::is_valid_server_name;

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
    use conduit::daemon::server_validators::{
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

#[tokio::test]
async fn rename_server_cascades_to_policies_and_server_tools() {
    use conduit::daemon::db::types::{Decision, DecisionSource, ToolMeta};

    let db = Db::spawn_in_memory().unwrap();
    let s = sample_server("github");
    let tools = vec![ToolMeta {
        name: "create_issue".into(),
        description: None,
        input_schema_json: "{}".into(),
        discovered_at: 1,
    }];
    db.upsert_server_with_tools(s, tools).await.unwrap();
    db.set_policy(
        "github",
        "create_issue",
        Decision::Allow,
        DecisionSource::User,
    )
    .await
    .unwrap();

    db.rename_server("github", "github-org").await.unwrap();

    // FK ON UPDATE CASCADE propagates the rename.
    let p = db
        .get_policy("github-org", "create_issue")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(p.decision, Decision::Allow);
    let tools = db.list_server_tools("github-org").await.unwrap();
    assert_eq!(tools.len(), 1);
    assert!(db.get_server("github").await.unwrap().is_none());
    assert!(db.get_server("github-org").await.unwrap().is_some());
}

#[tokio::test]
async fn upsert_server_with_tools_seeds_per_tool_policies_and_fallback() {
    use conduit::daemon::db::types::ToolMeta;

    let db = Db::spawn_in_memory().unwrap();
    let s = sample_server("github");
    let tools = vec![
        ToolMeta {
            name: "create_issue".into(),
            description: Some("Create a GitHub issue".into()),
            input_schema_json: "{}".into(),
            discovered_at: 1_700_000_000,
        },
        ToolMeta {
            name: "list_issues".into(),
            description: None,
            input_schema_json: "{}".into(),
            discovered_at: 1_700_000_000,
        },
    ];

    db.upsert_server_with_tools(s, tools.clone()).await.unwrap();

    let got_tools = db.list_server_tools("github").await.unwrap();
    assert_eq!(got_tools.len(), 2);
    assert_eq!(got_tools[0].name, "create_issue");

    let policies = db.list_policies_for_server("github").await.unwrap();
    // Two per-tool rows + one scope='server' fallback.
    assert!(
        policies.len() >= 3,
        "expected per-tool + fallback policies, got {policies:?}"
    );
    for p in &policies {
        assert_eq!(p.decision, conduit::daemon::db::types::Decision::Ask);
        assert_eq!(
            p.decision_source,
            conduit::daemon::db::types::DecisionSource::Probe
        );
    }
}

#[tokio::test]
async fn re_probe_drops_vanished_tools_and_their_policies() {
    use conduit::daemon::db::types::ToolMeta;

    let db = Db::spawn_in_memory().unwrap();
    let s = sample_server("github");
    let tools_v1 = vec![
        ToolMeta {
            name: "a".into(),
            description: None,
            input_schema_json: "{}".into(),
            discovered_at: 1,
        },
        ToolMeta {
            name: "b".into(),
            description: None,
            input_schema_json: "{}".into(),
            discovered_at: 1,
        },
    ];
    db.upsert_server_with_tools(s.clone(), tools_v1)
        .await
        .unwrap();

    let tools_v2 = vec![ToolMeta {
        name: "a".into(),
        description: None,
        input_schema_json: "{}".into(),
        discovered_at: 2,
    }];
    db.upsert_server_with_tools(s, tools_v2).await.unwrap();

    let names: Vec<String> = db
        .list_server_tools("github")
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert_eq!(names, vec!["a".to_string()]);

    let pol_b = db.get_policy("github", "b").await.unwrap();
    assert!(
        pol_b.is_none(),
        "policy for vanished tool 'b' should be gone"
    );
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
