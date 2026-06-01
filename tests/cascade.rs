//! D2/§9 gate: server deletion re-derives each affected client's config file
//! from its surviving `managed_mcp` rows (no denormalized blob), cancels pending
//! approvals, and removes the server row + its tool decisions via FK cascade.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test ergonomics"
)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use conduit::daemon::cascade::delete_server_with_cascade;
use conduit::daemon::client_config::builtin_by_id;
use conduit::daemon::client_config::render::render_servers;
use conduit::daemon::client_config::types::ConfigSchema;
use conduit::daemon::db::types::{ApprovalStatus, McpServer, McpTool, PendingApproval};
use conduit::daemon::db::Db;
use conduit::daemon::events::{Bus, Event};

fn unique_dir(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!("mcp-cascade-{}-{tag}-{n}", std::process::id()))
}

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

fn cursor_schema() -> ConfigSchema {
    ConfigSchema::from(&builtin_by_id("Cursor").unwrap().config_spec)
}

/// Project a Cursor client whose config path is `cfg_path`, managing the given
/// servers, with the file rendered to match. Returns the client's `mcp_key`.
async fn seed_cursor(db: &Db, cfg_path: &std::path::Path, servers: &[&str]) -> String {
    let schema_json = serde_json::to_string(&cursor_schema()).unwrap();
    let row = db
        .project_builtin(
            "Cursor",
            "Cursor",
            &cfg_path.display().to_string(),
            None,
            &schema_json,
        )
        .await
        .unwrap();
    let names: Vec<String> = servers.iter().map(|s| (*s).to_string()).collect();
    db.set_managed_servers("Cursor", names.clone())
        .await
        .unwrap();
    render_servers(&cursor_schema(), cfg_path, &row.mcp_key, &names, 9876).unwrap();
    row.mcp_key
}

#[tokio::test]
async fn cascade_re_derives_client_config_and_deletes_server_row() {
    let db = Db::spawn_in_memory().unwrap();
    let bus = Bus::new();
    let dir = unique_dir("trim");
    std::fs::create_dir_all(&dir).unwrap();
    let cfg_path = dir.join("mcp.json");

    db.upsert_server_with_tools(
        server("gmail"),
        vec![McpTool {
            server: "gmail".into(),
            name: "send".into(),
            description: None,
            input_schema_json: "{}".into(),
            discovered_at: 0,
            decision: None,
        }],
    )
    .await
    .unwrap();
    db.upsert_server_with_tools(server("github"), vec![])
        .await
        .unwrap();
    seed_cursor(&db, &cfg_path, &["gmail", "github"]).await;

    let report = delete_server_with_cascade(&db, &bus, "gmail", 9876, false)
        .await
        .unwrap();

    // The client's config was re-derived.
    assert_eq!(report.rewritten_clients, vec!["Cursor".to_string()]);
    assert!(db.get_server("gmail").await.unwrap().is_none());
    assert!(db.get_server("github").await.unwrap().is_some());

    let updated: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&cfg_path).unwrap()).unwrap();
    assert!(
        updated["mcpServers"]["gmail"].is_null(),
        "deleted server purged from file"
    );
    assert!(
        updated["mcpServers"]["github"]["url"].is_string(),
        "survivor kept"
    );

    // managed_mcp reflects the survivor only.
    assert_eq!(
        db.list_managed_servers("Cursor").await.unwrap(),
        vec!["github".to_string()]
    );

    let _ignored = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn cascade_cancels_pending_approvals_for_deleted_server() {
    let db = Db::spawn_in_memory().unwrap();
    let bus = Bus::new();
    let mut rx = bus.subscribe();

    db.upsert_server_with_tools(server("gmail"), vec![])
        .await
        .unwrap();
    db.create_pending_approval(PendingApproval {
        id: "uuid-1".into(),
        mcp_server: "gmail".into(),
        tool: "send".into(),
        args_json: "{}".into(),
        client_key: "k0000000".into(),
        created_at: 0,
        status: ApprovalStatus::Pending,
    })
    .await
    .unwrap();

    let report = delete_server_with_cascade(&db, &bus, "gmail", 9876, false)
        .await
        .unwrap();
    assert_eq!(report.cancelled_approvals, vec!["uuid-1".to_string()]);

    let row = db.get_pending_approval("uuid-1").await.unwrap().unwrap();
    assert_eq!(row.status, ApprovalStatus::Cancelled);

    let evt = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .unwrap()
        .unwrap();
    match evt {
        Event::ApprovalResolved { id, status } => {
            assert_eq!(id, "uuid-1");
            assert_eq!(status, ApprovalStatus::Cancelled);
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[tokio::test]
async fn cascade_tolerates_missing_client_config_file() {
    let db = Db::spawn_in_memory().unwrap();
    let bus = Bus::new();
    let missing = unique_dir("missing").join("never-created.json");

    db.upsert_server_with_tools(server("gmail"), vec![])
        .await
        .unwrap();
    // Project a client whose file was never written; managed_mcp lists gmail.
    let schema_json = serde_json::to_string(&cursor_schema()).unwrap();
    db.project_builtin(
        "Cursor",
        "Cursor",
        &missing.display().to_string(),
        None,
        &schema_json,
    )
    .await
    .unwrap();
    db.set_managed_servers("Cursor", vec!["gmail".to_string()])
        .await
        .unwrap();

    // Re-derive writes a fresh (empty-set) file — no panic on the missing file.
    let report = delete_server_with_cascade(&db, &bus, "gmail", 9876, false)
        .await
        .unwrap();
    assert_eq!(report.rewritten_clients, vec!["Cursor".to_string()]);
    assert!(db.get_server("gmail").await.unwrap().is_none());
}
