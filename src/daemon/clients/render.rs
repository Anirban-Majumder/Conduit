//! The one MCP-config renderer — format-switched on `ConfigSchema`, IDE-agnostic.
//!
//! Replaces the old per-format `write_uniform_mcp_servers` / `write_toml_mcp_servers`
//! and their purge siblings. Reads the existing config (JSON, JSONC, or TOML),
//! splices our managed server entries in/out under `schema.key_path`, and writes
//! atomically via `io`. Four invariants the real editor schemas force:
//!   1. token substitution is recursive over the whole `entry_template` tree;
//!   2. JSONC reads tolerate comments + trailing commas;
//!   3. `key_path` is walked-and-created, merging beside the user's siblings;
//!   4. purge matches the endpoint anywhere in an entry, not a fixed `url` field.

use std::fs;
use std::path::Path;

use serde_json::{Map, Value};

use super::io::{atomic_write_json, atomic_write_toml};
use super::resolved::ResolvedClient;
use super::schema::{ConfigFormat, ConfigSchema};
use super::ClientError;

fn url_for(port: u16, key: &str, server: &str) -> String {
    format!("http://127.0.0.1:{port}/{key}/{server}/mcp")
}

/// Does `url` point at our daemon for this (port, key)? Matches any server
/// segment so purge sweeps every entry we ever wrote under this key.
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

/// Render the entry template for one server, substituting the fixed token vocab
/// recursively into every string at any depth (rule 1). A token may live inside
/// a nested array — e.g. `["mcp-remote", "<url-token>"]`.
fn render_entry(template: &Value, port: u16, key: &str, server: &str) -> Value {
    let url = url_for(port, key, server);
    let port_s = port.to_string();
    substitute(template, &url, &port_s, key, server)
}

#[expect(
    clippy::literal_string_with_formatting_args,
    reason = "${url} etc. are our placeholder tokens, not Rust format args"
)]
fn substitute(v: &Value, url: &str, port: &str, key: &str, name: &str) -> Value {
    match v {
        Value::String(s) => Value::String(
            s.replace("${url}", url)
                .replace("${port}", port)
                .replace("${key}", key)
                .replace("${name}", name),
        ),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, val)| (k.clone(), substitute(val, url, port, key, name)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(
            a.iter()
                .map(|val| substitute(val, url, port, key, name))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Does this entry (rendered or user-authored) reference our endpoint anywhere
/// in its serialized form (rule 4)? Walks every nested string.
fn entry_is_ours(entry: &Value, port: u16, key: &str) -> bool {
    any_string(entry, &|s| url_is_ours(s, port, key))
}

fn entry_targets_server(entry: &Value, port: u16, key: &str, server: &str) -> bool {
    let target = url_for(port, key, server);
    any_string(entry, &|s| s == target)
}

fn any_string(v: &Value, pred: &dyn Fn(&str) -> bool) -> bool {
    match v {
        Value::String(s) => pred(s),
        Value::Object(m) => m.values().any(|val| any_string(val, pred)),
        Value::Array(a) => a.iter().any(|val| any_string(val, pred)),
        _ => false,
    }
}

// ── Public API ─────────────────────────────────────────────────────────────

/// Make the client's config file hold *exactly* our `servers` set.
///
/// Purges every entry of ours, then renders the given set, preserving the
/// user's own entries. This is what callers want after a managed-set change
/// (toggle, cascade) — `render_servers` alone merges and would leave removed
/// servers behind.
pub fn render_mcp(
    client: &ResolvedClient,
    servers: &[String],
    port: u16,
) -> Result<(), ClientError> {
    purge_mcp(&client.schema, &client.config_path, &client.mcp_key, port)?;
    render_servers(
        &client.schema,
        &client.config_path,
        &client.mcp_key,
        servers,
        port,
    )
}

/// Core writer: merge `servers` into the file under `key_path`, preserving
/// existing entries. Also driven directly by tests with an arbitrary path.
pub fn render_servers(
    schema: &ConfigSchema,
    path: &Path,
    key: &str,
    servers: &[String],
    port: u16,
) -> Result<(), ClientError> {
    // An empty key_path would insert our entries at the file root, mangling the
    // user's real editor config. A built-in's schema is a code-owned cache that
    // should never be empty; if it is (corrupt row / unparsable schema_json that
    // defaulted), fail closed rather than corrupt the file.
    if schema.key_path.is_empty() {
        return Err(ClientError::EmptyKeyPath);
    }

    let entries: Vec<(String, Value)> = servers
        .iter()
        .map(|s| {
            (
                s.clone(),
                render_entry(&schema.entry_template, port, key, s),
            )
        })
        .collect();

    match schema.format {
        ConfigFormat::Toml => render_toml(path, &schema.key_path, &entries),
        ConfigFormat::Json | ConfigFormat::Jsonc => {
            render_json(path, schema.format, &schema.key_path, &entries)
        }
    }
}

/// Drop every entry under the schema's `key_path` whose endpoint is ours.
/// Missing file is a no-op.
pub fn purge_mcp(
    schema: &ConfigSchema,
    path: &Path,
    key: &str,
    port: u16,
) -> Result<(), ClientError> {
    purge_where(schema, path, &|entry| entry_is_ours(entry, port, key))
}

/// Drop one server's entry under the schema's `key_path`. Missing file is a no-op.
pub fn purge_one(
    schema: &ConfigSchema,
    path: &Path,
    key: &str,
    server: &str,
    port: u16,
) -> Result<(), ClientError> {
    purge_where(schema, path, &|entry| {
        entry_targets_server(entry, port, key, server)
    })
}

// ── JSON / JSONC ────────────────────────────────────────────────────────────

fn render_json(
    path: &Path,
    format: ConfigFormat,
    key_path: &[String],
    entries: &[(String, Value)],
) -> Result<(), ClientError> {
    let mut root = read_json(path, format)?.unwrap_or_default();
    {
        let map = descend_create(&mut root, key_path);
        for (name, entry) in entries {
            map.insert(name.clone(), entry.clone());
        }
    }
    atomic_write_json(path, &Value::Object(root))
}

fn purge_where(
    schema: &ConfigSchema,
    path: &Path,
    is_target: &dyn Fn(&Value) -> bool,
) -> Result<(), ClientError> {
    match schema.format {
        ConfigFormat::Toml => {
            let Some(mut root) = read_toml(path)? else {
                return Ok(());
            };
            if let Some(map) = descend_existing_toml(&mut root, &schema.key_path) {
                map.retain(|_n, e| !is_target(&toml_to_json(e)));
            }
            atomic_write_toml(path, &toml::Value::Table(root))
        }
        ConfigFormat::Json | ConfigFormat::Jsonc => {
            let Some(mut root) = read_json(path, schema.format)? else {
                return Ok(());
            };
            if let Some(map) = descend_existing_json(&mut root, &schema.key_path) {
                map.retain(|_n, e| !is_target(e));
            }
            atomic_write_json(path, &Value::Object(root))
        }
    }
}

/// Walk `key_path`, creating intermediate objects, and return the server map at
/// the leaf (rule 3). Empty `key_path` writes at the root.
#[expect(
    clippy::expect_used,
    reason = "the line above guarantees the slot is an object; a conditional \
              reborrow in this loop hits an NLL limitation, so expect is the \
              clean infallible descent"
)]
fn descend_create<'a>(
    root: &'a mut Map<String, Value>,
    key_path: &[String],
) -> &'a mut Map<String, Value> {
    let mut cur = root;
    for seg in key_path {
        let slot = cur
            .entry(seg.clone())
            .or_insert_with(|| Value::Object(Map::new()));
        // A non-object value where we need to descend is malformed by our
        // standards; replace it rather than clobber siblings elsewhere.
        if !slot.is_object() {
            *slot = Value::Object(Map::new());
        }
        cur = slot
            .as_object_mut()
            .expect("slot ensured to be an object above");
    }
    cur
}

fn descend_existing_json<'a>(
    root: &'a mut Map<String, Value>,
    key_path: &[String],
) -> Option<&'a mut Map<String, Value>> {
    let mut cur = root;
    for seg in key_path {
        cur = cur.get_mut(seg)?.as_object_mut()?;
    }
    Some(cur)
}

fn read_json(path: &Path, format: ConfigFormat) -> Result<Option<Map<String, Value>>, ClientError> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(ClientError::Io(e)),
    };
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&bytes);
    let cleaned = if format == ConfigFormat::Jsonc {
        strip_jsonc(&text)
    } else {
        text.into_owned()
    };
    let value: Value = serde_json::from_str(&cleaned).map_err(|source| ClientError::ParseJson {
        path: path.to_owned(),
        source,
    })?;
    match value {
        Value::Object(m) => Ok(Some(m)),
        _ => Err(ClientError::Json(serde::de::Error::custom(
            "expected top-level JSON object",
        ))),
    }
}

/// Strip `//` and `/* */` comments and trailing commas from a JSONC document,
/// leaving string literals untouched (rule 2). Comment-tolerant read path for
/// editors (VS Code, Gemini, Zed) whose config carries comments.
fn strip_jsonc(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = '\0';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            _ => out.push(c),
        }
    }
    strip_trailing_commas(&out)
}

/// Remove commas that immediately precede `}` or `]` (ignoring whitespace),
/// outside string literals.
fn strip_trailing_commas(src: &str) -> String {
    let bytes: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut in_string = false;
    let mut escaped = false;
    for (i, &c) in bytes.iter().enumerate() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        if c == '"' {
            in_string = true;
            out.push(c);
            continue;
        }
        if c == ',' {
            let next = bytes
                .get(i + 1..)
                .and_then(|rest| rest.iter().find(|n| !n.is_whitespace()));
            if matches!(next, Some('}' | ']')) {
                continue; // drop the trailing comma
            }
        }
        out.push(c);
    }
    out
}

// ── TOML ─────────────────────────────────────────────────────────────────────

fn render_toml(
    path: &Path,
    key_path: &[String],
    entries: &[(String, Value)],
) -> Result<(), ClientError> {
    let mut root = read_toml(path)?.unwrap_or_default();
    {
        let map = descend_create_toml(&mut root, key_path);
        for (name, entry) in entries {
            map.insert(name.clone(), json_to_toml(entry));
        }
    }
    atomic_write_toml(path, &toml::Value::Table(root))
}

#[expect(
    clippy::expect_used,
    reason = "the line above guarantees the slot is a table; a conditional \
              reborrow in this loop hits an NLL limitation, so expect is the \
              clean infallible descent"
)]
fn descend_create_toml<'a>(
    root: &'a mut toml::value::Table,
    key_path: &[String],
) -> &'a mut toml::value::Table {
    let mut cur = root;
    for seg in key_path {
        let slot = cur
            .entry(seg.clone())
            .or_insert_with(|| toml::Value::Table(toml::value::Table::new()));
        if !slot.is_table() {
            *slot = toml::Value::Table(toml::value::Table::new());
        }
        cur = slot
            .as_table_mut()
            .expect("slot ensured to be a table above");
    }
    cur
}

fn descend_existing_toml<'a>(
    root: &'a mut toml::value::Table,
    key_path: &[String],
) -> Option<&'a mut toml::value::Table> {
    let mut cur = root;
    for seg in key_path {
        cur = cur.get_mut(seg)?.as_table_mut()?;
    }
    Some(cur)
}

fn read_toml(path: &Path) -> Result<Option<toml::value::Table>, ClientError> {
    let text = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(ClientError::Io(e)),
    };
    if text.chars().all(char::is_whitespace) {
        return Ok(None);
    }
    let value: toml::Value = toml::from_str(&text).map_err(|source| ClientError::ParseToml {
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

fn json_to_toml(v: &Value) -> toml::Value {
    match v {
        Value::String(s) => toml::Value::String(s.clone()),
        Value::Bool(b) => toml::Value::Boolean(*b),
        Value::Number(n) => n.as_i64().map_or_else(
            || toml::Value::Float(n.as_f64().unwrap_or(0.0)),
            toml::Value::Integer,
        ),
        Value::Array(a) => toml::Value::Array(a.iter().map(json_to_toml).collect()),
        Value::Object(m) => toml::Value::Table(
            m.iter()
                .map(|(k, val)| (k.clone(), json_to_toml(val)))
                .collect(),
        ),
        Value::Null => toml::Value::String(String::new()),
    }
}

fn toml_to_json(v: &toml::Value) -> Value {
    match v {
        toml::Value::String(s) => Value::String(s.clone()),
        toml::Value::Boolean(b) => Value::Bool(*b),
        toml::Value::Integer(i) => Value::Number((*i).into()),
        toml::Value::Float(f) => {
            serde_json::Number::from_f64(*f).map_or(Value::Null, Value::Number)
        }
        toml::Value::Datetime(d) => Value::String(d.to_string()),
        toml::Value::Array(a) => Value::Array(a.iter().map(toml_to_json).collect()),
        toml::Value::Table(t) => Value::Object(
            t.iter()
                .map(|(k, val)| (k.clone(), toml_to_json(val)))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        unused_qualifications,
        reason = "test ergonomics"
    )]

    use super::*;
    use serde_json::json;

    fn schema(
        format: ConfigFormat,
        key_path: &[&str],
        template: serde_json::Value,
    ) -> ConfigSchema {
        ConfigSchema {
            format,
            key_path: key_path.iter().map(|s| (*s).to_string()).collect(),
            entry_template: template,
        }
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        use crate::daemon::time_util::now_unix_nanos;
        std::env::temp_dir().join(format!("conduit-render-test-{name}-{}", now_unix_nanos()))
    }

    // Guard: an empty key_path (corrupt/defaulted schema) must NOT write our
    // entries at the file root — it would mangle the user's real editor config.
    #[test]
    fn render_refuses_empty_key_path_and_leaves_file_untouched() {
        let dir = tmp("empty-keypath");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"userSetting":"keep"}"#).unwrap();

        let s = schema(ConfigFormat::Json, &[], json!({"url": "${url}"}));
        let err = render_servers(&s, &path, "k0000000", &["gmail".to_string()], 9876);
        assert!(matches!(err, Err(ClientError::EmptyKeyPath)));

        // File is byte-for-byte untouched (no root-level junk written).
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            r#"{"userSetting":"keep"}"#
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    // Rule 1: recursive substitution of the full token vocab at any depth,
    // including inside a nested array (the mcp-remote / Zed shape).
    #[test]
    fn substitutes_all_tokens_recursively_including_nested_arrays() {
        let dir = tmp("tokens");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("zed.json");
        let s = schema(
            ConfigFormat::Json,
            &["context_servers"],
            json!({
                "source": "custom",
                "command": "npx",
                "args": ["mcp-remote", "${url}"],
                "meta": { "label": "${name}@${port}", "tok": "${key}" }
            }),
        );

        render_servers(&s, &path, "abc12345", &["gmail".to_string()], 9876).unwrap();

        let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let entry = &v["context_servers"]["gmail"];
        assert_eq!(entry["args"][1], "http://127.0.0.1:9876/abc12345/gmail/mcp");
        assert_eq!(entry["meta"]["label"], "gmail@9876");
        assert_eq!(entry["meta"]["tok"], "abc12345");
        std::fs::remove_dir_all(&dir).ok();
    }

    // Rule 2: JSONC reads tolerate // and /* */ comments and trailing commas.
    #[test]
    fn jsonc_read_tolerates_comments_and_trailing_commas() {
        let dir = tmp("jsonc");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(
            &path,
            r#"{
                // top comment
                "userSetting": "keep me", /* inline */
                "servers": {
                    "mine": { "url": "http://x", }, // trailing comma
                },
            }"#,
        )
        .unwrap();

        let s = schema(
            ConfigFormat::Jsonc,
            &["servers"],
            json!({"type":"http","url":"${url}"}),
        );
        render_servers(&s, &path, "vsc00001", &["gmail".to_string()], 9876).unwrap();

        let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["userSetting"], "keep me");
        assert_eq!(v["servers"]["mine"]["url"], "http://x");
        assert_eq!(v["servers"]["gmail"]["type"], "http");
        std::fs::remove_dir_all(&dir).ok();
    }

    // The stripper must NOT touch // or /* inside string literals.
    #[test]
    fn jsonc_strip_preserves_comment_like_strings() {
        let src = r#"{ "url": "http://example.com/a", "note": "a /* b */ c" }"#;
        let cleaned = strip_jsonc(src);
        let v: Value = serde_json::from_str(&cleaned).unwrap();
        assert_eq!(v["url"], "http://example.com/a");
        assert_eq!(v["note"], "a /* b */ c");
    }

    // Rule 3: walk-and-create a multi-segment key_path, preserving siblings.
    #[test]
    fn creates_nested_key_path_and_keeps_siblings() {
        let dir = tmp("nested");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("c.json");
        std::fs::write(&path, r#"{"mcp":{"existing":1}}"#).unwrap();

        let s = schema(
            ConfigFormat::Json,
            &["mcp", "servers"],
            json!({"url":"${url}"}),
        );
        render_servers(&s, &path, "k0000000", &["gmail".to_string()], 9876).unwrap();

        let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["mcp"]["existing"], 1);
        assert!(v["mcp"]["servers"]["gmail"]["url"].is_string());
        std::fs::remove_dir_all(&dir).ok();
    }

    // Rule 4: purge matches the endpoint anywhere in an entry (url-in-args),
    // not just a fixed `url` field.
    #[test]
    fn purge_matches_endpoint_inside_nested_args() {
        let dir = tmp("purge-args");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("zed.json");
        let s = schema(
            ConfigFormat::Json,
            &["context_servers"],
            json!({"command":"npx","args":["mcp-remote","${url}"]}),
        );
        render_servers(&s, &path, "ourkey00", &["gmail".to_string()], 9876).unwrap();
        // Add a user entry that is NOT ours.
        let mut cur: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        cur["context_servers"]["theirs"] = json!({"command":"npx","args":["other"]});
        std::fs::write(&path, serde_json::to_vec_pretty(&cur).unwrap()).unwrap();

        purge_mcp(&s, &path, "ourkey00", 9876).unwrap();

        let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(
            v["context_servers"]["gmail"].is_null(),
            "url-in-args entry purged"
        );
        assert!(
            v["context_servers"]["theirs"].is_object(),
            "user entry kept"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
