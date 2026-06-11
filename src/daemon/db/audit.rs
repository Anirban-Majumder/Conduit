//! Audit-log table access (the `audit_log` table inside `state.db`).
//!
//! Append-only inserts of every request decision, a filtered/paginated query
//! builder backing the Audit tab, and a retention purge by timestamp. The
//! audit log lives as a table here, not a separate `audit.db`.

use super::{Db, DbCmd};
use rusqlite::{params, params_from_iter, types::Value as SqlValue, Connection};

use crate::daemon::db::types::{
    AuditCounts, AuditDecision, AuditEntry, AuditFilter, DecisionSource,
};
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
        client_key: row.get(2)?,
        client_name: row.get(3)?,
        mcp_server: row.get(4)?,
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
             (ts, client_key, client_name, mcp_server, tool, decision, decision_source,
              latency_ms, request_args, response_status, response_length, error_code)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             RETURNING id",
        )?
        .query_row(
            params![
                entry.ts,
                &entry.client_key,
                &entry.client_name,
                &entry.mcp_server,
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
        "SELECT id, ts, client_key, client_name, mcp_server, tool, decision, decision_source,
                latency_ms, request_args, response_status, response_length, error_code
         FROM audit_log WHERE 1=1",
    );
    let mut args: Vec<SqlValue> = Vec::new();

    if let Some(k) = &filter.client_key {
        sql.push_str(" AND client_key = ?");
        args.push(SqlValue::Text(k.clone()));
    }
    if let Some(s) = &filter.mcp_server {
        sql.push_str(" AND mcp_server = ?");
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
        let escaped = t
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        sql.push_str(" AND (mcp_server LIKE ? ESCAPE '\\' OR tool LIKE ? ESCAPE '\\')");
        let pattern = format!("%{escaped}%");
        args.push(SqlValue::Text(pattern.clone()));
        args.push(SqlValue::Text(pattern));
    }
    sql.push_str(" ORDER BY ts DESC");
    if let Some(limit) = filter.limit {
        sql.push_str(" LIMIT ?");
        args.push(SqlValue::Integer(i64::from(limit)));
    } else if filter.offset.is_some() {
        // SQLite requires LIMIT before OFFSET; -1 means "no limit".
        sql.push_str(" LIMIT -1");
    }
    if let Some(offset) = filter.offset {
        sql.push_str(" OFFSET ?");
        args.push(SqlValue::Integer(i64::from(offset)));
    }

    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(params_from_iter(args.iter()), decode_audit_entry_row)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Count audit entries matching `filter` grouped by decision; uses the
/// `idx_audit_log_ts` index via `WHERE ts >= ?` and avoids fetching full rows.
pub(super) fn query_count_audit_entries(
    conn: &Connection,
    filter: &AuditFilter,
) -> Result<AuditCounts, DbError> {
    let mut sql = String::from("SELECT decision, COUNT(*) FROM audit_log WHERE 1=1");
    let mut args: Vec<SqlValue> = Vec::new();

    if let Some(ts) = filter.since_ts {
        sql.push_str(" AND ts >= ?");
        args.push(SqlValue::Integer(ts));
    }
    if let Some(ts) = filter.until_ts {
        sql.push_str(" AND ts < ?");
        args.push(SqlValue::Integer(ts));
    }
    if let Some(k) = &filter.client_key {
        sql.push_str(" AND client_key = ?");
        args.push(SqlValue::Text(k.clone()));
    }
    if let Some(s) = &filter.mcp_server {
        sql.push_str(" AND mcp_server = ?");
        args.push(SqlValue::Text(s.clone()));
    }
    if let Some(t) = &filter.tool {
        sql.push_str(" AND tool = ?");
        args.push(SqlValue::Text(t.clone()));
    }
    if let Some(d) = &filter.decision {
        sql.push_str(" AND decision = ?");
        args.push(SqlValue::Text(d.as_db_str().to_owned()));
    }
    if let Some(t) = &filter.text {
        let escaped = t
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        sql.push_str(" AND (mcp_server LIKE ? ESCAPE '\\' OR tool LIKE ? ESCAPE '\\')");
        let pattern = format!("%{escaped}%");
        args.push(SqlValue::Text(pattern.clone()));
        args.push(SqlValue::Text(pattern));
    }
    sql.push_str(" GROUP BY decision");

    let mut stmt = conn.prepare_cached(&sql)?;
    let mut counts = AuditCounts::default();
    let rows = stmt.query_map(params_from_iter(args.iter()), |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (decision_str, n) = row?;
        let n = n.cast_unsigned();
        counts.total += n;
        // Denied = explicit deny, user-denied ask, AND timed-out ask — a
        // timeout denies the call (matches the old dashboard computation).
        if matches!(
            AuditDecision::from_db_str(&decision_str),
            Some(AuditDecision::Deny | AuditDecision::AskDenied | AuditDecision::AskTimeout)
        ) {
            counts.denied += n;
        }
    }
    Ok(counts)
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
        self.send_cmd(|reply| DbCmd::InsertAuditEntry { entry, reply })
            .await
    }

    pub async fn query_audit_log(&self, filter: AuditFilter) -> Result<Vec<AuditEntry>, DbError> {
        self.send_cmd(|reply| DbCmd::QueryAuditLog { filter, reply })
            .await
    }

    pub async fn purge_old_audit_entries(&self, older_than_ts: i64) -> Result<u64, DbError> {
        self.send_cmd(|reply| DbCmd::PurgeOldAuditEntries {
            older_than_ts,
            reply,
        })
        .await
    }

    /// Count audit entries matching `filter` without fetching full rows.
    /// Returns totals suitable for a dashboard summary.
    pub async fn count_audit_entries(&self, filter: AuditFilter) -> Result<AuditCounts, DbError> {
        self.send_cmd(|reply| DbCmd::CountAuditEntries { filter, reply })
            .await
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, reason = "test ergonomics")]

    use super::*;
    use crate::daemon::db::migrations::init_conn;
    use crate::daemon::db::types::{AuditDecision, AuditEntry, AuditFilter};
    use rusqlite::Connection;

    fn make_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_conn(&conn).unwrap();
        conn
    }

    fn insert_entry(conn: &Connection, decision: AuditDecision, ts: i64) {
        let entry = AuditEntry {
            id: None,
            ts,
            client_key: None,
            client_name: None,
            mcp_server: "srv".to_string(),
            tool: Some("tool".to_string()),
            decision,
            decision_source: None,
            latency_ms: None,
            request_args: None,
            response_status: None,
            response_length: None,
            error_code: None,
        };
        query_insert_audit_entry(conn, &entry).unwrap();
    }

    /// DB-4: offset-without-limit must not produce bare OFFSET (invalid SQL).
    #[test]
    fn offset_without_limit_returns_rows() {
        let conn = make_conn();
        insert_entry(&conn, AuditDecision::Allow, 100);
        insert_entry(&conn, AuditDecision::Allow, 200);
        insert_entry(&conn, AuditDecision::Allow, 300);

        // With limit: should skip first (highest-ts) entry and return 2.
        let with_limit = query_query_audit_log(
            &conn,
            &AuditFilter {
                limit: Some(10),
                offset: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(with_limit.len(), 2);

        // Without limit: LIMIT -1 OFFSET 1 — skip first row, return 2 remaining.
        let without_limit = query_query_audit_log(
            &conn,
            &AuditFilter {
                limit: None,
                offset: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(without_limit.len(), 2);
    }

    /// DB-7: count matches a known row set.
    #[test]
    fn count_audit_entries_matches_known_set() {
        let conn = make_conn();
        insert_entry(&conn, AuditDecision::Allow, 1000);
        insert_entry(&conn, AuditDecision::Deny, 1001);
        insert_entry(&conn, AuditDecision::Deny, 1002);
        insert_entry(&conn, AuditDecision::AskDenied, 1003);
        insert_entry(&conn, AuditDecision::AskTimeout, 1004);

        let counts = query_count_audit_entries(&conn, &AuditFilter::default()).unwrap();
        assert_eq!(counts.total, 5);
        // Deny + AskDenied + AskTimeout = 4 denied
        assert_eq!(counts.denied, 4);

        // Filter by since_ts excludes first two rows.
        let counts_filtered = query_count_audit_entries(
            &conn,
            &AuditFilter {
                since_ts: Some(1002),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(counts_filtered.total, 3);
        // Deny at ts=1002, AskDenied at ts=1003, AskTimeout at ts=1004
        assert_eq!(counts_filtered.denied, 3);
    }
}
