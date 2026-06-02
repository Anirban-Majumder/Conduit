//! Phase 5b scoped `managed_mcp` CRUD + repo enumeration (§4).
//!
//! Directory-scope keys every managed row by `(client_id, scope, server_name)`,
//! `scope` being `'global'` or a normalized absolute repo dir. These tests pin
//! the scope-aware list/set/add/remove queries, the DISTINCT/UNION repo
//! enumeration across `managed_mcp` + `managed_skill`, and "remove from all".

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test ergonomics")]

use conduit::daemon::db::types::{McpServer, Skill};
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

fn skill(directory: &str, name: &str) -> Skill {
    Skill {
        directory: directory.into(),
        repo_owner: "o".into(),
        repo_name: "r".into(),
        repo_branch: "main".into(),
        source_directory: name.into(),
        skill_name: name.into(),
        description: String::new(),
        readme_url: None,
        installed_at: 0,
        targets: Vec::new(),
    }
}

async fn seed(db: &Db) {
    for s in ["gmail", "github"] {
        db.upsert_server(server(s)).await.unwrap();
    }
    db.project_builtin(
        "Cursor",
        "Cursor",
        "/x",
        None,
        "{}",
        Some(".cursor/mcp.json"),
        Some(".cursor/skills"),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn set_and_list_are_isolated_per_scope() {
    let db = Db::spawn_in_memory().unwrap();
    seed(&db).await;

    db.set_managed_servers("Cursor", "global", vec!["gmail".into()])
        .await
        .unwrap();
    db.set_managed_servers("Cursor", "/w/foo", vec!["github".into()])
        .await
        .unwrap();

    assert_eq!(
        db.list_managed_servers("Cursor", "global").await.unwrap(),
        vec!["gmail".to_string()]
    );
    assert_eq!(
        db.list_managed_servers("Cursor", "/w/foo").await.unwrap(),
        vec!["github".to_string()]
    );
}

#[tokio::test]
async fn add_and_remove_single_server_at_scope() {
    let db = Db::spawn_in_memory().unwrap();
    seed(&db).await;

    db.add_managed_server("Cursor", "/w/foo", "gmail")
        .await
        .unwrap();
    db.add_managed_server("Cursor", "/w/foo", "github")
        .await
        .unwrap();
    // Idempotent insert.
    db.add_managed_server("Cursor", "/w/foo", "gmail")
        .await
        .unwrap();
    assert_eq!(
        db.list_managed_servers("Cursor", "/w/foo").await.unwrap(),
        vec!["github".to_string(), "gmail".to_string()]
    );

    db.remove_managed_server("Cursor", "/w/foo", "gmail")
        .await
        .unwrap();
    assert_eq!(
        db.list_managed_servers("Cursor", "/w/foo").await.unwrap(),
        vec!["github".to_string()]
    );
    // Global was never touched.
    assert!(db
        .list_managed_servers("Cursor", "global")
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn clients_managing_server_returns_every_scope_pair() {
    let db = Db::spawn_in_memory().unwrap();
    seed(&db).await;
    db.add_managed_server("Cursor", "global", "gmail")
        .await
        .unwrap();
    db.add_managed_server("Cursor", "/w/foo", "gmail")
        .await
        .unwrap();

    let mut pairs = db.list_clients_managing_server("gmail").await.unwrap();
    pairs.sort();
    assert_eq!(
        pairs,
        vec![
            ("Cursor".to_string(), "/w/foo".to_string()),
            ("Cursor".to_string(), "global".to_string()),
        ]
    );
}

#[tokio::test]
async fn remove_server_all_scopes_clears_everywhere() {
    let db = Db::spawn_in_memory().unwrap();
    seed(&db).await;
    db.add_managed_server("Cursor", "global", "gmail")
        .await
        .unwrap();
    db.add_managed_server("Cursor", "/w/foo", "gmail")
        .await
        .unwrap();
    db.add_managed_server("Cursor", "/w/bar", "gmail")
        .await
        .unwrap();

    db.remove_server_all_scopes("Cursor", "gmail")
        .await
        .unwrap();
    assert!(db
        .list_clients_managing_server("gmail")
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn known_repos_is_distinct_union_excluding_global() {
    let db = Db::spawn_in_memory().unwrap();
    seed(&db).await;
    db.upsert_skill_metadata(skill("o/r/a", "a")).await.unwrap();

    db.add_managed_server("Cursor", "global", "gmail")
        .await
        .unwrap();
    db.add_managed_server("Cursor", "/w/foo", "gmail")
        .await
        .unwrap();
    // A skill row contributes its own repo scope to the union.
    db.add_skill_target("o/r/a", "Cursor", "/w/bar")
        .await
        .unwrap();
    // Duplicate scope across the two tables must collapse.
    db.add_skill_target("o/r/a", "Cursor", "/w/foo")
        .await
        .unwrap();

    let repos = db.list_known_repos().await.unwrap();
    assert_eq!(repos, vec!["/w/bar".to_string(), "/w/foo".to_string()]);
}

#[tokio::test]
async fn scopes_for_client_includes_global_and_repos_across_tables() {
    let db = Db::spawn_in_memory().unwrap();
    seed(&db).await;
    db.upsert_skill_metadata(skill("o/r/a", "a")).await.unwrap();

    db.add_managed_server("Cursor", "global", "gmail")
        .await
        .unwrap();
    db.add_managed_server("Cursor", "/w/foo", "github")
        .await
        .unwrap();
    db.add_skill_target("o/r/a", "Cursor", "/w/bar")
        .await
        .unwrap();

    let scopes = db.list_scopes_for_client("Cursor").await.unwrap();
    assert_eq!(
        scopes,
        vec![
            "/w/bar".to_string(),
            "/w/foo".to_string(),
            "global".to_string(),
        ]
    );
}
