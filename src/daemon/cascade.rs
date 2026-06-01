//! Server deletion with cascade.
//!
//! DB-side FK CASCADE removes `managed_mcp` + `mcp_tool` rows; we additionally
//! cancel pending approvals, optionally drop audit history, and purge OAuth
//! tokens from the keyring. Crucially we then **re-derive each affected client's
//! config file** from the surviving `managed_mcp` rows, so the deleted server's
//! entry disappears from every editor that was managing it — no denormalized
//! blob, no per-IDE branching: `resolve_client` + the one renderer.

use crate::daemon::clients::{render_mcp, resolve_client};
use crate::daemon::db::types::ApprovalStatus;
use crate::daemon::db::Db;
use crate::daemon::errors::DbError;
use crate::daemon::events::{Bus, Event};
use crate::daemon::secrets;

#[derive(Debug, Default)]
pub struct CascadeReport {
    pub cancelled_approvals: Vec<String>,
    /// Client IDs whose config files were re-derived after the delete.
    pub rewritten_clients: Vec<String>,
}

pub async fn delete_server_with_cascade(
    db: &Db,
    bus: &Bus,
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

    // Re-derive each affected client's config from its SURVIVING managed servers.
    // De-dup client ids (a client may appear once per scope; global scope only
    // for now). The renderer rewrites the file without the deleted entry.
    let mut rewritten = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (client_id, _scope) in affected {
        if !seen.insert(client_id.clone()) {
            continue;
        }
        if let Err(e) = rewrite_client_config(db, &client_id, port).await {
            tracing::warn!(client_id = %client_id, error = %e, "cascade: config re-derive failed");
            continue;
        }
        rewritten.push(client_id);
    }

    Ok(CascadeReport {
        cancelled_approvals: cancelled,
        rewritten_clients: rewritten,
    })
}

/// Rewrite one client's MCP config file from its remaining managed servers.
async fn rewrite_client_config(db: &Db, client_id: &str, port: u16) -> Result<(), DbError> {
    let Some(client) = resolve_client(db, client_id).await? else {
        return Ok(());
    };
    if !client.is_builtin {
        // Custom clients are user-authored; leave their files alone for now.
        return Ok(());
    }
    let remaining = db.list_managed_servers(client_id).await?;
    tokio::task::spawn_blocking(move || render_mcp(&client, &remaining, port))
        .await
        .map_err(|e| DbError::Encoding(format!("join: {e}")))?
        .map_err(|e| DbError::Encoding(format!("render: {e}")))
}
