//! Server deletion with cascade.
//!
//! DB-side FK CASCADE removes `managed_mcp` + `mcp_tool` rows; we additionally
//! cancel pending approvals, optionally drop audit history, and purge OAuth
//! tokens from the keyring. Crucially we then **re-derive each affected client's
//! config file** from the surviving `managed_mcp` rows, so the deleted server's
//! entry disappears from every editor that was managing it — no denormalized
//! blob, no per-IDE branching: `resolve_client` + the one renderer.

use std::sync::Arc;

use crate::daemon::client_config::{render_scope, resolve_client, Scope};
use crate::daemon::db::types::ApprovalStatus;
use crate::daemon::db::Db;
use crate::daemon::errors::DbError;
use crate::daemon::events::{Bus, Event};
use crate::daemon::secrets;
use crate::daemon::upstream::manager::UpstreamManager;

#[derive(Debug, Default)]
pub struct CascadeReport {
    pub cancelled_approvals: Vec<String>,
    /// Client IDs whose config files were re-derived after the delete.
    pub rewritten_clients: Vec<String>,
}

pub async fn delete_server_with_cascade(
    db: &Db,
    bus: &Bus,
    upstream: &Arc<UpstreamManager>,
    server_name: &str,
    port: u16,
    delete_audit_history: bool,
) -> Result<CascadeReport, DbError> {
    // Snapshot affected (client_id, scope) BEFORE the delete wipes managed_mcp.
    let affected = db.list_clients_managing_server(server_name).await?;

    // Single DB transaction: delete managed_mcp rows, cancel approvals,
    // optionally delete audit, delete the server (mcp_tool cascades).
    let cancelled = db
        .commit_cascade_delete(server_name, delete_audit_history)
        .await?;
    for id in &cancelled {
        bus.publish(Event::ApprovalResolved {
            id: id.clone(),
            status: ApprovalStatus::Cancelled,
        });
    }

    if let Err(e) = secrets::delete_oauth_tokens(server_name) {
        tracing::warn!(server = %server_name, error = %e, "cascade: failed to delete OAuth tokens");
    }

    // Kill the upstream client so a deleted server's process stops immediately.
    upstream.kill(server_name).await;

    // Re-derive each affected (client, scope) from its SURVIVING managed servers.
    // De-dup on the (client, scope) PAIR — NOT the client id — so a server
    // managed at both global and a repo re-derives both files (§6). The renderer
    // rewrites each file without the deleted entry (and removes an emptied repo
    // file).
    let mut rewritten = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (client_id, scope) in affected {
        if !seen.insert((client_id.clone(), scope.clone())) {
            continue;
        }
        if let Err(e) = rewrite_client_scope(db, &client_id, &scope, port).await {
            tracing::warn!(client_id = %client_id, scope = %scope, error = %e, "cascade: config re-derive failed");
            continue;
        }
        if !rewritten.contains(&client_id) {
            rewritten.push(client_id);
        }
    }

    Ok(CascadeReport {
        cancelled_approvals: cancelled,
        rewritten_clients: rewritten,
    })
}

/// Rewrite one `(client, scope)`'s MCP config file from its remaining managed
/// servers. Custom clients are user-authored, so their files are left alone.
async fn rewrite_client_scope(
    db: &Db,
    client_id: &str,
    scope: &str,
    port: u16,
) -> Result<(), DbError> {
    let Some(client) = resolve_client(db, client_id).await? else {
        return Ok(());
    };
    if !client.is_builtin {
        return Ok(());
    }
    let remaining = db.list_managed_servers(client_id, scope).await?;
    let scope = Scope::from_db(scope);
    tokio::task::spawn_blocking(move || render_scope(&client, &scope, &remaining, port))
        .await
        .map_err(|e| DbError::Encoding(format!("join: {e}")))?
        .map_err(|e| DbError::Encoding(format!("render: {e}")))
}
