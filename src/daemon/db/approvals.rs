//! Pending-approval queue storage.
//!
//! Rows in `pending_approvals` back the human-in-the-loop "ask" flow: created
//! when a request needs approval, resolved (approved/denied/cancelled) by the
//! UI, or swept to `timeout` once stale. `sweep_stale_pending_approvals`
//! returns the affected ids so the caller can publish per-id resolved events.

use super::{Db, DbCmd};
use rusqlite::{params, Connection, OptionalExtension};

use crate::daemon::db::types::{ApprovalStatus, PendingApproval};
use crate::daemon::errors::DbError;

fn decode_pending_approval_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PendingApproval> {
    let status_str: String = row.get(6)?;
    let status = ApprovalStatus::from_db_str(&status_str).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            6,
            rusqlite::types::Type::Text,
            format!("unknown approval status: {status_str}").into(),
        )
    })?;
    Ok(PendingApproval {
        id: row.get(0)?,
        mcp_server: row.get(1)?,
        tool: row.get(2)?,
        args_json: row.get(3)?,
        client_key: row.get(4)?,
        created_at: row.get(5)?,
        status,
    })
}

pub(super) fn query_create_pending_approval(
    conn: &Connection,
    approval: &PendingApproval,
) -> Result<(), DbError> {
    conn.prepare_cached(
        "INSERT INTO pending_approvals (id, mcp_server, tool, args_json, client_key, created_at, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?
    .execute(params![
        &approval.id,
        &approval.mcp_server,
        &approval.tool,
        &approval.args_json,
        &approval.client_key,
        approval.created_at,
        approval.status.as_db_str(),
    ])?;
    Ok(())
}

pub(super) fn query_get_pending_approval(
    conn: &Connection,
    id: &str,
) -> Result<Option<PendingApproval>, DbError> {
    let row = conn
        .prepare_cached(
            "SELECT id, mcp_server, tool, args_json, client_key, created_at, status
             FROM pending_approvals WHERE id = ?1",
        )?
        .query_row(params![id], decode_pending_approval_row)
        .optional()?;
    Ok(row)
}

/// Conditionally flips a `pending` row to `new_status`.
///
/// Returns 1 if the row existed and was `pending` (this caller wins the race),
/// 0 if it was already resolved by a concurrent caller or never existed.
pub(super) fn query_resolve_pending_approval(
    conn: &Connection,
    id: &str,
    new_status: ApprovalStatus,
) -> Result<usize, DbError> {
    let n = conn
        .prepare_cached(
            "UPDATE pending_approvals SET status = ?1 WHERE id = ?2 AND status = 'pending'",
        )?
        .execute(params![new_status.as_db_str(), id])?;
    Ok(n)
}

pub(super) fn query_list_pending_approvals(
    conn: &Connection,
) -> Result<Vec<PendingApproval>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, mcp_server, tool, args_json, client_key, created_at, status
         FROM pending_approvals WHERE status = 'pending' ORDER BY created_at DESC",
    )?;
    let rows = stmt.query_map([], decode_pending_approval_row)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub(super) fn query_sweep_stale_pending_approvals(
    conn: &Connection,
    older_than_ts: i64,
) -> Result<Vec<String>, DbError> {
    let mut stmt = conn.prepare_cached(
        "UPDATE pending_approvals SET status = 'timeout'
         WHERE status = 'pending' AND created_at < ?1
         RETURNING id",
    )?;
    let ids = stmt
        .query_map(params![older_than_ts], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ids)
}

impl Db {
    pub async fn create_pending_approval(&self, approval: PendingApproval) -> Result<(), DbError> {
        self.send_cmd(|reply| DbCmd::CreatePendingApproval { approval, reply })
            .await
    }

    pub async fn get_pending_approval(&self, id: &str) -> Result<Option<PendingApproval>, DbError> {
        self.send_cmd(|reply| DbCmd::GetPendingApproval {
            id: id.to_string(),
            reply,
        })
        .await
    }

    /// Returns 1 if this caller won the race (row was pending), 0 otherwise.
    pub async fn resolve_pending_approval(
        &self,
        id: &str,
        new_status: ApprovalStatus,
    ) -> Result<usize, DbError> {
        self.send_cmd(|reply| DbCmd::ResolvePendingApproval {
            id: id.to_string(),
            new_status,
            reply,
        })
        .await
    }

    pub async fn list_pending_approvals(&self) -> Result<Vec<PendingApproval>, DbError> {
        self.send_cmd(|reply| DbCmd::ListPendingApprovals { reply })
            .await
    }

    /// Flips every `pending` row older than `older_than_ts` to `timeout`.
    /// Returns the list of ids that were swept so the caller can publish
    /// per-id `Event::ApprovalResolved` events.
    pub async fn sweep_stale_pending_approvals(
        &self,
        older_than_ts: i64,
    ) -> Result<Vec<String>, DbError> {
        self.send_cmd(|reply| DbCmd::SweepStalePendingApprovals {
            older_than_ts,
            reply,
        })
        .await
    }
}
