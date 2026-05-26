#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test ergonomics"
)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use mcp_router::daemon::cascade::delete_server_with_cascade;
use mcp_router::daemon::db::types::{
    ApprovalStatus, ManagedIdeConfig, PendingApproval, Server, ToolMeta,
};
use mcp_router::daemon::db::Db;
use mcp_router::daemon::events::{Bus, Event};
use mcp_router::daemon::ide_writer::write_uniform_mcp_servers;

fn unique_dir(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!("mcp-cascade-{}-{tag}-{n}", std::process::id()))
}

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
    }
}

#[tokio::test]
async fn cascade_trims_ide_configs_and_deletes_server_row() {
    let db = Db::spawn_in_memory().unwrap();
    let bus = Bus::new();
    let dir = unique_dir("trim");
    std::fs::create_dir_all(&dir).unwrap();
    let cfg_path = dir.join("mcp.json");
    write_uniform_mcp_servers(
        &cfg_path,
        "abcd1234",
        &["gmail".to_string(), "github".to_string()],
        9876,
    )
    .unwrap();

    db.upsert_server_with_tools(
        server("gmail"),
        vec![ToolMeta {
            name: "send".into(),
            description: None,
            input_schema_json: "{}".into(),
            discovered_at: 0,
        }],
    )
    .await
    .unwrap();
    db.upsert_server_with_tools(server("github"), vec![])
        .await
        .unwrap();
    db.record_ide_config_write(ManagedIdeConfig {
        ide_name: "Cursor".into(),
        ide_key: "abcd1234".into(),
        config_path: cfg_path.clone(),
        managed_entries: vec!["gmail".into(), "github".into()],
        written_at: 0,
    })
    .await
    .unwrap();

    let report = delete_server_with_cascade(&db, &bus, "gmail", 9876, false)
        .await
        .unwrap();

    assert_eq!(report.ides_trimmed.len(), 1);
    assert_eq!(report.ides_trimmed[0].remaining, vec!["github".to_string()]);
    assert!(report.ides_skipped.is_empty());
    assert!(db.get_server("gmail").await.unwrap().is_none());
    assert!(db.get_server("github").await.unwrap().is_some());

    let updated: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&cfg_path).unwrap()).unwrap();
    assert!(updated["mcpServers"]["gmail"].is_null());
    assert!(updated["mcpServers"]["github"]["url"].is_string());

    let stored = db.list_ide_configs_for_ide("Cursor").await.unwrap();
    assert_eq!(stored[0].managed_entries, vec!["github".to_string()]);

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
        server: "gmail".into(),
        tool: "send".into(),
        args_json: "{}".into(),
        ide_key: "k0000000".into(),
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

    // Bus published the cancellation.
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
async fn cascade_skips_missing_ide_config_files() {
    let db = Db::spawn_in_memory().unwrap();
    let bus = Bus::new();
    let missing = unique_dir("missing").join("never-created.json");

    db.upsert_server_with_tools(server("gmail"), vec![])
        .await
        .unwrap();
    db.record_ide_config_write(ManagedIdeConfig {
        ide_name: "Cursor".into(),
        ide_key: "k0000000".into(),
        config_path: missing.clone(),
        managed_entries: vec!["gmail".into()],
        written_at: 0,
    })
    .await
    .unwrap();

    let report = delete_server_with_cascade(&db, &bus, "gmail", 9876, false)
        .await
        .unwrap();
    // purge_one_server_for_ide is a no-op for missing files, so the IDE counts
    // as "trimmed" (the entry it claims is gone is verifiably absent).
    assert_eq!(report.ides_skipped.len(), 0);
    assert!(db.get_server("gmail").await.unwrap().is_none());
}
