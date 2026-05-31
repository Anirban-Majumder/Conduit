//! Per-tool and per-server approval policy storage and resolution.
//!
//! Rows in `policies` are keyed by `(server, tool, scope)`; `scope='tool'` is
//! the per-tool decision and `scope='server'` (with empty tool) is the
//! server-wide fallback. `Db::resolve_policy` walks the three-tier lookup
//! (tool → server → global `settings_kv` default) used by the request path.

use super::{Db, DbCmd};
use rusqlite::{params, Connection, OptionalExtension};

use super::now_unix;
use crate::daemon::db::types::{Decision, DecisionSource, Policy};
use crate::daemon::errors::DbError;

fn decode_policy_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Policy> {
    let decision_str: String = row.get(3)?;
    let source_str: String = row.get(4)?;
    let decision = Decision::from_db_str(&decision_str).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            format!("unknown decision: {decision_str}").into(),
        )
    })?;
    let decision_source = DecisionSource::from_db_str(&source_str).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            format!("unknown decision_source: {source_str}").into(),
        )
    })?;
    Ok(Policy {
        id: row.get(0)?,
        server: row.get(1)?,
        tool: row.get(2)?,
        decision,
        decision_source,
        created_at: row.get(5)?,
    })
}

pub(super) fn query_set_policy(
    conn: &Connection,
    server: &str,
    tool: &str,
    decision: Decision,
    decision_source: DecisionSource,
) -> Result<i64, DbError> {
    let now = now_unix();
    let id: i64 = conn
        .prepare_cached(
            "INSERT INTO policies (server, tool, scope, decision, decision_source, created_at)
             VALUES (?1, ?2, 'tool', ?3, ?4, ?5)
             ON CONFLICT(server, tool, scope) DO UPDATE SET
                 decision = excluded.decision,
                 decision_source = excluded.decision_source,
                 created_at = excluded.created_at
             RETURNING id",
        )?
        .query_row(
            params![
                server,
                tool,
                decision.as_db_str(),
                decision_source.as_db_str(),
                now
            ],
            |row| row.get(0),
        )?;
    Ok(id)
}

pub(super) fn query_get_policy(
    conn: &Connection,
    server: &str,
    tool: &str,
) -> Result<Option<Policy>, DbError> {
    let row = conn
        .prepare_cached(
            "SELECT id, server, tool, decision, decision_source, created_at
             FROM policies WHERE server = ?1 AND tool = ?2 AND scope = 'tool'",
        )?
        .query_row(params![server, tool], decode_policy_row)
        .optional()?;
    Ok(row)
}

pub(super) fn query_get_policy_scoped(
    conn: &Connection,
    server: &str,
    tool: &str,
    scope: &str,
) -> Result<Option<Policy>, DbError> {
    let row = conn
        .prepare_cached(
            "SELECT id, server, tool, decision, decision_source, created_at
             FROM policies WHERE server = ?1 AND tool = ?2 AND scope = ?3",
        )?
        .query_row(params![server, tool, scope], decode_policy_row)
        .optional()?;
    Ok(row)
}

pub(super) fn query_list_policies_for_server(
    conn: &Connection,
    server: &str,
) -> Result<Vec<Policy>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, server, tool, decision, decision_source, created_at
         FROM policies WHERE server = ?1 ORDER BY tool",
    )?;
    let rows = stmt.query_map(params![server], decode_policy_row)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub(super) fn query_list_all_policies(conn: &Connection) -> Result<Vec<Policy>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, server, tool, decision, decision_source, created_at
         FROM policies ORDER BY server, tool",
    )?;
    let rows = stmt.query_map([], decode_policy_row)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub(super) fn query_delete_policies_for_server(
    conn: &Connection,
    server: &str,
) -> Result<(), DbError> {
    conn.prepare_cached("DELETE FROM policies WHERE server = ?1")?
        .execute(params![server])?;
    Ok(())
}

impl Db {
    pub async fn set_policy(
        &self,
        server: &str,
        tool: &str,
        decision: Decision,
        decision_source: DecisionSource,
    ) -> Result<i64, DbError> {
        self.send_cmd(|reply| DbCmd::SetPolicy {
            server: server.to_string(),
            tool: tool.to_string(),
            decision,
            decision_source,
            reply,
        })
        .await
    }

    /// Three-tier lookup: (server, tool, 'tool') → (server, '', 'server') →
    /// `settings_kv.default_unknown_tool_policy`. Returns the first match.
    pub async fn resolve_policy(&self, server: &str, tool: &str) -> Result<Decision, DbError> {
        if let Some(p) = self.get_policy(server, tool).await? {
            return Ok(p.decision);
        }
        if let Some(p) = self.get_policy_scoped(server, "", "server").await? {
            return Ok(p.decision);
        }
        let global = self
            .get_settings_kv("default_unknown_tool_policy")
            .await?
            .as_deref()
            .and_then(Decision::from_db_str)
            .unwrap_or(Decision::Ask);
        Ok(global)
    }

    pub async fn get_policy_scoped(
        &self,
        server: &str,
        tool: &str,
        scope: &str,
    ) -> Result<Option<Policy>, DbError> {
        self.send_cmd(|reply| DbCmd::GetPolicyScoped {
            server: server.to_string(),
            tool: tool.to_string(),
            scope: scope.to_string(),
            reply,
        })
        .await
    }

    pub async fn get_policy(&self, server: &str, tool: &str) -> Result<Option<Policy>, DbError> {
        self.send_cmd(|reply| DbCmd::GetPolicy {
            server: server.to_string(),
            tool: tool.to_string(),
            reply,
        })
        .await
    }

    pub async fn list_policies_for_server(&self, server: &str) -> Result<Vec<Policy>, DbError> {
        self.send_cmd(|reply| DbCmd::ListPoliciesForServer {
            server: server.to_string(),
            reply,
        })
        .await
    }

    pub async fn list_all_policies(&self) -> Result<Vec<Policy>, DbError> {
        self.send_cmd(|reply| DbCmd::ListAllPolicies { reply })
            .await
    }

    pub async fn delete_policies_for_server(&self, server: &str) -> Result<(), DbError> {
        self.send_cmd(|reply| DbCmd::DeletePoliciesForServer {
            server: server.to_string(),
            reply,
        })
        .await
    }
}
