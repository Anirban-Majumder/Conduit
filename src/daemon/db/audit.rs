//! Audit-log table access (the `audit_log` table inside `state.db`).
//!
//! Append-only inserts of every request decision, a filtered/paginated query
//! builder backing the Audit tab, and a retention purge by timestamp. The
//! audit log lives as a table here, not a separate `audit.db`.

use super::{Db, DbCmd};
use rusqlite::{params, params_from_iter, types::Value as SqlValue, Connection};

use crate::daemon::db::types::{AuditDecision, AuditEntry, AuditFilter, DecisionSource};
use crate::daemon::errors::DbError;

fn decode_audit_entry_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AuditEntry> {
    let decision_str: String = row.get(6)?;
    let decision = AuditDecision::from_db_str(&decision_str).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            6,
            rusqlite::types::Type::Text,
            format!("unknown audit decision: {decision_str}").into(),
        )
    })?;
    let decision_source = row
        .get::<_, Option<String>>(7)?
        .map(|s| {
            DecisionSource::from_db_str(&s).ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    7,
                    rusqlite::types::Type::Text,
                    format!("unknown decision_source: {s}").into(),
                )
            })
        })
        .transpose()?;
    Ok(AuditEntry {
        id: Some(row.get(0)?),
        ts: row.get(1)?,
        ide_key: row.get(2)?,
        ide_name: row.get(3)?,
        server: row.get(4)?,
        tool: row.get(5)?,
        decision,
        decision_source,
        latency_ms: row.get(8)?,
        request_args: row.get(9)?,
        response_status: row.get(10)?,
        response_length: row.get(11)?,
        error_code: row.get(12)?,
    })
}

pub(super) fn query_insert_audit_entry(
    conn: &Connection,
    entry: &AuditEntry,
) -> Result<i64, DbError> {
    let id: i64 = conn
        .prepare_cached(
            "INSERT INTO audit_log
             (ts, ide_key, ide_name, server, tool, decision, decision_source,
              latency_ms, request_args, response_status, response_length, error_code)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             RETURNING id",
        )?
        .query_row(
            params![
                entry.ts,
                &entry.ide_key,
                &entry.ide_name,
                &entry.server,
                &entry.tool,
                entry.decision.as_db_str(),
                entry.decision_source.map(DecisionSource::as_db_str),
                entry.latency_ms,
                &entry.request_args,
                &entry.response_status,
                entry.response_length,
                &entry.error_code,
            ],
            |row| row.get(0),
        )?;
    Ok(id)
}

pub(super) fn query_query_audit_log(
    conn: &Connection,
    filter: &AuditFilter,
) -> Result<Vec<AuditEntry>, DbError> {
    let mut sql = String::from(
        "SELECT id, ts, ide_key, ide_name, server, tool, decision, decision_source,
                latency_ms, request_args, response_status, response_length, error_code
         FROM audit_log WHERE 1=1",
    );
    let mut args: Vec<SqlValue> = Vec::new();

    if let Some(k) = &filter.ide_key {
        sql.push_str(" AND ide_key = ?");
        args.push(SqlValue::Text(k.clone()));
    }
    if let Some(s) = &filter.server {
        sql.push_str(" AND server = ?");
        args.push(SqlValue::Text(s.clone()));
    }
    if let Some(t) = &filter.tool {
        sql.push_str(" AND tool = ?");
        args.push(SqlValue::Text(t.clone()));
    }
    if let Some(ts) = filter.since_ts {
        sql.push_str(" AND ts >= ?");
        args.push(SqlValue::Integer(ts));
    }
    if let Some(ts) = filter.until_ts {
        sql.push_str(" AND ts < ?");
        args.push(SqlValue::Integer(ts));
    }
    if let Some(d) = &filter.decision {
        sql.push_str(" AND decision = ?");
        args.push(SqlValue::Text(d.as_db_str().to_owned()));
    }
    if let Some(t) = &filter.text {
        // Escape LIKE metacharacters so a literal '%' or '_' doesn't act as a
        // wildcard. ESCAPE '\' tells SQLite our escape prefix.
        let escaped = t.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
        sql.push_str(" AND (server LIKE ? ESCAPE '\\' OR tool LIKE ? ESCAPE '\\')");
        let pattern = format!("%{escaped}%");
        args.push(SqlValue::Text(pattern.clone()));
        args.push(SqlValue::Text(pattern));
    }
    sql.push_str(" ORDER BY ts DESC");
    if let Some(limit) = filter.limit {
        sql.push_str(" LIMIT ?");
        args.push(SqlValue::Integer(i64::from(limit)));
    }
    if let Some(offset) = filter.offset {
        sql.push_str(" OFFSET ?");
        args.push(SqlValue::Integer(i64::from(offset)));
    }

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(args.iter()), decode_audit_entry_row)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub(super) fn query_purge_old_audit_entries(
    conn: &Connection,
    older_than_ts: i64,
) -> Result<u64, DbError> {
    let n = conn
        .prepare_cached("DELETE FROM audit_log WHERE ts < ?1")?
        .execute(params![older_than_ts])?;
    Ok(n as u64)
}

impl Db {
    pub async fn insert_audit_entry(&self, entry: AuditEntry) -> Result<i64, DbError> {
        self.send_cmd(|reply| DbCmd::InsertAuditEntry { entry, reply }).await
    }

    pub async fn query_audit_log(&self, filter: AuditFilter) -> Result<Vec<AuditEntry>, DbError> {
        self.send_cmd(|reply| DbCmd::QueryAuditLog { filter, reply }).await
    }

    pub async fn purge_old_audit_entries(&self, older_than_ts: i64) -> Result<u64, DbError> {
        self.send_cmd(|reply| DbCmd::PurgeOldAuditEntries { older_than_ts, reply }).await
    }
}
