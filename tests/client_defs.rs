//! Static client-def correctness: catalog membership, path resolution, and
//! entry-template rendering for Claude Desktop and Claude Code.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test ergonomics"
)]

use std::path::PathBuf;

use conduit::client::claude_code::DEF as CLAUDE_CODE;
use conduit::client::claude_desktop::DEF as CLAUDE_DESKTOP;
use conduit::daemon::client_config::paths::{resolve_under, PathSpec};
use conduit::daemon::client_config::render::render_servers;
use conduit::daemon::client_config::types::ConfigSchema;
use conduit::daemon::client_config::{builtin_by_id, BUILTINS};

// ── Catalog membership ────────────────────────────────────────────────────────

#[test]
fn claude_desktop_in_builtins() {
    assert!(
        BUILTINS.iter().any(|d| d.id == "ClaudeDesktop"),
        "ClaudeDesktop must appear in BUILTINS"
    );
}

#[test]
fn claude_desktop_lookup_by_id() {
    let def = builtin_by_id("ClaudeDesktop").expect("ClaudeDesktop must be in BUILTINS");
    assert_eq!(def.display_name, "Claude Desktop");
}

#[test]
fn builtin_ids_are_unique() {
    let mut ids: Vec<&str> = BUILTINS.iter().map(|d| d.id).collect();
    ids.sort_unstable();
    let original_len = ids.len();
    ids.dedup();
    assert_eq!(ids.len(), original_len, "duplicate BUILTINS ids detected");
}

// ── Claude Desktop path resolution ───────────────────────────────────────────

/// `ConfigDir` resolves to `<home>/.config/<rel>` in the test seam (Linux layout).
/// On macOS the real resolver would give `~/Library/Application Support/...`;
/// `resolve_under` exercises the cross-platform path logic without OS deps.
#[test]
fn claude_desktop_config_path_under_fake_home() {
    let home = PathBuf::from("/home/testuser");
    let resolved = resolve_under(&CLAUDE_DESKTOP.config, &home);
    assert_eq!(
        resolved,
        PathBuf::from("/home/testuser/.config/Claude/claude_desktop_config.json"),
        "ConfigDir resolves to .config/... relative to home in test seam"
    );
}

#[test]
fn claude_desktop_config_is_config_dir_variant() {
    // Verify the PathSpec variant so any accidental change from ConfigDir is caught.
    assert!(
        matches!(CLAUDE_DESKTOP.config, PathSpec::ConfigDir(_)),
        "ClaudeDesktop config must use PathSpec::ConfigDir"
    );
}

#[test]
fn claude_desktop_key_path_is_mcp_servers() {
    assert_eq!(CLAUDE_DESKTOP.config_spec.key_path, &["mcpServers"]);
}

#[test]
fn claude_desktop_has_no_skills() {
    assert!(
        CLAUDE_DESKTOP.skills.is_none(),
        "Claude Desktop has no skills support"
    );
}

// ── Claude Desktop entry template ────────────────────────────────────────────

fn fresh_tmp(tag: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("conduit-client-defs-{tag}-{ns}"))
}

/// Rendering a Claude Desktop entry must produce an mcp-remote stdio shim
/// that embeds the router URL in the args array, not a bare `url` field.
#[test]
fn claude_desktop_entry_is_mcp_remote_shim_with_url() {
    let dir = fresh_tmp("cd-render");
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = dir.join("claude_desktop_config.json");

    render_servers(
        &ConfigSchema::from(&CLAUDE_DESKTOP.config_spec),
        &cfg,
        "desk1234",
        &["gmail".to_string()],
        9876,
    )
    .unwrap();

    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&cfg).unwrap()).unwrap();
    let entry = &v["mcpServers"]["gmail"];
    assert_eq!(entry["command"], "npx", "must use npx as command");
    let args = entry["args"].as_array().expect("args must be an array");
    let url_arg = args
        .iter()
        .find(|a| a.as_str().is_some_and(|s| s.contains("desk1234")))
        .expect("args must contain the router URL");
    assert!(
        url_arg
            .as_str()
            .unwrap()
            .contains("http://127.0.0.1:9876/desk1234/gmail/mcp"),
        "router URL must appear in args"
    );
    assert!(
        entry["url"].is_null(),
        "must NOT have a top-level url field (STDIO shim, not SSE)"
    );

    std::fs::remove_dir_all(&dir).ok();
}

// ── CC-8: Claude Code no longer uses mac_app ─────────────────────────────────

#[test]
fn claude_code_binary_is_claude() {
    assert!(
        CLAUDE_CODE.binaries.contains(&"claude"),
        "Claude Code must be detected via the 'claude' binary"
    );
}

/// On all platforms, detection of Claude Code must not rely on a macOS app bundle
/// (Claude.app belongs to Claude Desktop, not the CLI).
#[cfg(target_os = "macos")]
#[test]
fn claude_code_mac_app_is_none() {
    assert!(
        CLAUDE_CODE.mac_app.is_none(),
        "Claude Code must not use mac_app — Claude.app is Claude Desktop (CC-8)"
    );
}

/// Claude Desktop uses Claude.app on macOS for detection.
#[cfg(target_os = "macos")]
#[test]
fn claude_desktop_mac_app_is_claude_app() {
    assert_eq!(
        CLAUDE_DESKTOP.mac_app,
        Some("Claude.app"),
        "Claude Desktop must detect via Claude.app on macOS"
    );
}
