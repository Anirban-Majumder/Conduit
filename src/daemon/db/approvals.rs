//! Pending-approval queue storage.
//!
//! Rows in `pending_approvals` back the human-in-the-loop "ask" flow: created
//! when a request needs approval, resolved (approved/denied/cancelled) by the
//! UI, or swept to `timeout` once stale. `sweep_stale_pending_approvals`
//! returns the affected ids so the caller can publish per-id resolved events.

use super::{Db, DbCmd};
use rusqlite::{params, Connection, OptionalExtension};
use tokio::sync::oneshot;

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
        server: row.get(1)?,
        tool: row.get(2)?,
        args_json: row.get(3)?,
        ide_key: row.get(4)?,
        created_at: row.get(5)?,
        status,
    })
}

pub(super) fn query_create_pending_approval(
    conn: &Connection,
    approval: &PendingApproval,
) -> Result<(), DbError> {
    conn.prepare_cached(
        "INSERT INTO pending_approvals (id, server, tool, args_json, ide_key, created_at, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?
    .execute(params![
        &approval.id,
        &approval.server,
        &approval.tool,
        &approval.args_json,
        &approval.ide_key,
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
            "SELECT id, server, tool, args_json, ide_key, created_at, status
             FROM pending_approvals WHERE id = ?1",
        )?
        .query_row(params![id], decode_pending_approval_row)
        .optional()?;
    Ok(row)
}

pub(super) fn query_resolve_pending_approval(
    conn: &Connection,
    id: &str,
    new_status: ApprovalStatus,
) -> Result<(), DbError> {
    conn.prepare_cached("UPDATE pending_approvals SET status = ?1 WHERE id = ?2")?
        .execute(params![new_status.as_db_str(), id])?;
    Ok(())
}

pub(super) fn query_list_pending_approvals(
    conn: &Connection,
) -> Result<Vec<PendingApproval>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, server, tool, args_json, ide_key, created_at, status
         FROM pending_approvals WHERE status = 'pending' ORDER BY created_at DESC",
    )?;
    let rows = stmt.query_map([], decode_pending_approval_row)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

pub(super) fn query_sweep_stale_pending_approvals(
    conn: &Connection,
    older_than_ts: i64,
) -> Result<Vec<String>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT id FROM pending_approvals
         WHERE status = 'pending' AND created_at < ?1",
    )?;
    let ids: Vec<String> = stmt
        .query_map(params![older_than_ts], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    if ids.is_empty() {
        return Ok(ids);
    }
    // SQLite caps `?N` parameters at 999; pending-approval volume is well under
    // that bound, but loop one-id-at-a-time to keep the cached prepare reusable.
    let mut update =
        conn.prepare_cached("UPDATE pending_approvals SET status = 'timeout' WHERE id = ?1")?;
    for id in &ids {
        update.execute(params![id])?;
    }
    Ok(ids)
}

impl Db {
    pub async fn create_pending_approval(&self, approval: PendingApproval) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::CreatePendingApproval { approval, reply })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn get_pending_approval(&self, id: &str) -> Result<Option<PendingApproval>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::GetPendingApproval {
                id: id.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn resolve_pending_approval(
        &self,
        id: &str,
        new_status: ApprovalStatus,
    ) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::ResolvePendingApproval {
                id: id.to_string(),
                new_status,
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn list_pending_approvals(&self) -> Result<Vec<PendingApproval>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::ListPendingApprovals { reply })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    /// Flips every `pending` row older than `older_than_ts` to `timeout`.
    /// Returns the list of ids that were swept so the caller can publish
    /// per-id `Event::ApprovalResolved` events.
    pub async fn sweep_stale_pending_approvals(
        &self,
        older_than_ts: i64,
    ) -> Result<Vec<String>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::SweepStalePendingApprovals {
                older_than_ts,
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }
}
