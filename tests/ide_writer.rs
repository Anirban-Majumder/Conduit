#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test ergonomics"
)]

use std::path::PathBuf;

use mcp_router::daemon::ide_writer::{purge_uniform_mcp_servers, write_uniform_mcp_servers};
use serde_json::json;

fn fresh_tmp(name: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("mcp-router-ide-writer-{name}-{ns}"))
}

#[test]
fn write_creates_fresh_file_with_our_entries() {
    let dir = fresh_tmp("fresh");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("mcp.json");

    write_uniform_mcp_servers(&path, "key12345", &["gmail".to_string()], 9876).unwrap();

    let written: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        written["mcpServers"]["gmail"]["url"],
        "http://127.0.0.1:9876/key12345/gmail/mcp"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn write_preserves_users_other_top_level_keys_and_servers() {
    let dir = fresh_tmp("preserve");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("mcp.json");

    let existing = json!({
        "userSetting": "do not touch",
        "mcpServers": {
            "user-server": { "command": "their-binary" }
        }
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&existing).unwrap()).unwrap();

    write_uniform_mcp_servers(&path, "ourkey00", &["gmail".to_string()], 9876).unwrap();

    let after: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(after["userSetting"], "do not touch");
    assert!(after["mcpServers"]["user-server"]["command"] == "their-binary");
    assert!(after["mcpServers"]["gmail"]["url"]
        .as_str()
        .unwrap()
        .contains("ourkey00"));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn write_refuses_to_clobber_invalid_json() {
    let dir = fresh_tmp("invalid");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("mcp.json");
    std::fs::write(&path, b"{this is not json").unwrap();

    let result = write_uniform_mcp_servers(&path, "anykey00", &["gmail".to_string()], 9876);
    assert!(result.is_err(), "expected error, got {result:?}");

    let raw = std::fs::read(&path).unwrap();
    assert_eq!(raw, b"{this is not json", "should not clobber user's file");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn purge_removes_only_our_entries() {
    let dir = fresh_tmp("purge");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("mcp.json");

    write_uniform_mcp_servers(
        &path,
        "ourkey00",
        &["gmail".to_string(), "github".to_string()],
        9876,
    )
    .unwrap();
    // Splice in a user-added server alongside ours
    let mut current: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    current["mcpServers"]["user-server"] = json!({ "command": "their-bin" });
    std::fs::write(&path, serde_json::to_vec_pretty(&current).unwrap()).unwrap();

    purge_uniform_mcp_servers(&path, "ourkey00", 9876).unwrap();

    let after: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(after["mcpServers"]["gmail"].is_null());
    assert!(after["mcpServers"]["github"].is_null());
    assert!(after["mcpServers"]["user-server"]["command"] == "their-bin");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn purge_of_missing_file_is_idempotent() {
    let dir = fresh_tmp("missing");
    let path = dir.join("mcp.json");
    purge_uniform_mcp_servers(&path, "key12345", 9876).unwrap();
}
