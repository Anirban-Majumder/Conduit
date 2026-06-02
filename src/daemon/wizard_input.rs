//! Add-Server wizard input: the "paste-magic" import parser (below) plus the
//! name/lifecycle validators (bottom). Spec §Add MCP server flow > Import modal.
//!
//! `parse` sniffs the format and returns a `ParsedServer`: full MCP JSON
//! (`{ "mcpServers": {...} }`), a bare command/url fragment, a plain env-var JSON
//! object, dotenv lines, or `KEY: value`/`KEY=value` pairs. Env/header names
//! matching `SECRET_KEYWORDS` are flagged `is_secret` so the wizard routes them
//! to the keyring.

use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedServer {
    pub name: Option<String>,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: Vec<ParsedEnv>,
    /// HTTP servers use `url` instead of `command`/`args`.
    pub url: Option<String>,
    /// HTTP servers may carry `headers` (e.g. `Authorization`, `X-API-Key`).
    /// Headers with secret-keyword names get `is_secret=true` like env entries.
    pub headers: Vec<ParsedEnv>,
}

impl ParsedServer {
    /// Inferred transport based on which fields are populated.
    /// Returns "http" if `url` is set, "stdio" otherwise.
    #[must_use]
    pub const fn inferred_transport(&self) -> &'static str {
        if self.url.is_some() {
            "http"
        } else {
            "stdio"
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedEnv {
    pub name: String,
    pub value: String,
    pub is_secret: bool,
}

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("no recognised format found in pasted text")]
    NoMatch,
}

const SECRET_KEYWORDS: &[&str] = &[
    "key",
    "token",
    "secret",
    "password",
    "credential",
    "auth",
    "bearer",
];

fn is_secret_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    SECRET_KEYWORDS.iter().any(|kw| lower.contains(kw))
}

fn make_env(name: String, value: String) -> ParsedEnv {
    let is_secret = is_secret_name(&name);
    ParsedEnv {
        name,
        value,
        is_secret,
    }
}

fn strip_quotes(s: &str) -> &str {
    let s = s.trim();
    if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')) {
        // Safety: we just confirmed the string starts and ends with a 1-byte ASCII char.
        let mut chars = s.chars();
        chars.next();
        chars.next_back();
        chars.as_str()
    } else {
        s
    }
}

fn env_from_json_object(obj: &serde_json::Map<String, Value>) -> Option<Vec<ParsedEnv>> {
    let env: Vec<ParsedEnv> = obj
        .iter()
        .map(|(k, v)| {
            let value = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            make_env(k.clone(), value)
        })
        .collect();
    if env.is_empty() {
        None
    } else {
        Some(env)
    }
}

fn parse_json(input: &str) -> Option<ParsedServer> {
    let value: Value = serde_json::from_str(input).ok()?;
    let obj = value.as_object()?;

    // Format 1: full `{ "mcpServers": { "<name>": { ... } } }`
    if let Some(servers_val) = obj.get("mcpServers") {
        let servers = servers_val.as_object()?;
        let (name, server_obj) = servers.iter().next()?;
        let server = server_obj.as_object()?;
        return Some(extract_server_fields(server, Some(name.clone())));
    }

    // Format 2: JSON fragment with stdio (`command`/`args`) or http (`url`) shape.
    if obj.contains_key("command") || obj.contains_key("args") || obj.contains_key("url") {
        return Some(extract_server_fields(obj, None));
    }

    // Format 3: plain env-var object — all values must be strings
    let all_strings = obj.values().all(Value::is_string);
    if all_strings && !obj.is_empty() {
        let env = env_from_json_object(obj)?;
        return Some(ParsedServer {
            env,
            ..Default::default()
        });
    }

    None
}

fn extract_server_fields(
    obj: &serde_json::Map<String, Value>,
    name: Option<String>,
) -> ParsedServer {
    let command = obj
        .get("command")
        .and_then(|v| v.as_str())
        .map(str::to_owned);

    let args = obj
        .get("args")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();

    let env = extract_string_map(obj.get("env"));

    // HTTP-style entries: `url` + optional `headers` map (Claude/Cline style).
    let url = obj.get("url").and_then(|v| v.as_str()).map(str::to_owned);
    let headers = extract_string_map(obj.get("headers"));

    ParsedServer {
        name,
        command,
        args,
        env,
        url,
        headers,
    }
}

/// Decode a `{ key: "value" }` JSON object into the `ParsedEnv` list shape.
/// Non-string values are stringified (matches v0 behavior for `env`). Returns
/// an empty vec for missing / non-object input.
fn extract_string_map(value: Option<&Value>) -> Vec<ParsedEnv> {
    value
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .map(|(k, v)| {
                    let value = match v {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    make_env(k.clone(), value)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_dotenv(input: &str) -> Option<Vec<ParsedEnv>> {
    let mut env = Vec::new();
    let mut any_matched = false;

    for line in input.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let mut parts = line.splitn(2, '=');
        let key = parts.next()?.trim();
        let raw_val = parts.next()?;
        let val = strip_quotes(raw_val.trim());
        if key.is_empty() {
            return None;
        }
        env.push(make_env(key.to_owned(), val.to_owned()));
        any_matched = true;
    }

    if any_matched {
        Some(env)
    } else {
        None
    }
}

fn parse_yaml_kv(input: &str) -> Option<Vec<ParsedEnv>> {
    let mut env = Vec::new();
    let mut any_matched = false;

    for line in input.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(2, ':');
        let key = parts.next()?.trim();
        let raw_val = parts.next()?;
        let val = strip_quotes(raw_val.trim());
        if key.is_empty() {
            return None;
        }
        env.push(make_env(key.to_owned(), val.to_owned()));
        any_matched = true;
    }

    if any_matched {
        Some(env)
    } else {
        None
    }
}

/// Parse pasted text into a [`ParsedServer`], detecting the format automatically.
pub fn parse(input: &str) -> Result<ParsedServer, ParseError> {
    let trimmed = input.trim();

    if trimmed.starts_with('{') {
        if let Some(parsed) = parse_json(trimmed) {
            return Ok(parsed);
        }
    }

    if let Some(env) = parse_dotenv(trimmed) {
        return Ok(ParsedServer {
            env,
            ..Default::default()
        });
    }

    if let Some(env) = parse_yaml_kv(trimmed) {
        return Ok(ParsedServer {
            env,
            ..Default::default()
        });
    }

    Err(ParseError::NoMatch)
}

// ── Add-Server name & lifecycle validation ──────────────────────────
pub const LIFECYCLE_EPHEMERAL: i32 = 0;
pub const LIFECYCLE_PERSISTENT: i32 = -1;
pub const LIFECYCLE_IDLE_MIN: i32 = 1;
pub const LIFECYCLE_IDLE_MAX: i32 = 86_400;
pub const NAME_MAX_LEN: usize = 64;

/// True iff `name` matches `^[a-z0-9_-]{1,64}$`.
pub fn is_valid_server_name(name: &str) -> bool {
    if name.is_empty() || name.len() > NAME_MAX_LEN {
        return false;
    }
    name.bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// Clamp the wizard's "idle TTL" stepper to `[1, 86400]` seconds. The wizard
/// also exposes Ephemeral (`0`) and Persistent (`-1`) as separate radio
/// choices — those bypass this clamp.
pub fn clamp_idle_lifecycle(raw: i32) -> i32 {
    raw.clamp(LIFECYCLE_IDLE_MIN, LIFECYCLE_IDLE_MAX)
}

/// Coerce a wizard transport + idle-stepper into the canonical
/// `lifecycle_seconds` int. `mode`: "ephemeral" | "persistent" | "idle".
pub fn lifecycle_from_wizard(mode: &str, idle_raw: i32) -> i32 {
    match mode {
        "ephemeral" => LIFECYCLE_EPHEMERAL,
        "persistent" => LIFECYCLE_PERSISTENT,
        _ => clamp_idle_lifecycle(idle_raw),
    }
}
