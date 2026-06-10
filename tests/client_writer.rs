//! Renderer fidelity gate (§3): the one `render`/`purge` path must reproduce
//! every built-in's exact MCP-config output, preserve user entries, and obey
//! the four pinned rules. Drives the real `BUILTINS` schemas, not hand-rolled
//! ones, so a DEF drift is caught here.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test ergonomics"
)]

use std::path::PathBuf;

use conduit::daemon::client_config::render::{purge_mcp, purge_one, render_scope, render_servers};
use conduit::daemon::client_config::types::{ConfigSchema, ResolvedClient, Scope};
use conduit::daemon::client_config::{builtin_by_id, read_raw, validate_json, write_raw};
use serde_json::json;

/// The `ConfigSchema` for a built-in, as projected into the DB.
fn schema_of(id: &str) -> ConfigSchema {
    ConfigSchema::from(&builtin_by_id(id).expect("known built-in").config_spec)
}

fn cursor_schema() -> ConfigSchema {
    schema_of("Cursor")
}

fn fresh_tmp(name: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("conduit-client-writer-{name}-{ns}"))
}

#[test]
fn write_creates_fresh_file_with_our_entries() {
    let dir = fresh_tmp("fresh");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("mcp.json");

    render_servers(
        &cursor_schema(),
        &path,
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
        "mcpServers": { "user-server": { "command": "their-binary" } }
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&existing).unwrap()).unwrap();

    render_servers(
        &cursor_schema(),
        &path,
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

    let result = render_servers(
        &cursor_schema(),
        &path,
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

    render_servers(
        &cursor_schema(),
        &path,
        "ourkey00",
        &["gmail".to_string(), "github".to_string()],
        9876,
    )
    .unwrap();
    let mut current: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    current["mcpServers"]["user-server"] = json!({ "command": "their-bin" });
    std::fs::write(&path, serde_json::to_vec_pretty(&current).unwrap()).unwrap();

    purge_mcp(&cursor_schema(), &path, "ourkey00", 9876).unwrap();

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
    purge_mcp(&cursor_schema(), &path, "key12345", 9876).unwrap();
}

#[test]
fn purge_one_server_drops_just_that_entry() {
    let dir = fresh_tmp("purge-one");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("mcp.json");
    render_servers(
        &cursor_schema(),
        &path,
        "abcd1234",
        &["gmail".to_string(), "github".to_string()],
        9876,
    )
    .unwrap();

    purge_one(&cursor_schema(), &path, "abcd1234", "gmail", 9876).unwrap();

    let after: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(after["mcpServers"]["gmail"].is_null());
    assert!(after["mcpServers"]["github"]["url"].is_string());
}

#[test]
fn purge_one_keeps_other_routers_and_stdio_entries() {
    let dir = fresh_tmp("purge-keep");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("mcp.json");
    render_servers(
        &cursor_schema(),
        &path,
        "ourskey0",
        &["gmail".to_string()],
        9876,
    )
    .unwrap();
    let mut cur: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    cur["editor"] = json!({ "fontSize": 14 });
    cur["mcpServers"]["other"] = json!({ "url": "http://localhost:9999/other/svc/mcp" });
    cur["mcpServers"]["my-custom"] = json!({ "command": "/usr/local/bin/x" });
    std::fs::write(&path, serde_json::to_vec_pretty(&cur).unwrap()).unwrap();

    purge_one(&cursor_schema(), &path, "ourskey0", "gmail", 9876).unwrap();

    let after: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(after["mcpServers"]["gmail"].is_null());
    assert_eq!(after["editor"]["fontSize"], 14);
    assert!(after["mcpServers"]["other"]["url"].is_string());
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

// ── Built-in shape fidelity: each DEF reproduces its exact entry JSON/TOML ───

#[test]
fn claude_code_emits_type_http_under_mcpservers() {
    let dir = fresh_tmp("bi-claude");
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = dir.join("config.json");
    render_servers(
        &schema_of("ClaudeCode"),
        &cfg,
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
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn vscode_copilot_uses_servers_key_with_type_http() {
    let dir = fresh_tmp("bi-vscode");
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = dir.join("mcp.json");
    render_servers(
        &schema_of("VsCodeCopilot"),
        &cfg,
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
fn opencode_uses_mcp_key_with_type_remote() {
    let dir = fresh_tmp("bi-opencode");
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = dir.join("opencode.json");
    render_servers(
        &schema_of("OpenCode"),
        &cfg,
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
fn cursor_emits_url_only_no_type_field() {
    let dir = fresh_tmp("bi-cursor");
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = dir.join("mcp.json");
    render_servers(
        &cursor_schema(),
        &cfg,
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

// ── Directory-scope (repo-local) writes + empty-repo cleanup (§5) ────────────

/// A built-in `ResolvedClient` pointed at a throwaway global path; repo-relative
/// paths come straight from the DEF so `config_target(Repo)` resolves correctly.
fn resolved(id: &str) -> ResolvedClient {
    let def = builtin_by_id(id).expect("known built-in");
    ResolvedClient {
        id: id.to_string(),
        display_name: id.to_string(),
        is_builtin: true,
        mcp_key: "key12345".to_string(),
        config_path: fresh_tmp("unused-global").join("global.json"),
        skills_dir: None,
        schema: ConfigSchema::from(&def.config_spec),
        project_config_rel: def.project_config_rel.map(PathBuf::from),
        project_skills_rel: def.project_skills_rel.map(PathBuf::from),
    }
}

#[test]
fn render_scope_writes_repo_file_at_project_relative_path() {
    let repo = fresh_tmp("repo-write");
    std::fs::create_dir_all(&repo).unwrap();
    let client = resolved("Cursor");

    render_scope(
        &client,
        &Scope::Repo(repo.clone()),
        &["gmail".to_string()],
        9876,
    )
    .unwrap();

    let path = repo.join(".cursor/mcp.json");
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        v["mcpServers"]["gmail"]["url"],
        "http://127.0.0.1:9876/key12345/gmail/mcp"
    );
    std::fs::remove_dir_all(&repo).ok();
}

#[test]
fn render_scope_last_removal_deletes_ours_created_file_and_dir() {
    let repo = fresh_tmp("repo-clean");
    std::fs::create_dir_all(&repo).unwrap();
    let client = resolved("Cursor");
    let scope = Scope::Repo(repo.clone());

    render_scope(&client, &scope, &["gmail".to_string()], 9876).unwrap();
    assert!(repo.join(".cursor/mcp.json").exists());

    // Removing the last managed server must delete the file AND the dir we made.
    render_scope(&client, &scope, &[], 9876).unwrap();
    assert!(
        !repo.join(".cursor/mcp.json").exists(),
        "scaffold file deleted"
    );
    assert!(!repo.join(".cursor").exists(), "ours-created dir pruned");
    assert!(repo.exists(), "repo dir itself untouched");
    std::fs::remove_dir_all(&repo).ok();
}

#[test]
fn render_scope_cleanup_keeps_file_holding_user_content() {
    let repo = fresh_tmp("repo-keepuser");
    std::fs::create_dir_all(repo.join(".cursor")).unwrap();
    let path = repo.join(".cursor/mcp.json");
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "someSetting": 1,
            "mcpServers": { "user-srv": { "command": "x" } }
        }))
        .unwrap(),
    )
    .unwrap();
    let client = resolved("Cursor");
    let scope = Scope::Repo(repo.clone());

    render_scope(&client, &scope, &["gmail".to_string()], 9876).unwrap();
    render_scope(&client, &scope, &[], 9876).unwrap();

    assert!(path.exists(), "file with user content must be kept");
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(v["someSetting"], 1);
    assert_eq!(v["mcpServers"]["user-srv"]["command"], "x");
    assert!(v["mcpServers"]["gmail"].is_null());
    std::fs::remove_dir_all(&repo).ok();
}

#[test]
fn render_scope_codex_toml_repo_write_and_cleanup() {
    let repo = fresh_tmp("repo-codex");
    std::fs::create_dir_all(&repo).unwrap();
    let client = resolved("Codex");
    let scope = Scope::Repo(repo.clone());

    render_scope(&client, &scope, &["gmail".to_string()], 9876).unwrap();
    let path = repo.join(".codex/config.toml");
    let v: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        v["mcp_servers"]["gmail"]["url"].as_str().unwrap(),
        "http://127.0.0.1:9876/key12345/gmail/mcp"
    );

    render_scope(&client, &scope, &[], 9876).unwrap();
    assert!(!path.exists(), "toml scaffold deleted on last removal");
    assert!(!repo.join(".codex").exists(), "dir pruned");
    std::fs::remove_dir_all(&repo).ok();
}

#[test]
fn codex_emits_toml_under_mcp_servers_table() {
    let dir = fresh_tmp("bi-codex");
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = dir.join("config.toml");
    render_servers(
        &schema_of("Codex"),
        &cfg,
        "cdx12345",
        &["gmail".to_string()],
        9876,
    )
    .unwrap();
    let text = std::fs::read_to_string(&cfg).unwrap();
    let v: toml::Value = toml::from_str(&text).unwrap();
    let url = v["mcp_servers"]["gmail"]["url"].as_str().unwrap();
    assert_eq!(url, "http://127.0.0.1:9876/cdx12345/gmail/mcp");
    std::fs::remove_dir_all(&dir).ok();
}

// ── CC-1: TOML writes are comment/format-preserving (toml_edit) ──────────────

/// Rendering into an existing Codex config that has user comments and extra keys
/// must preserve those comments and keys exactly — only the managed subtable changes.
#[test]
fn toml_render_preserves_user_comments_and_extra_keys() {
    let dir = fresh_tmp("toml-comments");
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = dir.join("config.toml");

    // Write a config with user comments and a user key outside our managed section.
    std::fs::write(
        &cfg,
        "# My Codex config\nsome_setting = true\n\n# MCP servers below\n",
    )
    .unwrap();

    render_servers(
        &schema_of("Codex"),
        &cfg,
        "cdx12345",
        &["gmail".to_string()],
        9876,
    )
    .unwrap();

    let text = std::fs::read_to_string(&cfg).unwrap();
    // User comments must survive.
    assert!(text.contains("# My Codex config"), "top comment preserved");
    assert!(
        text.contains("# MCP servers below"),
        "inline comment preserved"
    );
    // User key outside managed section must survive.
    assert!(text.contains("some_setting = true"), "user key preserved");
    // Our entry must be present.
    let v: toml::Value = toml::from_str(&text).unwrap();
    assert_eq!(
        v["mcp_servers"]["gmail"]["url"].as_str().unwrap(),
        "http://127.0.0.1:9876/cdx12345/gmail/mcp"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Purging a TOML config that has user comments outside our section must preserve
/// those comments.
#[test]
fn toml_purge_preserves_user_comments() {
    let dir = fresh_tmp("toml-purge-comments");
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = dir.join("config.toml");

    // First render our entry into a file with user comments.
    std::fs::write(&cfg, "# keep this comment\nsome_key = 42\n").unwrap();
    render_servers(
        &schema_of("Codex"),
        &cfg,
        "cdx12345",
        &["gmail".to_string()],
        9876,
    )
    .unwrap();

    purge_mcp(&schema_of("Codex"), &cfg, "cdx12345", 9876).unwrap();

    let text = std::fs::read_to_string(&cfg).unwrap();
    assert!(
        text.contains("# keep this comment"),
        "comment preserved after purge"
    );
    assert!(
        text.contains("some_key = 42"),
        "user key preserved after purge"
    );
    // Our entry must be gone.
    let v: toml::Value = toml::from_str(&text).unwrap();
    assert!(
        v.get("mcp_servers").and_then(|t| t.get("gmail")).is_none(),
        "managed entry removed"
    );
    std::fs::remove_dir_all(&dir).ok();
}

// ── CC-3: no-op write skip ────────────────────────────────────────────────────

/// When `render_scope` is called with an empty server list on a client whose
/// config file has never been touched, the file must NOT be created at all.
#[test]
fn render_scope_noop_does_not_create_file_when_nothing_to_write() {
    let repo = fresh_tmp("noop-repo");
    std::fs::create_dir_all(&repo).unwrap();
    let client = resolved("Cursor");
    let scope = Scope::Repo(repo.clone());

    render_scope(&client, &scope, &[], 9876).unwrap();

    // No file should be created since there are no entries and no existing file.
    assert!(
        !repo.join(".cursor/mcp.json").exists(),
        "no file created for empty noop"
    );
    std::fs::remove_dir_all(&repo).ok();
}

/// When `purge_mcp` is called and there are no matching entries, the file's
/// modification time must NOT change (or more precisely, the file bytes are
/// unchanged and we didn't rewrite it).
#[test]
fn purge_noop_does_not_rewrite_file() {
    let dir = fresh_tmp("purge-noop");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("mcp.json");
    let content = r#"{"mcpServers":{"user-server":{"command":"x"}}}"#;
    std::fs::write(&path, content).unwrap();
    let mtime_before = std::fs::metadata(&path).unwrap().modified().unwrap();

    // Purge with a different key — nothing to remove.
    purge_mcp(&cursor_schema(), &path, "different-key", 9876).unwrap();

    // File must be byte-identical and not touched.
    assert_eq!(std::fs::read(&path).unwrap(), content.as_bytes());
    let mtime_after = std::fs::metadata(&path).unwrap().modified().unwrap();
    assert_eq!(
        mtime_before, mtime_after,
        "file not rewritten on noop purge"
    );
    std::fs::remove_dir_all(&dir).ok();
}
