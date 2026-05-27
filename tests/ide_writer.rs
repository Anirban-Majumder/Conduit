#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test ergonomics"
)]

use std::path::PathBuf;

use conduit::daemon::clients::{
    purge_one_server_for_ide, purge_uniform_mcp_servers, read_raw, validate_json, write_raw,
    write_uniform_mcp_servers, IdeKind, JsonSchema,
};
use serde_json::json;

// Cursor's shape: top-level `mcpServers`, no `type` field. The shared JSON
// helpers used to hardcode this; the tests still exercise that shape.
const CURSOR_SCHEMA: JsonSchema = JsonSchema {
    top_level_key: "mcpServers",
    server_type: None,
};

fn fresh_tmp(name: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("conduit-ide-writer-{name}-{ns}"))
}

#[test]
fn write_creates_fresh_file_with_our_entries() {
    let dir = fresh_tmp("fresh");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("mcp.json");

    write_uniform_mcp_servers(
        &path,
        &CURSOR_SCHEMA,
        "key12345",
        &["gmail".to_string()],
        9876,
    )
    .unwrap();

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

    write_uniform_mcp_servers(
        &path,
        &CURSOR_SCHEMA,
        "ourkey00",
        &["gmail".to_string()],
        9876,
    )
    .unwrap();

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

    let result = write_uniform_mcp_servers(
        &path,
        &CURSOR_SCHEMA,
        "anykey00",
        &["gmail".to_string()],
        9876,
    );
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
        &CURSOR_SCHEMA,
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

    purge_uniform_mcp_servers(&path, &CURSOR_SCHEMA, "ourkey00", 9876).unwrap();

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
    purge_uniform_mcp_servers(&path, &CURSOR_SCHEMA, "key12345", 9876).unwrap();
}

#[test]
fn purge_one_server_drops_just_that_entry() {
    let dir = fresh_tmp("purge-one");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("mcp.json");
    write_uniform_mcp_servers(
        &path,
        &CURSOR_SCHEMA,
        "abcd1234",
        &["gmail".to_string(), "github".to_string()],
        9876,
    )
    .unwrap();

    purge_one_server_for_ide(IdeKind::Cursor, &path, 9876, "abcd1234", "gmail").unwrap();

    let after: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(after["mcpServers"]["gmail"].is_null());
    assert!(after["mcpServers"]["github"]["url"].is_string());
}

#[test]
fn purge_one_keeps_top_level_settings() {
    let dir = fresh_tmp("purge-top-level");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("mcp.json");
    write_uniform_mcp_servers(
        &path,
        &CURSOR_SCHEMA,
        "k0000000",
        &["gmail".to_string()],
        9876,
    )
    .unwrap();
    let mut cur: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    cur["editor"] = json!({ "fontSize": 14 });
    std::fs::write(&path, serde_json::to_vec_pretty(&cur).unwrap()).unwrap();

    purge_one_server_for_ide(IdeKind::Cursor, &path, 9876, "k0000000", "gmail").unwrap();

    let after: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(after["editor"]["fontSize"], 14);
}

#[test]
fn purge_one_keeps_other_routers() {
    let dir = fresh_tmp("purge-other-router");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("mcp.json");
    write_uniform_mcp_servers(
        &path,
        &CURSOR_SCHEMA,
        "ourskey0",
        &["gmail".to_string()],
        9876,
    )
    .unwrap();
    let mut cur: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    cur["mcpServers"]["other"] = json!({
        "url": "http://localhost:9999/other/svc/mcp"
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&cur).unwrap()).unwrap();

    purge_one_server_for_ide(IdeKind::Cursor, &path, 9876, "ourskey0", "gmail").unwrap();

    let after: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(after["mcpServers"]["gmail"].is_null());
    assert!(after["mcpServers"]["other"]["url"].is_string());
}

#[test]
fn purge_one_keeps_stdio_entries() {
    let dir = fresh_tmp("purge-stdio");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("mcp.json");
    write_uniform_mcp_servers(
        &path,
        &CURSOR_SCHEMA,
        "k0000000",
        &["gmail".to_string()],
        9876,
    )
    .unwrap();
    let mut cur: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    cur["mcpServers"]["my-custom"] = json!({ "command": "/usr/local/bin/x" });
    std::fs::write(&path, serde_json::to_vec_pretty(&cur).unwrap()).unwrap();

    purge_one_server_for_ide(IdeKind::Cursor, &path, 9876, "k0000000", "gmail").unwrap();

    let after: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(after["mcpServers"]["gmail"].is_null());
    assert_eq!(
        after["mcpServers"]["my-custom"]["command"],
        "/usr/local/bin/x"
    );
}

#[test]
fn validate_json_rejects_trailing_comma() {
    assert!(validate_json("{\"a\":1,}").is_err());
}

#[test]
fn validate_json_accepts_empty_object() {
    assert!(validate_json("{}").is_ok());
}

#[test]
fn write_raw_round_trips_via_read_raw() {
    let dir = fresh_tmp("raw");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("mcp.json");
    write_raw(&path, r#"{"mcpServers":{}}"#).unwrap();
    let got = read_raw(&path).unwrap().unwrap();
    let v: serde_json::Value = serde_json::from_str(&got).unwrap();
    assert!(v["mcpServers"].is_object());
}

#[test]
fn read_raw_returns_none_for_missing_file() {
    let dir = fresh_tmp("read-missing");
    let path = dir.join("does-not-exist.json");
    assert!(read_raw(&path).unwrap().is_none());
}

// Per-IDE schema sanity checks. Each IDE's docs / source dictates a specific
// top-level key + optional `type` discriminator. If any of these regress, the
// IDE rejects the file at load time (OpenCode in particular hard-errors).
#[test]
fn write_emits_claude_code_shape_with_type_http() {
    use conduit::daemon::clients::config_path_under;
    let home = fresh_tmp("ide-claude-code");
    std::fs::create_dir_all(&home).unwrap();
    let cfg = config_path_under(IdeKind::ClaudeCode, &home);
    // Sanity: Claude Code reads ~/.claude.json (not ~/.claude/claude_desktop_config.json).
    assert_eq!(cfg, home.join(".claude.json"));
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();

    // Exercise the shared helper with the same schema the dispatcher uses.
    write_uniform_mcp_servers(
        &cfg,
        &JsonSchema {
            top_level_key: "mcpServers",
            server_type: Some("http"),
        },
        "claude123",
        &["gmail".to_string()],
        9876,
    )
    .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&cfg).unwrap()).unwrap();
    assert_eq!(v["mcpServers"]["gmail"]["type"], "http");
    assert_eq!(
        v["mcpServers"]["gmail"]["url"],
        "http://127.0.0.1:9876/claude123/gmail/mcp"
    );
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn write_emits_vscode_copilot_shape_servers_with_type_http() {
    let dir = fresh_tmp("ide-vscode");
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = dir.join("mcp.json");
    write_uniform_mcp_servers(
        &cfg,
        &JsonSchema {
            top_level_key: "servers",
            server_type: Some("http"),
        },
        "vsc12345",
        &["gmail".to_string()],
        9876,
    )
    .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&cfg).unwrap()).unwrap();
    assert!(v["mcpServers"].is_null(), "must NOT use mcpServers key");
    assert_eq!(v["servers"]["gmail"]["type"], "http");
    assert!(v["servers"]["gmail"]["url"].is_string());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn write_emits_opencode_shape_mcp_with_type_remote() {
    let dir = fresh_tmp("ide-opencode");
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = dir.join("opencode.json");
    write_uniform_mcp_servers(
        &cfg,
        &JsonSchema {
            top_level_key: "mcp",
            server_type: Some("remote"),
        },
        "opcd1234",
        &["gmail".to_string()],
        9876,
    )
    .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&cfg).unwrap()).unwrap();
    assert!(v["mcpServers"].is_null(), "must NOT use mcpServers key");
    assert_eq!(v["mcp"]["gmail"]["type"], "remote");
    assert!(v["mcp"]["gmail"]["url"].is_string());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn write_emits_cursor_shape_without_type_field() {
    let dir = fresh_tmp("ide-cursor");
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = dir.join("mcp.json");
    write_uniform_mcp_servers(
        &cfg,
        &CURSOR_SCHEMA,
        "cur12345",
        &["gmail".to_string()],
        9876,
    )
    .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&cfg).unwrap()).unwrap();
    assert_eq!(v["mcpServers"]["gmail"]["type"], serde_json::Value::Null);
    assert!(v["mcpServers"]["gmail"]["url"].is_string());
    std::fs::remove_dir_all(&dir).ok();
}
