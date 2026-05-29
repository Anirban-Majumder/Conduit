//! IDE config writers and the shared merge/write/purge logic.
//!
//! Each per-IDE submodule supplies its OS-resolved config path and schema
//! (top-level key plus an optional `type` discriminator); this module owns the
//! shared deep-merge, atomic-write, and purge routines. `write`/`purge` dispatch
//! on `JsonSchema` (five IDEs) versus the TOML path (Codex), identifying our
//! entries by exact URL shape (`http://127.0.0.1:<port>/<key>/<server>/mcp`) so
//! user-authored entries are always preserved. Also resolves per-IDE skills
//! directories for the sync layer.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use crate::daemon::db::types::{IdeKey, ManagedIdeConfig};
pub use crate::daemon::ide_detect::IdeKind;

/// Per-IDE schema for the JSON-flavored MCP config (5 of 6 IDEs).
/// Codex is TOML and goes through `write_toml_mcp_servers` instead.
#[derive(Debug, Clone, Copy)]
pub struct JsonSchema {
    /// Top-level key holding the server map (e.g. `mcpServers`, `servers`, `mcp`).
    pub top_level_key: &'static str,
    /// Required `type` discriminator on each entry, or None if the IDE
    /// auto-negotiates transport from the URL.
    pub server_type: Option<&'static str>,
}

#[expect(
    clippy::match_same_arms,
    reason = "per-IDE arms intentional: each schema was verified independently against that project's docs/source. Coincidental equality today (Claude Code + OpenClaude) shouldn't mask future drift."
)]
const fn json_schema_for(ide: IdeKind) -> Option<JsonSchema> {
    match ide {
        IdeKind::ClaudeCode => Some(JsonSchema {
            top_level_key: "mcpServers",
            server_type: Some("http"),
        }),
        IdeKind::Cursor => Some(JsonSchema {
            top_level_key: "mcpServers",
            server_type: None,
        }),
        IdeKind::VsCodeCopilot => Some(JsonSchema {
            top_level_key: "servers",
            server_type: Some("http"),
        }),
        IdeKind::OpenCode => Some(JsonSchema {
            top_level_key: "mcp",
            server_type: Some("remote"),
        }),
        IdeKind::OpenClaude => Some(JsonSchema {
            top_level_key: "mcpServers",
            server_type: Some("http"),
        }),
        IdeKind::Codex => None,
    }
}

pub mod claude_code;
pub mod codex;
pub mod cursor;
pub mod openclaude;
pub mod opencode;
pub mod vscode_copilot;

// Template for adding a new client. Gated with `#![cfg(any())]` so it never
// compiles — copy this file to `<name>.rs` and wire it into the dispatchers.
pub mod example;

pub mod error;
pub use error::ClientError;

/// Resolve the IDE's config file path on the CURRENT operating system.
/// Returns None if we have no path mapping for this (ide, os) combination.
pub fn config_path_for(ide: IdeKind) -> Option<PathBuf> {
    match ide {
        IdeKind::ClaudeCode => claude_code::config_path(),
        IdeKind::Cursor => cursor::config_path(),
        IdeKind::VsCodeCopilot => vscode_copilot::config_path(),
        IdeKind::OpenCode => opencode::config_path(),
        IdeKind::OpenClaude => openclaude::config_path(),
        IdeKind::Codex => codex::config_path(),
    }
}

/// Test-only resolver: returns the path each IDE would write under `home`,
/// using the Unix-style layout from the spec. Useful for driving tests with
/// a fake home directory.
pub fn config_path_under(ide: IdeKind, home: &Path) -> PathBuf {
    match ide {
        IdeKind::ClaudeCode => claude_code::config_path_under(home),
        IdeKind::Cursor => cursor::config_path_under(home),
        IdeKind::VsCodeCopilot => vscode_copilot::config_path_under(home),
        IdeKind::OpenCode => opencode::config_path_under(home),
        IdeKind::OpenClaude => openclaude::config_path_under(home),
        IdeKind::Codex => codex::config_path_under(home),
    }
}

/// IDE targets that natively load `<dir>/skills/*` at startup. VS Code Copilot
/// has no global skills directory (prompt files are workspace-scoped only).
pub const SKILL_TARGET_IDES: &[IdeKind] = &[
    IdeKind::ClaudeCode,
    IdeKind::Cursor,
    IdeKind::OpenClaude,
    IdeKind::OpenCode,
    IdeKind::Codex,
];

pub fn skills_dir_for(ide: IdeKind) -> Option<PathBuf> {
    match ide {
        IdeKind::ClaudeCode => claude_code::skills_dir(),
        IdeKind::Cursor => cursor::skills_dir(),
        IdeKind::OpenClaude => openclaude::skills_dir(),
        IdeKind::OpenCode => opencode::skills_dir(),
        IdeKind::Codex => codex::skills_dir(),
        IdeKind::VsCodeCopilot => None,
    }
}

pub fn skills_dir_under(ide: IdeKind, home: &Path) -> Option<PathBuf> {
    match ide {
        IdeKind::ClaudeCode => Some(claude_code::skills_dir_under(home)),
        IdeKind::Cursor => Some(cursor::skills_dir_under(home)),
        IdeKind::OpenClaude => Some(openclaude::skills_dir_under(home)),
        IdeKind::OpenCode => Some(opencode::skills_dir_under(home)),
        IdeKind::Codex => Some(codex::skills_dir_under(home)),
        IdeKind::VsCodeCopilot => None,
    }
}

pub const fn ide_kind_from_db_name(s: &str) -> Option<IdeKind> {
    match s.as_bytes() {
        b"ClaudeCode" => Some(IdeKind::ClaudeCode),
        b"OpenClaude" => Some(IdeKind::OpenClaude),
        b"OpenCode" => Some(IdeKind::OpenCode),
        b"Cursor" => Some(IdeKind::Cursor),
        b"VsCodeCopilot" => Some(IdeKind::VsCodeCopilot),
        b"Codex" => Some(IdeKind::Codex),
        _ => None,
    }
}

pub const fn ide_db_name(ide: IdeKind) -> &'static str {
    ide_name(ide)
}

const fn ide_name(ide: IdeKind) -> &'static str {
    match ide {
        IdeKind::ClaudeCode => claude_code::IDE_NAME,
        IdeKind::Cursor => cursor::IDE_NAME,
        IdeKind::VsCodeCopilot => vscode_copilot::IDE_NAME,
        IdeKind::OpenCode => opencode::IDE_NAME,
        IdeKind::OpenClaude => openclaude::IDE_NAME,
        IdeKind::Codex => codex::IDE_NAME,
    }
}

/// Inject our entries into the IDE's MCP config.
///
/// Returns a `ManagedIdeConfig` describing what was written. Caller persists
/// via `Db::record_ide_config_write`. Atomic-write: tempfile in same dir +
/// rename. Preserves user's other entries.
pub fn write(
    ide: IdeKind,
    ide_key: &IdeKey,
    server_names: &[String],
    port: u16,
) -> Result<ManagedIdeConfig, ClientError> {
    let path = config_path_for(ide).ok_or(ClientError::NoPathForOs)?;
    if let Some(schema) = json_schema_for(ide) {
        write_uniform_mcp_servers(&path, &schema, &ide_key.key, server_names, port)?;
    } else {
        write_toml_mcp_servers(&path, &ide_key.key, server_names, port)?;
    }
    Ok(ManagedIdeConfig {
        ide_name: ide_name(ide).to_string(),
        ide_key: ide_key.key.clone(),
        config_path: path,
        managed_entries: server_names.to_vec(),
        written_at: now_unix(),
    })
}

/// Remove our entries from the IDE's MCP config.
///
/// Identifies our entries by URL match: `http://127.0.0.1:<port>/<ide_key>/<server>/mcp`.
/// Preserves every other entry the user added. Idempotent (missing file is Ok).
pub fn purge(ide: IdeKind, ide_key: &str, port: u16) -> Result<(), ClientError> {
    let path = config_path_for(ide).ok_or(ClientError::NoPathForOs)?;
    let Some(schema) = json_schema_for(ide) else {
        return purge_toml_mcp_servers(&path, ide_key, port);
    };
    purge_uniform_mcp_servers(&path, &schema, ide_key, port)
}

fn url_for(port: u16, key: &str, server: &str) -> String {
    format!("http://127.0.0.1:{port}/{key}/{server}/mcp")
}

fn url_is_ours(url: &str, port: u16, key: &str) -> bool {
    let prefix = format!("http://127.0.0.1:{port}/{key}/");
    let Some(rest) = url.strip_prefix(&prefix) else {
        return false;
    };
    let Some(server_part) = rest.strip_suffix("/mcp") else {
        return false;
    };
    !server_part.is_empty() && !server_part.contains('/')
}

/// Merge our server entries into the JSON file at `path` (under
/// `schema.top_level_key`) and write atomically. Used by `write`; exposed so
/// tests can drive arbitrary paths.
pub fn write_uniform_mcp_servers(
    path: &Path,
    schema: &JsonSchema,
    key: &str,
    server_names: &[String],
    port: u16,
) -> Result<(), ClientError> {
    let mut root = read_existing_object(path)?.unwrap_or_default();

    let mut servers = match root.remove(schema.top_level_key) {
        Some(Value::Object(m)) => m,
        // Non-object value under the top-level key is malformed by our
        // standards; replace it. Other top-level keys are preserved.
        Some(_) | None => Map::new(),
    };

    for name in server_names {
        let mut entry = Map::new();
        if let Some(ty) = schema.server_type {
            entry.insert("type".to_string(), Value::String(ty.to_string()));
        }
        entry.insert("url".to_string(), Value::String(url_for(port, key, name)));
        servers.insert(name.clone(), Value::Object(entry));
    }

    root.insert(schema.top_level_key.to_string(), Value::Object(servers));
    atomic_write_json(path, &Value::Object(root))
}

/// Drop one server's entry from the IDE's MCP config by exact URL match.
///
/// Targets `http://127.0.0.1:<port>/<ide_key>/<server_name>/mcp`. Used by
/// §3.1 cascade delete and the per-server "Remove from this IDE" button.
/// Other entries (user-authored, other servers, other top-level keys) are
/// preserved. `ide` selects the per-IDE schema (top-level key + format).
pub fn purge_one_server_for_ide(
    ide: IdeKind,
    path: &Path,
    port: u16,
    ide_key: &str,
    server_name: &str,
) -> Result<(), ClientError> {
    let Some(schema) = json_schema_for(ide) else {
        return purge_one_server_for_ide_toml(path, port, ide_key, server_name);
    };
    let Some(mut root) = read_existing_object(path)? else {
        return Ok(());
    };
    let target = url_for(port, ide_key, server_name);
    if let Some(Value::Object(servers)) = root.get_mut(schema.top_level_key) {
        servers.retain(|_name, entry| {
            entry.get("url").and_then(Value::as_str) != Some(target.as_str())
        });
    }
    atomic_write_json(path, &Value::Object(root))
}

/// Read the raw file contents (UTF-8 string) for the Editors-tab inline JSON
/// viewer. Returns `None` if the file doesn't exist yet.
pub fn read_raw(path: &Path) -> Result<Option<String>, ClientError> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ClientError::Io(e)),
    }
}

/// Atomic write of the inline-editor textarea. Validates JSON first; refuses
/// to clobber the file on parse error.
pub fn write_raw(path: &Path, text: &str) -> Result<(), ClientError> {
    let value: Value = serde_json::from_str(text)?;
    let Value::Object(_) = &value else {
        return Err(ClientError::Json(serde::de::Error::custom(
            "expected top-level JSON object",
        )));
    };
    atomic_write_json(path, &value)
}

/// Returns Ok(()) if `text` parses as a valid top-level JSON object. Used by
/// the Editors-tab live-validate (without writing).
pub fn validate_json(text: &str) -> Result<(), ClientError> {
    let value: Value = serde_json::from_str(text)?;
    let Value::Object(_) = &value else {
        return Err(ClientError::Json(serde::de::Error::custom(
            "expected top-level JSON object",
        )));
    };
    Ok(())
}

/// Drop every entry under `schema.top_level_key` whose `url` matches our
/// (port, key) shape. Missing file is a no-op. Exposed so tests can drive
/// arbitrary paths.
pub fn purge_uniform_mcp_servers(
    path: &Path,
    schema: &JsonSchema,
    key: &str,
    port: u16,
) -> Result<(), ClientError> {
    let Some(mut root) = read_existing_object(path)? else {
        return Ok(());
    };

    if let Some(Value::Object(servers)) = root.get_mut(schema.top_level_key) {
        servers.retain(|_name, entry| {
            let Some(url) = entry.get("url").and_then(Value::as_str) else {
                return true;
            };
            !url_is_ours(url, port, key)
        });
    }

    atomic_write_json(path, &Value::Object(root))
}

fn read_existing_object(path: &Path) -> Result<Option<Map<String, Value>>, ClientError> {
    match fs::read(path) {
        Ok(bytes) if bytes.iter().all(u8::is_ascii_whitespace) => Ok(None),
        Ok(bytes) => {
            let value: Value = serde_json::from_slice(&bytes)?;
            match value {
                Value::Object(m) => Ok(Some(m)),
                // Valid JSON but not an object — refuse rather than clobber.
                _ => Err(ClientError::Json(serde::de::Error::custom(
                    "expected top-level JSON object",
                ))),
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ClientError::Io(e)),
    }
}

fn atomic_write_json(path: &Path, value: &Value) -> Result<(), ClientError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    let mut serialized = serde_json::to_vec_pretty(value)?;
    serialized.push(b'\n');

    let tmp = tmp_path_for(path)?;
    fs::write(&tmp, &serialized)?;
    atomic_rename_or_replace(&tmp, path).map_err(|e| {
        let _ignored = fs::remove_file(&tmp);
        ClientError::Io(e)
    })
}

/// TOML-flavored sibling of `write_uniform_mcp_servers` for Codex
/// (`~/.codex/config.toml`). Top-level key is `mcp_servers` (`snake_case`),
/// per-server table holds `url` for the `StreamableHttp` transport.
pub fn write_toml_mcp_servers(
    path: &Path,
    key: &str,
    server_names: &[String],
    port: u16,
) -> Result<(), ClientError> {
    let mut root = read_existing_toml_table(path)?.unwrap_or_default();

    let mut servers = match root.remove("mcp_servers") {
        Some(toml::Value::Table(t)) => t,
        Some(_) | None => toml::value::Table::new(),
    };

    for name in server_names {
        let mut entry = toml::value::Table::new();
        entry.insert(
            "url".to_string(),
            toml::Value::String(url_for(port, key, name)),
        );
        servers.insert(name.clone(), toml::Value::Table(entry));
    }

    root.insert("mcp_servers".to_string(), toml::Value::Table(servers));
    atomic_write_toml(path, &toml::Value::Table(root))
}

/// TOML-flavored sibling of `purge_uniform_mcp_servers`.
pub fn purge_toml_mcp_servers(path: &Path, key: &str, port: u16) -> Result<(), ClientError> {
    let Some(mut root) = read_existing_toml_table(path)? else {
        return Ok(());
    };
    if let Some(toml::Value::Table(servers)) = root.get_mut("mcp_servers") {
        servers.retain(|_name, entry| {
            let Some(url) = entry.get("url").and_then(toml::Value::as_str) else {
                return true;
            };
            !url_is_ours(url, port, key)
        });
    }
    atomic_write_toml(path, &toml::Value::Table(root))
}

fn purge_one_server_for_ide_toml(
    path: &Path,
    port: u16,
    ide_key: &str,
    server_name: &str,
) -> Result<(), ClientError> {
    let Some(mut root) = read_existing_toml_table(path)? else {
        return Ok(());
    };
    let target = url_for(port, ide_key, server_name);
    if let Some(toml::Value::Table(servers)) = root.get_mut("mcp_servers") {
        servers.retain(|_name, entry| {
            entry.get("url").and_then(toml::Value::as_str) != Some(target.as_str())
        });
    }
    atomic_write_toml(path, &toml::Value::Table(root))
}

fn read_existing_toml_table(path: &Path) -> Result<Option<toml::value::Table>, ClientError> {
    match fs::read_to_string(path) {
        Ok(s) if s.chars().all(char::is_whitespace) => Ok(None),
        Ok(s) => {
            let value: toml::Value = toml::from_str(&s)?;
            match value {
                toml::Value::Table(t) => Ok(Some(t)),
                _ => Err(ClientError::TomlDe(serde::de::Error::custom(
                    "expected top-level TOML table",
                ))),
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ClientError::Io(e)),
    }
}

fn atomic_write_toml(path: &Path, value: &toml::Value) -> Result<(), ClientError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    let mut serialized = toml::to_string_pretty(value)?.into_bytes();
    if !serialized.ends_with(b"\n") {
        serialized.push(b'\n');
    }

    let tmp = tmp_path_for(path)?;
    fs::write(&tmp, &serialized)?;
    atomic_rename_or_replace(&tmp, path).map_err(|e| {
        let _ignored = fs::remove_file(&tmp);
        ClientError::Io(e)
    })
}

#[cfg(windows)]
fn atomic_rename_or_replace(tmp: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;

    const ERROR_ACCESS_DENIED: i32 = 5;
    const ERROR_SHARING_VIOLATION: i32 = 32;
    const REPLACEFILE_IGNORE_ACL_ERRORS: u32 = 0x0000_0004;

    match fs::rename(tmp, target) {
        Ok(()) => return Ok(()),
        Err(e) => {
            let raw = e.raw_os_error();
            if raw != Some(ERROR_ACCESS_DENIED) && raw != Some(ERROR_SHARING_VIOLATION) {
                return Err(e);
            }
        }
    }

    let replaced: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    let replacement: Vec<u16> = tmp.as_os_str().encode_wide().chain(Some(0)).collect();

    tracing::debug!(
        ?target,
        "rename failed (likely IDE lock), falling back to ReplaceFileW"
    );

    #[expect(
        unsafe_code,
        reason = "ReplaceFileW FFI — documented Windows API for atomic file replacement"
    )]
    let ok = unsafe {
        ReplaceFileW(
            replaced.as_ptr(),
            replacement.as_ptr(),
            std::ptr::null(),
            REPLACEFILE_IGNORE_ACL_ERRORS,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };

    if ok != 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(windows))]
fn atomic_rename_or_replace(tmp: &Path, target: &Path) -> std::io::Result<()> {
    fs::rename(tmp, target)
}

fn tmp_path_for(path: &Path) -> Result<PathBuf, ClientError> {
    let file_name = path
        .file_name()
        .ok_or(ClientError::NonUtf8Path)?
        .to_str()
        .ok_or(ClientError::NonUtf8Path)?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let pid = std::process::id();
    let nonce = now_nanos();
    Ok(parent.join(format!(".{file_name}.{pid}.{nonce}.tmp")))
}

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn now_unix() -> i64 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    i64::try_from(secs).unwrap_or(i64::MAX)
}
