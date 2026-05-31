//! Detected-IDE registry and per-IDE managed-config tracking.
//!
//! The `ides` table holds one row per known IDE kind: its generated `ide_key`,
//! lifecycle status (active/stale/`not_tracked`), and the config path plus
//! managed-entry list this daemon wrote. Used by detection (`upsert_detected`,
//! `mark_stale`), the routing key lookup, and the IDE-config writer.

use super::{Db, DbCmd};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};

use super::{bind_in_clause, now_unix};
use crate::daemon::db::types::{fresh_ide_key, IdeKey, IdeRecord, IdeStatus, ManagedIdeConfig};
use crate::daemon::errors::DbError;

const IDE_SELECT_COLUMNS: &str =
    "ide_kind, display_name, ide_key, status, detection_source, config_path,
     managed_entries_json, first_seen_at, last_seen_at, not_tracked_at";

pub(super) fn query_list_all_ide_configs(
    conn: &Connection,
) -> Result<Vec<ManagedIdeConfig>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT ide_kind, ide_key, config_path, managed_entries_json, last_seen_at
         FROM ides
         WHERE config_path IS NOT NULL AND status != 'not_tracked'
         ORDER BY ide_kind",
    )?;
    let rows = stmt.query_map([], |row| {
        let path_str: String = row.get(2)?;
        let entries_json: String = row.get(3)?;
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            PathBuf::from(path_str),
            entries_json,
            row.get::<_, i64>(4)?,
        ))
    })?;
    let mut out = Vec::new();
    for r in rows {
        let (ide_kind, ide_key, config_path, entries_json, last_seen_at) = r?;
        let managed_entries: Vec<String> =
            serde_json::from_str(&entries_json).map_err(|e| DbError::Encoding(e.to_string()))?;
        out.push(ManagedIdeConfig {
            ide_name: ide_kind,
            ide_key,
            config_path,
            managed_entries,
            written_at: last_seen_at,
        });
    }
    Ok(out)
}

pub(super) fn query_lookup_ide_key(
    conn: &Connection,
    key: &str,
) -> Result<Option<IdeKey>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT ide_key, display_name, last_seen_at FROM ides
         WHERE ide_key = ?1 AND status != 'not_tracked'",
    )?;
    let row = stmt
        .query_row(params![key], |row| {
            Ok(IdeKey {
                key: row.get(0)?,
                ide_name: row.get(1)?,
                created_at: row.get(2)?,
            })
        })
        .optional()?;
    Ok(row)
}

fn decode_ide_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<IdeRecord> {
    let status_str: String = row.get(3)?;
    let status = IdeStatus::from_db_str(&status_str).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            format!("unknown ide status: {status_str}").into(),
        )
    })?;
    let entries_json: String = row.get(6)?;
    let managed_entries: Vec<String> = serde_json::from_str(&entries_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            6,
            rusqlite::types::Type::Text,
            format!("decode managed_entries_json: {e}").into(),
        )
    })?;
    let config_path: Option<String> = row.get(5)?;
    Ok(IdeRecord {
        ide_kind: row.get(0)?,
        display_name: row.get(1)?,
        ide_key: row.get(2)?,
        status,
        detection_source: row.get(4)?,
        config_path: config_path.map(PathBuf::from),
        managed_entries,
        first_seen_at: row.get(7)?,
        last_seen_at: row.get(8)?,
        not_tracked_at: row.get(9)?,
    })
}

pub(super) fn query_list_ides(conn: &Connection) -> Result<Vec<IdeRecord>, DbError> {
    let sql = format!("SELECT {IDE_SELECT_COLUMNS} FROM ides ORDER BY display_name");
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map([], decode_ide_record)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub(super) fn query_list_active_ides(conn: &Connection) -> Result<Vec<IdeRecord>, DbError> {
    let sql = format!(
        "SELECT {IDE_SELECT_COLUMNS} FROM ides WHERE status = 'active' ORDER BY display_name"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map([], decode_ide_record)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub(super) fn query_get_ide_by_kind(
    conn: &Connection,
    ide_kind: &str,
) -> Result<Option<IdeRecord>, DbError> {
    let sql = format!("SELECT {IDE_SELECT_COLUMNS} FROM ides WHERE ide_kind = ?1");
    let row = conn
        .prepare_cached(&sql)?
        .query_row(params![ide_kind], decode_ide_record)
        .optional()?;
    Ok(row)
}

pub(super) fn query_upsert_detected_ide(
    conn: &Connection,
    ide_kind: &str,
    display_name: &str,
    detection_source: Option<&str>,
) -> Result<IdeRecord, DbError> {
    let now = now_unix();
    let tx = conn.unchecked_transaction()?;
    let existing = {
        let sql = format!("SELECT {IDE_SELECT_COLUMNS} FROM ides WHERE ide_kind = ?1");
        tx.prepare_cached(&sql)?
            .query_row(params![ide_kind], decode_ide_record)
            .optional()?
    };

    if let Some(mut row) = existing {
        row.last_seen_at = now;
        row.display_name = display_name.to_string();
        row.detection_source = detection_source.map(str::to_string);
        if matches!(row.status, IdeStatus::Stale) {
            row.status = IdeStatus::Active;
        }
        tx.prepare_cached(
            "UPDATE ides SET display_name = ?1, detection_source = ?2,
                             last_seen_at = ?3,
                             status = CASE WHEN status = 'stale' THEN 'active' ELSE status END
             WHERE ide_kind = ?4",
        )?
        .execute(params![display_name, detection_source, now, ide_kind])?;
        tx.commit()?;
        return Ok(row);
    }

    let key = fresh_ide_key();
    tx.prepare_cached(
        "INSERT INTO ides
            (ide_kind, display_name, ide_key, status, detection_source,
             config_path, managed_entries_json, first_seen_at, last_seen_at, not_tracked_at)
         VALUES (?1, ?2, ?3, 'active', ?4, NULL, '[]', ?5, ?5, NULL)",
    )?
    .execute(params![ide_kind, display_name, key, detection_source, now])?;
    tx.commit()?;
    Ok(IdeRecord {
        ide_kind: ide_kind.to_string(),
        display_name: display_name.to_string(),
        ide_key: key,
        status: IdeStatus::Active,
        detection_source: detection_source.map(str::to_string),
        config_path: None,
        managed_entries: Vec::new(),
        first_seen_at: now,
        last_seen_at: now,
        not_tracked_at: None,
    })
}

pub(super) fn query_mark_ides_stale(
    conn: &Connection,
    kinds_present: &[String],
) -> Result<(), DbError> {
    let (placeholders, params) = bind_in_clause(kinds_present);
    let sql = format!(
        "UPDATE ides SET status = 'stale'
         WHERE status = 'active' AND ide_kind NOT IN ({placeholders})"
    );
    conn.prepare(&sql)?.execute(&params[..])?;
    Ok(())
}

pub(super) fn query_mark_ide_not_tracked(conn: &Connection, ide_kind: &str) -> Result<(), DbError> {
    let now = now_unix();
    conn.prepare_cached(
        "UPDATE ides SET status = 'not_tracked', config_path = NULL,
                         managed_entries_json = '[]', not_tracked_at = ?1
         WHERE ide_kind = ?2",
    )?
    .execute(params![now, ide_kind])?;
    Ok(())
}

pub(super) fn query_retrack_ide(conn: &Connection, ide_kind: &str) -> Result<(), DbError> {
    let now = now_unix();
    conn.prepare_cached(
        "UPDATE ides SET status = 'active', not_tracked_at = NULL, last_seen_at = ?1
         WHERE ide_kind = ?2 AND status = 'not_tracked'",
    )?
    .execute(params![now, ide_kind])?;
    Ok(())
}

pub(super) fn query_update_ide_managed_entries(
    conn: &Connection,
    ide_kind: &str,
    config_path: &Path,
    managed_entries: &[String],
) -> Result<(), DbError> {
    let json =
        serde_json::to_string(managed_entries).map_err(|e| DbError::Encoding(e.to_string()))?;
    let path_str = config_path
        .to_str()
        .ok_or_else(|| DbError::Encoding("non-utf8 config_path".to_string()))?;
    let now = now_unix();
    conn.prepare_cached(
        "UPDATE ides SET config_path = ?1, managed_entries_json = ?2, last_seen_at = ?3
         WHERE ide_kind = ?4",
    )?
    .execute(params![path_str, json, now, ide_kind])?;
    Ok(())
}

impl Db {
    pub async fn lookup_ide_key(&self, key: &str) -> Result<Option<IdeKey>, DbError> {
        self.send_cmd(|reply| DbCmd::LookupIdeKey { key: key.to_string(), reply }).await
    }

    pub async fn upsert_detected_ide(
        &self,
        ide_kind: &str,
        display_name: &str,
        detection_source: Option<String>,
    ) -> Result<IdeRecord, DbError> {
        self.send_cmd(|reply| DbCmd::UpsertDetectedIde {
            ide_kind: ide_kind.to_string(),
            display_name: display_name.to_string(),
            detection_source,
            reply,
        })
        .await
    }

    pub async fn list_ides(&self) -> Result<Vec<IdeRecord>, DbError> {
        self.send_cmd(|reply| DbCmd::ListIdes { reply }).await
    }

    pub async fn list_active_ides(&self) -> Result<Vec<IdeRecord>, DbError> {
        self.send_cmd(|reply| DbCmd::ListActiveIdes { reply }).await
    }

    pub async fn get_ide_by_kind(&self, ide_kind: &str) -> Result<Option<IdeRecord>, DbError> {
        self.send_cmd(|reply| DbCmd::GetIdeByKind { ide_kind: ide_kind.to_string(), reply }).await
    }

    pub async fn mark_ides_stale(&self, kinds_present: Vec<String>) -> Result<(), DbError> {
        self.send_cmd(|reply| DbCmd::MarkIdesStale { kinds_present, reply }).await
    }

    pub async fn mark_ide_not_tracked(&self, ide_kind: &str) -> Result<(), DbError> {
        self.send_cmd(|reply| DbCmd::MarkIdeNotTracked { ide_kind: ide_kind.to_string(), reply })
            .await
    }

    pub async fn retrack_ide(&self, ide_kind: &str) -> Result<(), DbError> {
        self.send_cmd(|reply| DbCmd::RetrackIde { ide_kind: ide_kind.to_string(), reply }).await
    }

    pub async fn update_ide_managed_entries(
        &self,
        ide_kind: &str,
        config_path: PathBuf,
        managed_entries: Vec<String>,
    ) -> Result<(), DbError> {
        self.send_cmd(|reply| DbCmd::UpdateIdeManagedEntries {
            ide_kind: ide_kind.to_string(),
            config_path,
            managed_entries,
            reply,
        })
        .await
    }

    pub async fn list_all_ide_configs(&self) -> Result<Vec<ManagedIdeConfig>, DbError> {
        self.send_cmd(|reply| DbCmd::ListAllIdeConfigs { reply }).await
    }
}
