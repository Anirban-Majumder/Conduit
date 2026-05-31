//! MCP request bridging: forward IDE → upstream, run the policy + approval gate
//! for `tools/call`, and audit every decision. Spec §"MCP request flow".
//!
//! `route_tool_call` resolves the per-(server,tool) policy (Allow/Deny/Ask),
//! routes Ask through `ApprovalCenter`, forwards approved calls to the upstream
//! manager, and writes an `AuditEntry` for each tool-call outcome. Auth-flavoured
//! upstream errors flip the server's auth-error state (edge-triggered, emitting
//! `AuthFailure`/`AuthCleared` once per transition). Only `tools/list` and
//! `tools/call` are proxied; audit is scoped to `tools/call` outcomes.

use std::time::Instant;

use serde_json::Value;
use thiserror::Error;

use crate::daemon::approval::{ApprovalError, ApprovalOutcome, ApprovalRequest};
use crate::daemon::db::types::{AuditDecision, AuditEntry, Decision, DecisionSource, Server};
use crate::daemon::errors::DbError;
use crate::daemon::events::Event;
use crate::daemon::http_server::{server_to_config, AppState};
use crate::daemon::time_util::now_unix_secs;
use crate::daemon::upstream::manager::UpstreamError;
use crate::daemon::upstream::probe::message_looks_like_auth_failure;

#[derive(Debug)]
pub enum ProxyOutcome {
    Forward(Value),
    UserDenied,
    ApprovalTimeout,
}

#[derive(Debug, Error)]
pub enum ProxyError {
    #[error("db: {0}")]
    Db(#[from] DbError),

    #[error("approval: {0}")]
    Approval(#[from] ApprovalError),

    #[error("upstream: {0}")]
    Upstream(#[from] UpstreamError),
}

const TOOLS_CALL: &str = "tools/call";
const TOOLS_LIST: &str = "tools/list";

/// Top-level routing for a single JSON-RPC request from an IDE.
///
/// For `tools/call`: policy lookup → maybe approval → upstream forward → audit.
/// For everything else: upstream forward (pass-through). Audit is recorded for
/// `tools/call` outcomes only — list/initialize traffic is verbose and the spec
/// scopes audit to tool-level decisions.
#[expect(
    clippy::too_many_lines,
    reason = "single end-to-end match over policy + outcome"
)]
pub async fn route_tool_call(
    state: &AppState,
    ide_key: &str,
    ide_name: &str,
    server: &Server,
    method: &str,
    params: Value,
) -> Result<ProxyOutcome, ProxyError> {
    if method == TOOLS_LIST {
        let config = server_to_config(server).map_err(io_to_upstream)?;
        let result = state.upstream.list_tools(&config).await?;
        return Ok(ProxyOutcome::Forward(result));
    }
    if method != TOOLS_CALL {
        // v0 only proxies tools/list and tools/call. initialize is handled
        // internally by rmcp during upstream spawn; everything else returns
        // method_not_found at the JSON-RPC layer in the HTTP handler.
        return Err(ProxyError::Upstream(UpstreamError::InitFailed(format!(
            "method not supported: {method}"
        ))));
    }

    let tool_name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let args_json = serde_json::to_string(&params).unwrap_or_else(|_| "null".to_string());

    let (decision, source) = match state.db.get_policy(&server.name, &tool_name).await? {
        Some(p) => (p.decision, p.decision_source),
        None => (
            state.db.resolve_policy(&server.name, &tool_name).await?,
            DecisionSource::Default,
        ),
    };

    let start = Instant::now();
    let outcome = match decision {
        Decision::Allow => {
            forward_and_audit(
                state,
                ide_key,
                ide_name,
                server,
                &tool_name,
                params,
                &args_json,
                Some(source),
                AuditDecision::Allow,
                start,
            )
            .await?
        }
        Decision::Deny => {
            insert_audit(
                state,
                ide_key,
                ide_name,
                &server.name,
                &tool_name,
                &args_json,
                AuditDecision::Deny,
                Some(source),
                start.elapsed(),
                None,
                None,
            )
            .await;
            ProxyOutcome::UserDenied
        }
        Decision::Ask => {
            let approval_outcome = state
                .approvals
                .request(ApprovalRequest {
                    server: server.name.clone(),
                    tool: tool_name.clone(),
                    args_json: args_json.clone(),
                    ide_key: ide_key.to_string(),
                })
                .await?;
            match approval_outcome {
                ApprovalOutcome::Approved => {
                    forward_and_audit(
                        state,
                        ide_key,
                        ide_name,
                        server,
                        &tool_name,
                        params,
                        &args_json,
                        Some(source),
                        AuditDecision::AskApproved,
                        start,
                    )
                    .await?
                }
                ApprovalOutcome::Denied => {
                    insert_audit(
                        state,
                        ide_key,
                        ide_name,
                        &server.name,
                        &tool_name,
                        &args_json,
                        AuditDecision::AskDenied,
                        Some(source),
                        start.elapsed(),
                        None,
                        None,
                    )
                    .await;
                    ProxyOutcome::UserDenied
                }
                ApprovalOutcome::Timeout => {
                    insert_audit(
                        state,
                        ide_key,
                        ide_name,
                        &server.name,
                        &tool_name,
                        &args_json,
                        AuditDecision::AskTimeout,
                        Some(source),
                        start.elapsed(),
                        None,
                        None,
                    )
                    .await;
                    ProxyOutcome::ApprovalTimeout
                }
            }
        }
    };

    Ok(outcome)
}

#[expect(
    clippy::too_many_arguments,
    reason = "audit entry has many denormalised fields"
)]
async fn forward_and_audit(
    state: &AppState,
    ide_key: &str,
    ide_name: &str,
    server: &Server,
    tool_name: &str,
    params: Value,
    args_json: &str,
    source: Option<DecisionSource>,
    decision: AuditDecision,
    start: Instant,
) -> Result<ProxyOutcome, ProxyError> {
    let config = server_to_config(server).map_err(io_to_upstream)?;
    match state.upstream.call_tool(&config, TOOLS_CALL, params).await {
        Ok(result) => {
            let latency = start.elapsed();
            let response_status = "ok".to_string();
            let response_len = i64::try_from(serde_json::to_string(&result).map_or(0, |s| s.len()))
                .unwrap_or(i64::MAX);
            insert_audit(
                state,
                ide_key,
                ide_name,
                &server.name,
                tool_name,
                args_json,
                decision,
                source,
                latency,
                Some(response_status),
                Some(response_len),
            )
            .await;
            maybe_clear_auth_error(state, &server.name).await;
            Ok(ProxyOutcome::Forward(result))
        }
        Err(e) => {
            let latency = start.elapsed();
            insert_audit(
                state,
                ide_key,
                ide_name,
                &server.name,
                tool_name,
                args_json,
                AuditDecision::Error,
                source,
                latency,
                None,
                None,
            )
            .await;
            maybe_record_auth_error(state, &server.name, &e).await;
            Err(ProxyError::Upstream(e))
        }
    }
}

async fn maybe_record_auth_error(state: &AppState, server: &str, err: &UpstreamError) {
    let msg = err.to_string();
    if !message_looks_like_auth_failure(&msg) {
        return;
    }
    match state.db.set_server_auth_error(server, &msg).await {
        Ok(true) => {
            state.bus.publish(Event::AuthFailure {
                server: server.to_string(),
                message: msg,
            });
        }
        Ok(false) => {}
        Err(e) => tracing::warn!(error = %e, server, "set_server_auth_error failed"),
    }
}

async fn maybe_clear_auth_error(state: &AppState, server: &str) {
    match state.db.clear_server_auth_error(server).await {
        Ok(true) => {
            state.bus.publish(Event::AuthCleared {
                server: server.to_string(),
            });
        }
        Ok(false) => {}
        Err(e) => tracing::warn!(error = %e, server, "clear_server_auth_error failed"),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "audit entry has many denormalised fields"
)]
async fn insert_audit(
    state: &AppState,
    ide_key: &str,
    ide_name: &str,
    server: &str,
    tool: &str,
    args_json: &str,
    decision: AuditDecision,
    source: Option<DecisionSource>,
    latency: std::time::Duration,
    response_status: Option<String>,
    response_length: Option<i64>,
) {
    let entry = AuditEntry {
        id: None,
        ts: now_unix_secs(),
        ide_key: Some(ide_key.to_string()),
        ide_name: Some(ide_name.to_string()),
        server: server.to_string(),
        tool: Some(tool.to_string()),
        decision,
        decision_source: source,
        latency_ms: i64::try_from(latency.as_millis()).ok(),
        request_args: Some(args_json.to_string()),
        response_status,
        response_length,
        error_code: None,
    };
    match state.db.insert_audit_entry(entry).await {
        Ok(_) => {
            state.bus.publish(Event::AuditAppended);
        }
        Err(e) => tracing::warn!(error = %e, server, tool, "audit insert failed"),
    }
}

fn io_to_upstream(e: crate::daemon::http_server::ServerError) -> ProxyError {
    ProxyError::Upstream(UpstreamError::InitFailed(e.to_string()))
}
