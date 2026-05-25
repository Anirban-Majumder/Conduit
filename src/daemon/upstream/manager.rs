// Upstream lifecycle manager: lazy spawn on first call, idle TTL tracker.
// Spec §"Server lifecycle".

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rmcp::model::CallToolRequestParams;
use rmcp::service::{RoleClient, RunningService, ServiceError};
use serde_json::{Map, Value};
use thiserror::Error;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::daemon::upstream::{http, stdio};
use crate::paths::Paths;

const EPHEMERAL: i32 = 0;
const PERSISTENT: i32 = -1;

/// Transport descriptor for an upstream MCP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    /// stdio child-process upstream — `rmcp::transport::child_process`.
    Stdio {
        command: String,
        args: Vec<String>,
        /// Env entries already resolved (secrets fetched from the keyring).
        env: Vec<(String, String)>,
    },
    /// HTTP MCP upstream (streamable-http transport).
    Http { url: String },
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub name: String,
    pub transport: Transport,
    /// `0` = ephemeral, `-1` = persistent, `N > 0` = idle TTL seconds.
    pub lifecycle_seconds: i32,
}

#[derive(Debug, Error)]
pub enum UpstreamError {
    #[error("rmcp service error: {0}")]
    Rmcp(#[from] ServiceError),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("initialize failed: {0}")]
    InitFailed(String),

    #[error("upstream process died between calls")]
    Crashed,

    #[error("operation timed out")]
    Timeout,

    #[error("serialization: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("expected JSON object for tool arguments, got {got}")]
    BadArgumentType { got: &'static str },
}

/// One running upstream client + last-use bookkeeping.
struct Entry {
    client: RunningService<RoleClient, ()>,
    last_use: Instant,
    lifecycle_seconds: i32,
}

impl std::fmt::Debug for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // RunningService is not Debug; print the fields that matter.
        f.debug_struct("Entry")
            .field("last_use", &self.last_use)
            .field("lifecycle_seconds", &self.lifecycle_seconds)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub struct UpstreamManager {
    state: Arc<Mutex<HashMap<String, Entry>>>,
    cancel: CancellationToken,
    // `paths` is held so a future stage can route per-server log files
    // (spec §"Debug + diagnostics"); not yet wired.
    #[expect(
        dead_code,
        reason = "reserved for per-server log file paths in a later stage"
    )]
    paths: Paths,
}

impl UpstreamManager {
    pub fn new(paths: Paths, cancel: CancellationToken) -> Self {
        let state = Arc::new(Mutex::new(HashMap::new()));
        let mgr = Self {
            state: state.clone(),
            cancel: cancel.clone(),
            paths,
        };
        spawn_reaper(state, cancel);
        mgr
    }

    /// Forward a JSON-RPC tools/call to the named upstream. Spawns lazily.
    /// On ephemeral (`lifecycle_seconds == 0`) the upstream is killed
    /// immediately after the call returns. The returned `Value` is the
    /// pre-serialized `CallToolResult`; JSON-RPC errors from the upstream
    /// surface as `UpstreamError::Rmcp(...)` and the caller is expected to
    /// translate them back into JSON-RPC error responses verbatim per the
    /// pass-through rule (spec §"Error semantics").
    pub async fn call_tool(
        &self,
        config: &ServerConfig,
        method: &str,
        params: Value,
    ) -> Result<Value, UpstreamError> {
        if method != "tools/call" {
            return Err(UpstreamError::InitFailed(format!(
                "call_tool only forwards tools/call, got {method}"
            )));
        }

        let request = parse_tool_call_params(params)?;
        self.ensure_running(config).await?;

        let mut state = self.state.lock().await;
        let peer = {
            let Some(entry) = state.get_mut(&config.name) else {
                return Err(UpstreamError::Crashed);
            };
            if entry.client.is_closed() {
                state.remove(&config.name);
                return Err(UpstreamError::Crashed);
            }
            entry.last_use = Instant::now();
            entry.client.peer().clone()
        };
        drop(state);

        let response = match peer.call_tool(request).await {
            Ok(call_result) => serde_json::to_value(call_result)?,
            Err(e) => return Err(UpstreamError::Rmcp(e)),
        };

        if config.lifecycle_seconds == EPHEMERAL {
            self.kill(&config.name).await;
        }

        Ok(response)
    }

    /// Fetch upstream's tools/list. Spawns lazily if needed.
    pub async fn list_tools(&self, config: &ServerConfig) -> Result<Value, UpstreamError> {
        self.ensure_running(config).await?;

        let mut state = self.state.lock().await;
        let peer = {
            let Some(entry) = state.get_mut(&config.name) else {
                return Err(UpstreamError::Crashed);
            };
            if entry.client.is_closed() {
                state.remove(&config.name);
                return Err(UpstreamError::Crashed);
            }
            entry.last_use = Instant::now();
            entry.client.peer().clone()
        };
        drop(state);

        match peer.list_tools(None).await {
            Ok(list) => Ok(serde_json::to_value(list)?),
            Err(e) => Err(UpstreamError::Rmcp(e)),
        }
    }

    /// Force-kill an upstream by server name. Idempotent.
    pub async fn kill(&self, server_name: &str) {
        let entry = {
            let mut state = self.state.lock().await;
            state.remove(server_name)
        };
        if let Some(entry) = entry {
            cancel_entry(server_name, entry).await;
        }
    }

    /// Force-kill every upstream. Idempotent. Shutdown path.
    pub async fn kill_all(&self) {
        let drained: Vec<(String, Entry)> = {
            let mut state = self.state.lock().await;
            state.drain().collect()
        };
        for (name, entry) in drained {
            cancel_entry(&name, entry).await;
        }
    }

    async fn ensure_running(&self, config: &ServerConfig) -> Result<(), UpstreamError> {
        let already_running = {
            let state = self.state.lock().await;
            state
                .get(&config.name)
                .is_some_and(|e| !e.client.is_closed())
        };
        if already_running {
            return Ok(());
        }

        // Spawn outside the lock — the initialize handshake can take seconds.
        let client = match &config.transport {
            Transport::Stdio { command, args, env } => {
                stdio::connect(command, args, env, self.cancel.clone()).await?
            }
            Transport::Http { url } => http::connect(url, self.cancel.clone()).await?,
        };

        // Insert atomically OR detect a race; cancel any duplicate OUTSIDE the lock.
        let duplicate: Option<RunningService<RoleClient, ()>> = {
            let mut state = self.state.lock().await;
            let raced = state
                .get(&config.name)
                .is_some_and(|e| !e.client.is_closed());
            if raced {
                Some(client)
            } else {
                state.insert(
                    config.name.clone(),
                    Entry {
                        client,
                        last_use: Instant::now(),
                        lifecycle_seconds: config.lifecycle_seconds,
                    },
                );
                None
            }
        };

        if let Some(dup) = duplicate {
            if let Err(e) = dup.cancel().await {
                tracing::warn!(error = %e, server = %config.name, "duplicate-spawn cancel failed");
            }
        }
        Ok(())
    }
}

/// Convert a possibly-untyped JSON-RPC params blob into rmcp's typed
/// `CallToolRequestParams`. Accepts the shape `{ "name": "...",
/// "arguments": { ... } }` per the MCP spec.
fn parse_tool_call_params(params: Value) -> Result<CallToolRequestParams, UpstreamError> {
    let mut obj = match params {
        Value::Object(map) => map,
        Value::Null => Map::new(),
        Value::String(_) => return Err(UpstreamError::BadArgumentType { got: "string" }),
        Value::Number(_) => return Err(UpstreamError::BadArgumentType { got: "number" }),
        Value::Bool(_) => return Err(UpstreamError::BadArgumentType { got: "bool" }),
        Value::Array(_) => return Err(UpstreamError::BadArgumentType { got: "array" }),
    };

    let Some(Value::String(name)) = obj.remove("name") else {
        return Err(UpstreamError::InitFailed(
            "tools/call params missing string 'name'".into(),
        ));
    };

    let arguments = match obj.remove("arguments") {
        Some(Value::Object(m)) => Some(m),
        Some(Value::Null) | None => None,
        Some(_) => return Err(UpstreamError::BadArgumentType { got: "non-object" }),
    };

    let mut req = CallToolRequestParams::new(name);
    req.arguments = arguments;
    Ok(req)
}

async fn cancel_entry(server_name: &str, entry: Entry) {
    // RunningService::cancel consumes self; ignore errors (Drop also kills
    // the child process, but cancel waits for the rmcp loop to finish so
    // we know the subprocess is truly gone before we return).
    let Entry { client, .. } = entry;
    if let Err(e) = client.cancel().await {
        tracing::warn!(server = %server_name, error = %e, "upstream cancel join failed");
    }
}

/// Periodically scan for idle entries past their TTL and kill them.
/// Bounded work: one timer tick + one lock acquisition per period; the
/// number of upstreams per daemon is small (dozens at most).
fn spawn_reaper(state: Arc<Mutex<HashMap<String, Entry>>>, cancel: CancellationToken) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                _ = tick.tick() => {
                    let to_kill = collect_expired(&state).await;
                    for (name, entry) in to_kill {
                        cancel_entry(&name, entry).await;
                    }
                }
            }
        }
    });
}

async fn collect_expired(state: &Arc<Mutex<HashMap<String, Entry>>>) -> Vec<(String, Entry)> {
    let mut guard = state.lock().await;
    let now = Instant::now();
    let expired_names: Vec<String> = guard
        .iter()
        .filter(|(_, entry)| is_expired(entry, now))
        .map(|(name, _)| name.clone())
        .collect();
    let mut out = Vec::with_capacity(expired_names.len());
    for name in expired_names {
        if let Some(entry) = guard.remove(&name) {
            out.push((name, entry));
        }
    }
    drop(guard);
    out
}

fn is_expired(entry: &Entry, now: Instant) -> bool {
    if entry.client.is_closed() {
        return true;
    }
    match entry.lifecycle_seconds {
        PERSISTENT => false,
        EPHEMERAL => true,
        n if n > 0 => {
            #[expect(clippy::cast_sign_loss, reason = "branch guarded by `n > 0` above")]
            let ttl = Duration::from_secs(n as u64);
            now.duration_since(entry.last_use) >= ttl
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tool_call_params_ok() {
        let v = serde_json::json!({
            "name": "send_email",
            "arguments": { "to": "a@b.c" }
        });
        let parsed = parse_tool_call_params(v).expect("parse");
        assert_eq!(parsed.name, "send_email");
        assert!(parsed.arguments.is_some());
    }

    #[test]
    fn parse_tool_call_params_no_arguments() {
        let v = serde_json::json!({ "name": "ping" });
        let parsed = parse_tool_call_params(v).expect("parse");
        assert_eq!(parsed.name, "ping");
        assert!(parsed.arguments.is_none());
    }

    #[test]
    fn parse_tool_call_params_rejects_array_root() {
        let v = serde_json::json!([1, 2, 3]);
        assert!(matches!(
            parse_tool_call_params(v),
            Err(UpstreamError::BadArgumentType { got: "array" })
        ));
    }

    #[test]
    fn parse_tool_call_params_rejects_missing_name() {
        let v = serde_json::json!({ "arguments": {} });
        assert!(matches!(
            parse_tool_call_params(v),
            Err(UpstreamError::InitFailed(_))
        ));
    }
}
