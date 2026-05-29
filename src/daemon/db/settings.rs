//! Simple key/value access over the `settings_kv` table.
//!
//! A get/set string store for internal DB-resident settings (e.g. the global
//! `default_unknown_tool_policy` read by policy resolution). User-tunable
//! settings live in `config.toml`, not here.

use super::{Db, DbCmd};
use rusqlite::{params, Connection, OptionalExtension};
use tokio::sync::oneshot;

use crate::daemon::errors::DbError;

pub(super) fn query_set_settings_kv(
    conn: &Connection,
    key: &str,
    value: &str,
) -> Result<(), DbError> {
    conn.prepare_cached(
        "INSERT INTO settings_kv (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )?
    .execute(params![key, value])?;
    Ok(())
}

pub(super) fn query_get_settings_kv(
    conn: &Connection,
    key: &str,
) -> Result<Option<String>, DbError> {
    let row = conn
        .prepare_cached("SELECT value FROM settings_kv WHERE key = ?1")?
        .query_row(params![key], |row| row.get::<_, String>(0))
        .optional()?;
    Ok(row)
}

impl Db {
    pub async fn get_settings_kv(&self, key: &str) -> Result<Option<String>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::GetSettingsKv {
                key: key.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn set_settings_kv(&self, key: &str, value: &str) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::SetSettingsKv {
                key: key.to_string(),
                value: value.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }
}
