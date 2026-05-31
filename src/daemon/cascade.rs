//! Server deletion with cascade: DB-side FK CASCADE removes managed_mcp +
//! mcp_tool rows, cancels pending approvals, and optionally deletes audit
//! history. OAuth tokens are purged from the keyring.
//!
//! The old flow required per-IDE config file rewriting at cascade time.
//! That responsibility now lives in the config-writer subsystem, which will
//! be driven separately after cascade completes.

use crate::daemon::db::types::ApprovalStatus;
use crate::daemon::db::Db;
use crate::daemon::errors::DbError;
use crate::daemon::events::{Bus, Event};
use crate::daemon::secrets;

#[derive(Debug, Default)]
pub struct CascadeReport {
    pub cancelled_approvals: Vec<String>,
    /// Client IDs that had this server in their managed_mcp set (for
    /// downstream config-file rewriting).
    pub affected_client_ids: Vec<String>,
}

pub async fn delete_server_with_cascade(
    db: &Db,
    bus: &Bus,
    server_name: &str,
    _port: u16,
    delete_audit_history: bool,
) -> Result<CascadeReport, DbError> {
    // Snapshot which clients manage this server *before* the DB delete wipes
    // the managed_mcp rows.
    let all_managed = db.list_all_managed_mcp().await?;
    let affected_client_ids: Vec<String> = all_managed
        .into_iter()
        .filter(|(_cid, servers)| servers.iter().any(|s| s == server_name))
        .map(|(cid, _)| cid)
        .collect();

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

    Ok(CascadeReport {
        cancelled_approvals: cancelled,
        affected_client_ids,
    })
}
