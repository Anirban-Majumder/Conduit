//! axum HTTP server bound to 127.0.0.1 — the MCP wire endpoint IDEs connect to.
//! Spec §"Transport: HTTP MCP". URL: `http://localhost:<port>/<ide-key>/<server>/mcp`.
//!
//! `handle_mcp` parses the JSON-RPC envelope, short-circuits notifications,
//! handles `initialize`/`ping` locally, validates the IDE key and server, then
//! hands tool traffic to `mcp_proxy::route_tool_call`. Outcomes map to JSON-RPC
//! results or the spec's error codes (`USER_DENIED` … `INTERNAL`). `AppState` is
//! the shared handle every handler clones. Serves until the `CancellationToken`
//! fires (graceful shutdown).

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::daemon::config::SharedSettings;
use crate::daemon::db::types::McpServer;
use crate::daemon::db::Db;
use crate::daemon::downstream::approval::ApprovalCenter;
use crate::daemon::downstream::proxy::{route_tool_call, ProxyError, ProxyOutcome};
use crate::daemon::events::Bus;
use crate::daemon::upstream::manager::UpstreamManager;

/// Shared state every handler reads from. Cheap to clone (everything wraps Arc/Sender).
#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub approvals: Arc<ApprovalCenter>,
    pub upstream: Arc<UpstreamManager>,
    pub bus: Bus,
    pub port: u16,
    pub settings: SharedSettings,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Error)]
pub enum ServerError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Bind axum on 127.0.0.1:<port>, serve until `cancel` fires.
pub async fn serve(state: AppState, cancel: CancellationToken) -> Result<(), ServerError> {
    let port = state.port;
    let app = Router::new()
        .route("/{key}/{server}/mcp", post(handle_mcp).get(handle_mcp_get))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    tracing::info!(port, "axum listening on 127.0.0.1");

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            cancel.cancelled().await;
        })
        .await?;
    Ok(())
}

/// Streamable HTTP transport sometimes uses GET for SSE-style server→client
/// notifications. For v0 we return 405 — IDEs we target use POST request/response.
async fn handle_mcp_get() -> (StatusCode, &'static str) {
    (StatusCode::METHOD_NOT_ALLOWED, "GET unsupported in v0")
}

async fn handle_mcp(
    Path((key, server)): Path<(String, String)>,
    State(state): State<AppState>,
    Json(envelope): Json<Value>,
) -> axum::response::Response {
    let id = envelope.get("id").cloned().unwrap_or(Value::Null);
    let method = envelope
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let params = envelope.get("params").cloned().unwrap_or(Value::Null);

    // JSON-RPC notifications carry no `id` and expect no response.
    if envelope.get("id").is_none() || method.starts_with("notifications/") {
        return (StatusCode::ACCEPTED, "").into_response();
    }

    match state.db.get_settings_kv("routing_paused").await {
        Ok(Some(v)) if v == "true" => {
            return rpc_error_response(id, DAEMON_PAUSED, "daemon_paused");
        }
        Ok(_) => {}
        Err(e) => return rpc_error_response(id, INTERNAL, &format!("db: {e}")),
    }

    let client_lookup = match state.db.lookup_client_key(&key).await {
        Ok(v) => v,
        Err(e) => return rpc_error_response(id, INTERNAL, &format!("db: {e}")),
    };
    let Some(client) = client_lookup else {
        return rpc_error_response(id, IDE_KEY_INVALID, "ide_key_invalid");
    };

    if method == "initialize" {
        return rpc_success_response(id, conduit_initialize_result(&params));
    }
    if method == "ping" {
        return rpc_success_response(id, json!({}));
    }

    let server_lookup = match state.db.get_server(&server).await {
        Ok(v) => v,
        Err(e) => return rpc_error_response(id, INTERNAL, &format!("db: {e}")),
    };
    let Some(server_cfg) = server_lookup else {
        return rpc_error_response(id, SERVER_NOT_FOUND, "server_not_found");
    };

    match route_tool_call(
        &state,
        &client.mcp_key,
        &client.name,
        &server_cfg,
        &method,
        params,
    )
    .await
    {
        Ok(ProxyOutcome::Forward(result)) => rpc_success_response(id, result),
        Ok(ProxyOutcome::UserDenied) => rpc_error_response(id, USER_DENIED, "user_denied"),
        Ok(ProxyOutcome::ApprovalTimeout) => {
            rpc_error_response(id, APPROVAL_TIMEOUT, "approval_timeout")
        }
        Err(ProxyError::Upstream(e)) => rpc_error_response(id, INTERNAL, &e.to_string()),
        Err(ProxyError::Db(e)) => rpc_error_response(id, INTERNAL, &format!("db: {e}")),
        Err(ProxyError::Approval(e)) => rpc_error_response(id, INTERNAL, &format!("approval: {e}")),
    }
}

const SUPPORTED_PROTOCOL_VERSIONS: &[&str] =
    &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const DEFAULT_PROTOCOL_VERSION: &str = "2025-06-18";

fn conduit_initialize_result(params: &Value) -> Value {
    let requested = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or("");
    let negotiated = if SUPPORTED_PROTOCOL_VERSIONS.contains(&requested) {
        requested
    } else {
        DEFAULT_PROTOCOL_VERSION
    };
    json!({
        "protocolVersion": negotiated,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": {
            "name": "conduit",
            "version": env!("CARGO_PKG_VERSION"),
        },
    })
}

/// JSON-RPC error codes per spec §"Error semantics".
pub const USER_DENIED: i32 = -32000;
pub const APPROVAL_TIMEOUT: i32 = -32001;
pub const SERVER_NOT_FOUND: i32 = -32002;
pub const IDE_KEY_INVALID: i32 = -32003;
pub const DAEMON_PAUSED: i32 = -32004;
pub const INTERNAL: i32 = -32603;

fn rpc_success_response(id: Value, result: Value) -> axum::response::Response {
    Json(json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    }))
    .into_response()
}

fn rpc_error_response(id: Value, code: i32, reason: &str) -> axum::response::Response {
    Json(json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": reason,
            "data": { "reason": reason },
        },
    }))
    .into_response()
}

/// Resolve a DB-stored `McpServer` into the upstream-layer `ServerConfig`, fetching
/// keyring secrets. Caller must already have the `McpServer` row.
///
/// All keyring I/O runs inside a single `spawn_blocking` call so that
/// synchronous D-Bus/keychain operations do not block tokio workers.
pub(crate) async fn server_to_config(
    server: &McpServer,
) -> Result<crate::daemon::upstream::manager::ServerConfig, ServerError> {
    use crate::daemon::db::types::{EnvValue, TRANSPORT_HTTP, TRANSPORT_STDIO};
    use crate::daemon::secrets;
    use crate::daemon::upstream::http::BEARER_TOKEN_ENV;
    use crate::daemon::upstream::manager::{ServerConfig, Transport};

    // Collect the data needed inside the blocking closure up front so we do
    // not borrow `server` across the spawn boundary.
    let server_name = server.name.clone();
    let env_entries: Vec<(String, EnvValue)> = server
        .env
        .iter()
        .map(|e| (e.name.clone(), e.value.clone()))
        .collect();

    let resolved_env: Vec<(String, String)> =
        tokio::task::spawn_blocking(move || -> Vec<(String, String)> {
            let mut out = Vec::with_capacity(env_entries.len());
            for (name, value) in env_entries {
                let resolved = match value {
                    EnvValue::Plain(v) => v,
                    EnvValue::Secret(uuid) => match secrets::fetch(&server_name, &uuid) {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                server = %server_name,
                                env = %name,
                                "secret fetch failed"
                            );
                            String::new()
                        }
                    },
                };
                out.push((name, resolved));
            }
            out
        })
        .await
        .map_err(|e| {
            ServerError::Io(std::io::Error::other(format!(
                "spawn_blocking join error: {e}"
            )))
        })?;

    let transport = match server.transport.as_str() {
        TRANSPORT_STDIO => Transport::Stdio {
            command: server.command.clone().unwrap_or_default(),
            args: server.args.clone(),
            env: resolved_env,
        },
        TRANSPORT_HTTP => {
            let bearer_token = resolved_env
                .iter()
                .find(|(name, _)| name == BEARER_TOKEN_ENV)
                .map(|(_, value)| value.clone());
            Transport::Http {
                url: server.url.clone().unwrap_or_default(),
                bearer_token,
            }
        }
        other => {
            return Err(ServerError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("unknown transport: {other}"),
            )));
        }
    };

    Ok(ServerConfig {
        name: server.name.clone(),
        transport,
        lifecycle_seconds: server.lifecycle_seconds,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, reason = "test ergonomics")]
    use super::*;

    #[test]
    fn error_codes_match_spec() {
        assert_eq!(USER_DENIED, -32000);
        assert_eq!(APPROVAL_TIMEOUT, -32001);
        assert_eq!(SERVER_NOT_FOUND, -32002);
        assert_eq!(IDE_KEY_INVALID, -32003);
        assert_eq!(DAEMON_PAUSED, -32004);
        assert_eq!(INTERNAL, -32603);
    }

    #[test]
    fn initialize_echoes_client_protocol_when_supported() {
        let req = json!({ "protocolVersion": "2025-03-26" });
        let res = conduit_initialize_result(&req);
        assert_eq!(res["protocolVersion"], "2025-03-26");
        assert_eq!(res["serverInfo"]["name"], "conduit");
        assert!(res["capabilities"]["tools"].is_object());
    }

    #[test]
    fn initialize_falls_back_when_client_version_unknown() {
        let req = json!({ "protocolVersion": "9999-12-31" });
        let res = conduit_initialize_result(&req);
        assert_eq!(res["protocolVersion"], DEFAULT_PROTOCOL_VERSION);
    }

    #[test]
    fn initialize_falls_back_when_client_omits_version() {
        let res = conduit_initialize_result(&json!({}));
        assert_eq!(res["protocolVersion"], DEFAULT_PROTOCOL_VERSION);
    }
}
