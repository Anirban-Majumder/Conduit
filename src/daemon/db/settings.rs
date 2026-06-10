//! Simple key/value access over the `settings_kv` table.
//!
//! A get/set string store for runtime state only (`routing_paused`,
//! `has_completed_first_run`, `shown_background_hint`, `client_def_version`,
//! `skill_repos_seeded`). User-tunable settings live in `config.toml`, not here.

use super::{Db, DbCmd};
use rusqlite::{params, Connection, OptionalExtension};

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
        self.send_cmd(|reply| DbCmd::GetSettingsKv {
            key: key.to_string(),
            reply,
        })
        .await
    }

    pub async fn set_settings_kv(&self, key: &str, value: &str) -> Result<(), DbError> {
        self.send_cmd(|reply| DbCmd::SetSettingsKv {
            key: key.to_string(),
            value: value.to_string(),
            reply,
        })
        .await
    }
}
