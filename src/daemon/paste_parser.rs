// Vercel-style paste-magic parser for the Add MCP server import modal.
// Spec §Add MCP server flow > Import modal.
//
// Inputs (any of):
//   - Full MCP JSON `{ "mcpServers": {...} }`
//   - JSON fragment `{ "command": ..., "env": ... }`
//   - Plain JSON object of env vars
//   - Dotenv-style lines (handles `export`, quotes, comments, blank lines)
//   - KEY=value single line
//   - YAML-ish KEY: value lines

use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedServer {
    pub name: Option<String>,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: Vec<ParsedEnv>,
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

    // Format 2: JSON fragment with "command" key
    if obj.contains_key("command") || obj.contains_key("args") {
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

    let env = obj
        .get("env")
        .and_then(|v| v.as_object())
        .map(|env_obj| {
            env_obj
                .iter()
                .map(|(k, v)| {
                    let value = match v {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    make_env(k.clone(), value)
                })
                .collect()
        })
        .unwrap_or_default();

    ParsedServer {
        name,
        command,
        args,
        env,
    }
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
