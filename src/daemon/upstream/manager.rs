//! Upstream lifecycle coordinator: spawns, caches, and reaps MCP clients.
//!
//! `UpstreamManager` owns the map of live `rmcp` clients keyed by server name
//! and forwards `tools/call` / `tools/list` to them, spawning lazily on first
//! use (`ensure_running`). A background reaper kills entries past their idle
//! TTL; `lifecycle_seconds` is `0` = ephemeral (killed right after the call),
//! `-1` = persistent, `N > 0` = idle TTL seconds. `RuntimeStatus` tracks
//! last-call health and publishes debounced `ServerHealthChanged` events.
//! `UpstreamError` is the typed error surface; `Rmcp` variants pass through to
//! the caller verbatim per the JSON-RPC pass-through rule.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rmcp::model::CallToolRequestParams;
use rmcp::service::{RoleClient, RunningService, ServiceError};
use serde_json::{Map, Value};
use thiserror::Error;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::daemon::events::{Bus, Event};
use crate::daemon::paths::Paths;
use crate::daemon::time_util::now_unix_secs;
use crate::daemon::upstream::health::HEALTH_EVENT_DEBOUNCE;
use crate::daemon::upstream::{http, stdio};

const ERROR_RING_CAP: usize = 5;

const EPHEMERAL: i32 = 0;
const PERSISTENT: i32 = -1;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const LIST_TOOLS_TIMEOUT: Duration = Duration::from_secs(60);
const CALL_TOOL_TIMEOUT: Duration = Duration::from_secs(600);

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
    /// HTTP MCP upstream (streamable-http transport). `bearer_token`, when
    /// set, becomes `Authorization: Bearer <token>` on every request.
    Http {
        url: String,
        bearer_token: Option<String>,
    },
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

    /// Upstream returned 401 Unauthorized with a parseable `WWW-Authenticate`
    /// header. The string is the raw header value, e.g.
    /// `Bearer realm="OAuth", resource_metadata="https://..."`.
    #[error("auth required: {0}")]
    AuthRequired(String),

    #[error("upstream process died between calls")]
    Crashed,

    #[error("operation timed out")]
    Timeout,

    #[error("serialization: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("expected JSON object for tool arguments, got {got}")]
    BadArgumentType { got: &'static str },
}

/// Fingerprint of the fields that would change the transport connection.
/// Used to detect when a config edit requires kill+respawn.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ConfigFingerprint {
    transport: Transport,
    lifecycle_seconds: i32,
}

impl ConfigFingerprint {
    fn from_config(config: &ServerConfig) -> Self {
        Self {
            transport: config.transport.clone(),
            lifecycle_seconds: config.lifecycle_seconds,
        }
    }
}

/// One running upstream client + last-use bookkeeping.
struct Entry {
    client: RunningService<RoleClient, ()>,
    last_use: Instant,
    lifecycle_seconds: i32,
    /// Config fingerprint at spawn time; used to detect stale entries on config edit.
    fingerprint: ConfigFingerprint,
    /// Number of calls currently in flight against this entry.
    in_flight: usize,
    /// Monotonically-increasing ID; ephemeral post-call kill compares this to
    /// avoid killing a concurrently-spawned fresh entry under the same name.
    generation: u64,
}

impl std::fmt::Debug for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // RunningService is not Debug; print the fields that matter.
        f.debug_struct("Entry")
            .field("last_use", &self.last_use)
            .field("lifecycle_seconds", &self.lifecycle_seconds)
            .field("in_flight", &self.in_flight)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Default)]
pub struct RuntimeStatus {
    pub last_call_at: Option<i64>,
    pub last_call_error: Option<String>,
    pub recent_errors: VecDeque<(i64, String)>,
    last_emit_at: Option<Instant>,
}

#[derive(Debug)]
pub struct UpstreamManager {
    state: Arc<Mutex<HashMap<String, Entry>>>,
    status: Arc<Mutex<HashMap<String, RuntimeStatus>>>,
    cancel: CancellationToken,
    bus: Option<Bus>,
    #[expect(
        dead_code,
        reason = "reserved for per-server log file paths in a later stage"
    )]
    paths: Paths,
}

impl UpstreamManager {
    pub fn new(paths: Paths, cancel: CancellationToken) -> Self {
        Self::with_bus(paths, cancel, None)
    }

    pub fn with_bus(paths: Paths, cancel: CancellationToken, bus: Option<Bus>) -> Self {
        let state = Arc::new(Mutex::new(HashMap::new()));
        let status = Arc::new(Mutex::new(HashMap::new()));
        let mgr = Self {
            state: state.clone(),
            status,
            cancel: cancel.clone(),
            bus,
            paths,
        };
        spawn_reaper(state, cancel);
        mgr
    }

    pub async fn snapshot(&self, server: &str) -> Option<RuntimeStatus> {
        self.status.lock().await.get(server).cloned()
    }

    async fn record(&self, server: &str, error: Option<String>) {
        let now = Instant::now();
        let unix = now_unix_secs();
        let mut map = self.status.lock().await;
        let entry = map.entry(server.to_string()).or_default();
        entry.last_call_at = Some(unix);
        if let Some(ref e) = error {
            entry.last_call_error = Some(e.clone());
            entry.recent_errors.push_back((unix, e.clone()));
            while entry.recent_errors.len() > ERROR_RING_CAP {
                entry.recent_errors.pop_front();
            }
        } else {
            entry.last_call_error = None;
        }
        let due = error.is_some()
            || entry
                .last_emit_at
                .is_none_or(|prev| now.duration_since(prev) >= HEALTH_EVENT_DEBOUNCE);
        if due {
            entry.last_emit_at = Some(now);
        }
        drop(map);
        if due {
            if let Some(bus) = &self.bus {
                bus.publish(Event::ServerHealthChanged {
                    name: server.to_string(),
                });
            }
        }
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
        if let Err(e) = self.ensure_running(config).await {
            self.record(&config.name, Some(e.to_string())).await;
            return Err(e);
        }

        let (peer, generation) = match self.get_peer_for_call(&config.name).await {
            Ok(p) => p,
            Err(e) => {
                self.record(&config.name, Some(e.to_string())).await;
                return Err(e);
            }
        };

        let result = tokio::time::timeout(CALL_TOOL_TIMEOUT, peer.call_tool(request)).await;

        // Always decrement in-flight, even on timeout or error.
        self.decrement_in_flight(&config.name).await;

        let response = match result {
            Err(_elapsed) => {
                let msg = "call_tool timed out".to_string();
                self.record(&config.name, Some(msg)).await;
                return Err(UpstreamError::Timeout);
            }
            Ok(Ok(call_result)) => serde_json::to_value(call_result)?,
            Ok(Err(e)) => {
                let msg = e.to_string();
                self.record(&config.name, Some(msg)).await;
                return Err(UpstreamError::Rmcp(e));
            }
        };
        self.record(&config.name, None).await;

        if config.lifecycle_seconds == EPHEMERAL {
            self.kill_generation(&config.name, generation).await;
        }

        Ok(response)
    }

    /// Fetch upstream's tools/list. Spawns lazily if needed.
    pub async fn list_tools(&self, config: &ServerConfig) -> Result<Value, UpstreamError> {
        self.ensure_running(config).await?;

        let (peer, _generation) = self.get_peer_for_call(&config.name).await?;

        let result = tokio::time::timeout(LIST_TOOLS_TIMEOUT, peer.list_tools(None)).await;

        self.decrement_in_flight(&config.name).await;

        match result {
            Err(_elapsed) => Err(UpstreamError::Timeout),
            Ok(Ok(list)) => Ok(serde_json::to_value(list)?),
            Ok(Err(e)) => Err(UpstreamError::Rmcp(e)),
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

    /// Decrement in-flight counter for an entry if it still exists.
    async fn decrement_in_flight(&self, server_name: &str) {
        let mut state = self.state.lock().await;
        if let Some(entry) = state.get_mut(server_name) {
            entry.in_flight = entry.in_flight.saturating_sub(1);
        }
    }

    /// Kill the entry for `server_name` only if it still has `generation`.
    /// Prevents an ephemeral post-call kill from evicting a concurrently-spawned fresh entry.
    async fn kill_generation(&self, server_name: &str, generation: u64) {
        let entry = {
            let mut state = self.state.lock().await;
            match state.get(server_name) {
                Some(e) if e.generation == generation => state.remove(server_name),
                _ => None,
            }
        };
        if let Some(e) = entry {
            cancel_entry(server_name, e).await;
        }
    }

    async fn get_peer_for_call(
        &self,
        server_name: &str,
    ) -> Result<(rmcp::service::Peer<RoleClient>, u64), UpstreamError> {
        let mut state = self.state.lock().await;
        let Some(entry) = state.get_mut(server_name) else {
            return Err(UpstreamError::Crashed);
        };
        if entry.client.peer().is_transport_closed() {
            state.remove(server_name);
            return Err(UpstreamError::Crashed);
        }
        entry.last_use = Instant::now();
        entry.in_flight += 1;
        let peer = entry.client.peer().clone();
        let generation = entry.generation;
        drop(state);
        Ok((peer, generation))
    }

    async fn ensure_running(&self, config: &ServerConfig) -> Result<(), UpstreamError> {
        // Kill stale entry when config has changed (fingerprint mismatch).
        let stale: Option<Entry> = {
            let mut state = self.state.lock().await;
            match state.get(config.name.as_str()) {
                Some(e)
                    if !e.client.peer().is_transport_closed()
                        && e.fingerprint != ConfigFingerprint::from_config(config) =>
                {
                    state.remove(config.name.as_str())
                }
                _ => None,
            }
        };
        if let Some(e) = stale {
            cancel_entry(&config.name, e).await;
        }

        let already_running = {
            let state = self.state.lock().await;
            state
                .get(&config.name)
                .is_some_and(|e| !e.client.peer().is_transport_closed())
        };
        if already_running {
            return Ok(());
        }

        // Each spawned client gets its own child token so that rmcp's
        // RunningService::cancel() only cancels that one client, not the
        // daemon-global token (UP-1).
        let child_ct = self.cancel.child_token();
        let client = tokio::time::timeout(
            CONNECT_TIMEOUT,
            connect_transport(&config.transport, &config.name, child_ct),
        )
        .await
        .map_err(|_elapsed| UpstreamError::Timeout)??;

        // Next generation number for this entry.
        let generation = {
            let state = self.state.lock().await;
            state
                .get(&config.name)
                .map_or(0, |e| e.generation.wrapping_add(1))
        };

        // Insert atomically OR detect a race; cancel any duplicate OUTSIDE the lock.
        let duplicate: Option<RunningService<RoleClient, ()>> = {
            let mut state = self.state.lock().await;
            let raced = state
                .get(&config.name)
                .is_some_and(|e| !e.client.peer().is_transport_closed());
            if raced {
                Some(client)
            } else {
                state.insert(
                    config.name.clone(),
                    Entry {
                        client,
                        last_use: Instant::now(),
                        lifecycle_seconds: config.lifecycle_seconds,
                        fingerprint: ConfigFingerprint::from_config(config),
                        in_flight: 0,
                        generation,
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

/// Connect using the given transport descriptor. Each call should receive a
/// fresh child `CancellationToken` so cancellation is scoped to this client only.
async fn connect_transport(
    transport: &Transport,
    server_name: &str,
    child_ct: CancellationToken,
) -> Result<RunningService<RoleClient, ()>, UpstreamError> {
    match transport {
        Transport::Stdio { command, args, env } => {
            stdio::connect(command, args, env, child_ct).await
        }
        Transport::Http { url, bearer_token } => {
            // Three cases: OAuth (stored creds → AuthClient w/ auto-refresh),
            // static bearer token, or no auth.
            match http::connect_oauth(url, server_name, child_ct.clone()).await? {
                Some(client) => Ok(client),
                None => http::connect(url, bearer_token.as_deref(), child_ct).await,
            }
        }
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
    // Never reap while a call is in flight — avoids killing mid-call.
    if entry.in_flight > 0 {
        return false;
    }
    if entry.client.peer().is_transport_closed() {
        return true;
    }
    match entry.lifecycle_seconds {
        PERSISTENT => false,
        // Ephemeral entries are only reaped here when idle (in_flight==0 above);
        // active ephemeral entries are killed by the caller after the call.
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

    #[test]
    fn child_token_does_not_cancel_parent() {
        // UP-1: cancelling the child token (as rmcp's RunningService::cancel would)
        // must not cancel the parent.
        let parent = CancellationToken::new();
        let child = parent.child_token();
        child.cancel();
        assert!(
            !parent.is_cancelled(),
            "child cancel must not propagate to parent"
        );
    }
}
