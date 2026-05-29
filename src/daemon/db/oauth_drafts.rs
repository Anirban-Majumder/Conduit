//! Tracking table for in-progress OAuth server drafts.
//!
//! `pending_oauth_drafts` records the UUID of each half-configured OAuth server
//! the Add-Server wizard has started but not yet committed, so abandoned drafts
//! can be reconciled (their keyring credential blobs cleaned up) on restart.

use super::{Db, DbCmd};
use rusqlite::{params, Connection};
use tokio::sync::oneshot;

use crate::daemon::errors::DbError;

pub(super) fn insert_oauth_draft(conn: &Connection, draft_uuid: &str) -> Result<(), DbError> {
    conn.prepare_cached("INSERT OR IGNORE INTO pending_oauth_drafts (draft_uuid) VALUES (?1)")?
        .execute(params![draft_uuid])?;
    Ok(())
}

pub(super) fn delete_oauth_draft(conn: &Connection, draft_uuid: &str) -> Result<(), DbError> {
    conn.prepare_cached("DELETE FROM pending_oauth_drafts WHERE draft_uuid = ?1")?
        .execute(params![draft_uuid])?;
    Ok(())
}

pub(super) fn list_oauth_drafts(conn: &Connection) -> Result<Vec<String>, DbError> {
    let mut stmt = conn.prepare_cached("SELECT draft_uuid FROM pending_oauth_drafts")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut drafts = Vec::new();
    for row in rows {
        drafts.push(row?);
    }
    Ok(drafts)
}

impl Db {
    pub async fn insert_oauth_draft(&self, draft_uuid: String) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::InsertOAuthDraft { draft_uuid, reply })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn delete_oauth_draft(&self, draft_uuid: String) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::DeleteOAuthDraft { draft_uuid, reply })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn list_oauth_drafts(&self) -> Result<Vec<String>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::ListOAuthDrafts { reply })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }
}
