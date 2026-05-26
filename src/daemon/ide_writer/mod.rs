// IDE config writers: Claude Code, Cursor, VS Code Copilot, OpenCode, OpenClaude.
// Each per-IDE module supplies its OS-resolved config path; this module owns
// the shared deep-merge / atomic-write / purge logic over the uniform
// `mcpServers` shape. See spec §Add IDE flow > IDE config paths.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use crate::daemon::db::types::{IdeKey, ManagedIdeConfig};
pub use crate::daemon::ide_detect::IdeKind;

pub mod claude_code;
pub mod cursor;
pub mod openclaude;
pub mod opencode;
pub mod vscode_copilot;

pub mod error;
pub use error::IdeWriterError;

/// Resolve the IDE's config file path on the CURRENT operating system.
/// Returns None if we have no path mapping for this (ide, os) combination.
pub fn config_path_for(ide: IdeKind) -> Option<PathBuf> {
    match ide {
        IdeKind::ClaudeCode => claude_code::config_path(),
        IdeKind::Cursor => cursor::config_path(),
        IdeKind::VsCodeCopilot => vscode_copilot::config_path(),
        IdeKind::OpenCode => opencode::config_path(),
        IdeKind::OpenClaude => openclaude::config_path(),
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
    }
}

const fn ide_name(ide: IdeKind) -> &'static str {
    match ide {
        IdeKind::ClaudeCode => claude_code::IDE_NAME,
        IdeKind::Cursor => cursor::IDE_NAME,
        IdeKind::VsCodeCopilot => vscode_copilot::IDE_NAME,
        IdeKind::OpenCode => opencode::IDE_NAME,
        IdeKind::OpenClaude => openclaude::IDE_NAME,
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
) -> Result<ManagedIdeConfig, IdeWriterError> {
    let path = config_path_for(ide).ok_or(IdeWriterError::NoPathForOs)?;
    write_uniform_mcp_servers(&path, &ide_key.key, server_names, port)?;
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
pub fn purge(ide: IdeKind, ide_key: &str, port: u16) -> Result<(), IdeWriterError> {
    let path = config_path_for(ide).ok_or(IdeWriterError::NoPathForOs)?;
    purge_uniform_mcp_servers(&path, ide_key, port)
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

/// Merge our `mcpServers` entries into the JSON file at `path` and write
/// atomically. Used by `write`; exposed so tests can drive arbitrary paths.
pub fn write_uniform_mcp_servers(
    path: &Path,
    key: &str,
    server_names: &[String],
    port: u16,
) -> Result<(), IdeWriterError> {
    let mut root = read_existing_object(path)?.unwrap_or_default();

    let mut servers = match root.remove("mcpServers") {
        Some(Value::Object(m)) => m,
        // Non-object value under `mcpServers` is malformed by our standards;
        // replace it. Other top-level keys are preserved.
        Some(_) | None => Map::new(),
    };

    for name in server_names {
        let mut entry = Map::new();
        entry.insert("url".to_string(), Value::String(url_for(port, key, name)));
        servers.insert(name.clone(), Value::Object(entry));
    }

    root.insert("mcpServers".to_string(), Value::Object(servers));
    atomic_write_json(path, &Value::Object(root))
}

/// Drop one server's `mcpServers` entry by exact URL match.
///
/// Targets `http://127.0.0.1:<port>/<ide_key>/<server_name>/mcp`. Used by
/// §3.1 cascade delete and the per-server "Remove from this IDE" button.
/// Other entries (user-authored, other servers, other top-level keys) are
/// preserved.
pub fn purge_one_server_for_ide(
    path: &Path,
    port: u16,
    ide_key: &str,
    server_name: &str,
) -> Result<(), IdeWriterError> {
    let Some(mut root) = read_existing_object(path)? else {
        return Ok(());
    };
    let target = url_for(port, ide_key, server_name);
    if let Some(Value::Object(servers)) = root.get_mut("mcpServers") {
        servers.retain(|_name, entry| {
            entry.get("url").and_then(Value::as_str) != Some(target.as_str())
        });
    }
    atomic_write_json(path, &Value::Object(root))
}

/// Read the raw file contents (UTF-8 string) for the Editors-tab inline JSON
/// viewer. Returns `None` if the file doesn't exist yet.
pub fn read_raw(path: &Path) -> Result<Option<String>, IdeWriterError> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(IdeWriterError::Io(e)),
    }
}

/// Atomic write of the inline-editor textarea. Validates JSON first; refuses
/// to clobber the file on parse error.
pub fn write_raw(path: &Path, text: &str) -> Result<(), IdeWriterError> {
    let value: Value = serde_json::from_str(text)?;
    let Value::Object(_) = &value else {
        return Err(IdeWriterError::Json(serde::de::Error::custom(
            "expected top-level JSON object",
        )));
    };
    atomic_write_json(path, &value)
}

/// Returns Ok(()) if `text` parses as a valid top-level JSON object. Used by
/// the Editors-tab live-validate (without writing).
pub fn validate_json(text: &str) -> Result<(), IdeWriterError> {
    let value: Value = serde_json::from_str(text)?;
    let Value::Object(_) = &value else {
        return Err(IdeWriterError::Json(serde::de::Error::custom(
            "expected top-level JSON object",
        )));
    };
    Ok(())
}

/// Drop every `mcpServers` entry whose `url` matches our (port, key) shape.
/// Missing file is a no-op. Exposed so tests can drive arbitrary paths.
pub fn purge_uniform_mcp_servers(path: &Path, key: &str, port: u16) -> Result<(), IdeWriterError> {
    let Some(mut root) = read_existing_object(path)? else {
        return Ok(());
    };

    if let Some(Value::Object(servers)) = root.get_mut("mcpServers") {
        servers.retain(|_name, entry| {
            let Some(url) = entry.get("url").and_then(Value::as_str) else {
                return true;
            };
            !url_is_ours(url, port, key)
        });
    }

    atomic_write_json(path, &Value::Object(root))
}

fn read_existing_object(path: &Path) -> Result<Option<Map<String, Value>>, IdeWriterError> {
    match fs::read(path) {
        Ok(bytes) if bytes.iter().all(u8::is_ascii_whitespace) => Ok(None),
        Ok(bytes) => {
            let value: Value = serde_json::from_slice(&bytes)?;
            match value {
                Value::Object(m) => Ok(Some(m)),
                // Valid JSON but not an object — refuse rather than clobber.
                _ => Err(IdeWriterError::Json(serde::de::Error::custom(
                    "expected top-level JSON object",
                ))),
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(IdeWriterError::Io(e)),
    }
}

fn atomic_write_json(path: &Path, value: &Value) -> Result<(), IdeWriterError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    let mut serialized = serde_json::to_vec_pretty(value)?;
    serialized.push(b'\n');

    let tmp = tmp_path_for(path)?;
    fs::write(&tmp, &serialized)?;
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ignored = fs::remove_file(&tmp);
            Err(IdeWriterError::Io(e))
        }
    }
}

fn tmp_path_for(path: &Path) -> Result<PathBuf, IdeWriterError> {
    let file_name = path
        .file_name()
        .ok_or(IdeWriterError::NonUtf8Path)?
        .to_str()
        .ok_or(IdeWriterError::NonUtf8Path)?;
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
