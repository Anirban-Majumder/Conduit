#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test ergonomics"
)]

use mcp_router::daemon::db::types::{EnvEntry, EnvValue, Server, TRANSPORT_HTTP, TRANSPORT_STDIO};
use mcp_router::daemon::db::Db;

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
    };
    db.upsert_server(s.clone()).await.unwrap();
    assert_eq!(db.get_server("linear").await.unwrap().unwrap(), s);
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
