//! JSON/TOML merge, read, and purge helpers for IDE MCP config files.
//!
//! Each function reads an existing config (JSON or TOML), splices our managed
//! entries in or out, then delegates to `io::atomic_write_*` for an atomic
//! write. `read_existing_object` / `read_existing_toml_table` attach the file
//! path to parse errors so callers get actionable log messages.

use std::fs;
use std::path::Path;

use serde_json::{Map, Value};

use super::io::{atomic_write_json, atomic_write_toml};
use super::{ClientError, JsonSchema};

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

/// Drop one server's entry from the JSON config by exact URL match.
pub(super) fn purge_one_server_for_ide_json(
    path: &Path,
    schema: &JsonSchema,
    port: u16,
    ide_key: &str,
    server_name: &str,
) -> Result<(), ClientError> {
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

/// TOML-flavored sibling of `write_uniform_mcp_servers` for Codex
/// (`~/.codex/config.toml`). Top-level key is `mcp_servers` (`snake_case`),
/// per-server table holds `url` for the `StreamableHttp` transport.
pub(super) fn write_toml_mcp_servers(
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
pub(super) fn purge_toml_mcp_servers(
    path: &Path,
    key: &str,
    port: u16,
) -> Result<(), ClientError> {
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

/// Drop one server's entry from the TOML config by exact URL match.
pub(super) fn purge_one_server_for_ide_toml(
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

fn read_existing_object(path: &Path) -> Result<Option<Map<String, Value>>, ClientError> {
    match fs::read(path) {
        Ok(bytes) if bytes.iter().all(u8::is_ascii_whitespace) => Ok(None),
        Ok(bytes) => {
            let value: Value =
                serde_json::from_slice(&bytes).map_err(|source| ClientError::ParseJson {
                    path: path.to_owned(),
                    source,
                })?;
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

fn read_existing_toml_table(path: &Path) -> Result<Option<toml::value::Table>, ClientError> {
    match fs::read_to_string(path) {
        Ok(s) if s.chars().all(char::is_whitespace) => Ok(None),
        Ok(s) => {
            let value: toml::Value =
                toml::from_str(&s).map_err(|source| ClientError::ParseToml {
                    path: path.to_owned(),
                    source,
                })?;
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
