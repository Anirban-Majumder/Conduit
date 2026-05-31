//! MCP server + tool CRUD and related multi-table operations.
//!
//! Covers `mcp_server` upserts/reads, the atomic add-server-with-tools write
//! (server row + `mcp_tool` rows with default decisions), rename (FK-cascaded),
//! auth-error transition tracking, and `commit_cascade_delete` which removes a
//! server while trimming client entries and cancelling its pending approvals.

use super::{bind_in_clause, now_unix, Db, DbCmd};
use rusqlite::{params, Connection, OptionalExtension};

use crate::daemon::db::types::{Decision, DecisionSource, EnvEntry, McpServer, McpTool};
use crate::daemon::errors::DbError;

const SERVERS_SELECT: &str = "SELECT name, transport, command, args_json, env_json, url,
       lifecycle_seconds, reported_name, reported_version,
       last_probed_at, last_probe_error, last_auth_error_msg
 FROM mcp_server";

pub(super) fn query_upsert_server(conn: &Connection, server: &McpServer) -> Result<(), DbError> {
    let args_json =
        serde_json::to_string(&server.args).map_err(|e| DbError::Encoding(e.to_string()))?;
    let env_json =
        serde_json::to_string(&server.env).map_err(|e| DbError::Encoding(e.to_string()))?;
    conn.prepare_cached(
        "INSERT INTO mcp_server
         (name, transport, command, args_json, env_json, url, lifecycle_seconds,
          reported_name, reported_version, last_probed_at, last_probe_error)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(name) DO UPDATE SET
             transport = excluded.transport,
             command = excluded.command,
             args_json = excluded.args_json,
             env_json = excluded.env_json,
             url = excluded.url,
             lifecycle_seconds = excluded.lifecycle_seconds,
             reported_name = excluded.reported_name,
             reported_version = excluded.reported_version,
             last_probed_at = excluded.last_probed_at,
             last_probe_error = excluded.last_probe_error",
    )?
    .execute(params![
        &server.name,
        &server.transport,
        &server.command,
        args_json,
        env_json,
        &server.url,
        server.lifecycle_seconds,
        &server.reported_name,
        &server.reported_version,
        server.last_probed_at,
        &server.last_probe_error,
    ])?;
    Ok(())
}

pub(super) fn query_get_server(
    conn: &Connection,
    name: &str,
) -> Result<Option<McpServer>, DbError> {
    let sql = format!("{SERVERS_SELECT} WHERE name = ?1");
    let row = conn
        .prepare_cached(&sql)?
        .query_row(params![name], decode_server_row)
        .optional()?;
    Ok(row)
}

pub(super) fn query_list_servers(conn: &Connection) -> Result<Vec<McpServer>, DbError> {
    let sql = format!("{SERVERS_SELECT} ORDER BY name");
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map([], decode_server_row)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub(super) fn query_delete_server(conn: &Connection, name: &str) -> Result<(), DbError> {
    conn.prepare_cached("DELETE FROM mcp_server WHERE name = ?1")?
        .execute(params![name])?;
    Ok(())
}

pub(super) fn query_rename_server(conn: &Connection, old: &str, new: &str) -> Result<(), DbError> {
    conn.prepare_cached("UPDATE mcp_server SET name = ?1 WHERE name = ?2")?
        .execute(params![new, old])?;
    Ok(())
}

/// Atomic save: upsert server + sync mcp_tool rows to the probed set.
///
/// For each tool, if no existing decision is set, defaults to `ask`. Tools that
/// vanished from the probe are deleted (CASCADE removes them from mcp_tool).
pub(super) fn query_upsert_server_with_tools(
    conn: &Connection,
    server: &McpServer,
    tools: &[McpTool],
) -> Result<(), DbError> {
    let tx = conn.unchecked_transaction()?;
    query_upsert_server(&tx, server)?;

    // Sync mcp_tool to exactly the probed set.
    let tool_names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    if tool_names.is_empty() {
        tx.prepare_cached("DELETE FROM mcp_tool WHERE mcp_server = ?1")?
            .execute(params![&server.name])?;
    } else {
        let (placeholders, tool_params) = bind_in_clause(&tool_names);
        let del_sql =
            format!("DELETE FROM mcp_tool WHERE mcp_server = ? AND name NOT IN ({placeholders})");
        let mut params_vec: Vec<&dyn rusqlite::ToSql> = vec![&server.name];
        params_vec.extend(tool_params);
        tx.prepare(&del_sql)?.execute(&params_vec[..])?;
    }

    for tool in tools {
        tx.prepare_cached(
            "INSERT INTO mcp_tool (mcp_server, name, description, input_schema, discovered_at, decision)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(mcp_server, name) DO UPDATE SET
                 description = excluded.description,
                 input_schema = excluded.input_schema,
                 discovered_at = excluded.discovered_at",
        )?
        .execute(params![
            &server.name,
            &tool.name,
            &tool.description,
            &tool.input_schema_json,
            tool.discovered_at,
            tool.decision.map(|d| d.as_db_str()),
        ])?;
    }
    tx.commit()?;
    Ok(())
}

pub(super) fn query_list_server_tools(
    conn: &Connection,
    server: &str,
) -> Result<Vec<McpTool>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT mcp_server, name, description, input_schema, discovered_at, decision
         FROM mcp_tool WHERE mcp_server = ?1 ORDER BY name",
    )?;
    let rows = stmt.query_map(params![server], decode_tool_row)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Set the decision for a single (server, tool) pair. If the tool doesn't exist
/// in mcp_tool yet, this is a no-op (caller should probe first).
pub(super) fn query_set_tool_decision(
    conn: &Connection,
    server: &str,
    tool: &str,
    decision: Decision,
    _source: DecisionSource,
) -> Result<(), DbError> {
    conn.prepare_cached("UPDATE mcp_tool SET decision = ?1 WHERE mcp_server = ?2 AND name = ?3")?
        .execute(params![decision.as_db_str(), server, tool])?;
    Ok(())
}

/// Get the decision for a specific (server, tool) pair.
pub(super) fn query_get_tool_decision(
    conn: &Connection,
    server: &str,
    tool: &str,
) -> Result<Option<Decision>, DbError> {
    let dec: Option<Option<String>> = conn
        .prepare_cached("SELECT decision FROM mcp_tool WHERE mcp_server = ?1 AND name = ?2")?
        .query_row(params![server, tool], |row| row.get::<_, Option<String>>(0))
        .optional()?;
    Ok(dec.flatten().and_then(|s| Decision::from_db_str(&s)))
}

/// 3-tier policy resolution: tool decision → settings_kv default → fallback Ask.
pub(super) fn query_resolve_policy(
    conn: &Connection,
    server: &str,
    tool: &str,
) -> Result<Decision, DbError> {
    // Tier 1: per-tool decision
    if let Some(d) = query_get_tool_decision(conn, server, tool)? {
        return Ok(d);
    }
    // Tier 2: global default from settings_kv
    let default_str: Option<String> = conn
        .prepare_cached("SELECT value FROM settings_kv WHERE key = 'default_unknown_tool_policy'")?
        .query_row([], |row| row.get::<_, String>(0))
        .optional()?;
    if let Some(ref s) = default_str {
        if let Some(d) = Decision::from_db_str(s) {
            return Ok(d);
        }
    }
    // Tier 3: hardcoded fallback
    Ok(Decision::Ask)
}

pub(super) fn query_set_server_auth_error(
    conn: &Connection,
    server: &str,
    message: &str,
) -> Result<bool, DbError> {
    let prev: Option<String> = conn
        .prepare_cached("SELECT last_auth_error_msg FROM mcp_server WHERE name = ?1")?
        .query_row(params![server], |row| row.get::<_, Option<String>>(0))
        .optional()?
        .flatten();
    let transition = prev.as_deref() != Some(message);
    conn.prepare_cached("UPDATE mcp_server SET last_auth_error_msg = ?1 WHERE name = ?2")?
        .execute(params![message, server])?;
    Ok(transition)
}

pub(super) fn query_clear_server_auth_error(
    conn: &Connection,
    server: &str,
) -> Result<bool, DbError> {
    let prev: Option<String> = conn
        .prepare_cached("SELECT last_auth_error_msg FROM mcp_server WHERE name = ?1")?
        .query_row(params![server], |row| row.get::<_, Option<String>>(0))
        .optional()?
        .flatten();
    let cleared = prev.is_some();
    if cleared {
        conn.prepare_cached("UPDATE mcp_server SET last_auth_error_msg = NULL WHERE name = ?1")?
            .execute(params![server])?;
    }
    Ok(cleared)
}

pub(super) fn query_commit_cascade_delete(
    conn: &Connection,
    server_name: &str,
    delete_audit_history: bool,
) -> Result<Vec<String>, DbError> {
    let tx = conn.unchecked_transaction()?;

    // Remove all managed_mcp rows for this server (FK CASCADE would also do it,
    // but explicit delete lets us do it inside the same tx).
    tx.prepare_cached("DELETE FROM managed_mcp WHERE server_name = ?1")?
        .execute(params![server_name])?;

    // Cancel pending approvals for this server.
    let cancelled_ids: Vec<String> = {
        let mut stmt = tx.prepare_cached(
            "SELECT id FROM pending_approvals WHERE mcp_server = ?1 AND status = 'pending'",
        )?;
        let rows = stmt.query_map(params![server_name], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    if !cancelled_ids.is_empty() {
        let (placeholders, params) = bind_in_clause(&cancelled_ids);
        let sql = format!(
            "UPDATE pending_approvals SET status = 'cancelled' WHERE id IN ({placeholders})"
        );
        tx.prepare(&sql)?.execute(&params[..])?;
    }

    if delete_audit_history {
        tx.prepare_cached("DELETE FROM audit_log WHERE mcp_server = ?1")?
            .execute(params![server_name])?;
    }

    // mcp_tool rows are CASCADE-deleted by the FK on mcp_server.
    tx.prepare_cached("DELETE FROM mcp_server WHERE name = ?1")?
        .execute(params![server_name])?;
    tx.commit()?;
    Ok(cancelled_ids)
}

fn decode_server_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<McpServer> {
    let args_json: String = row.get(3)?;
    let env_json: String = row.get(4)?;
    let args: Vec<String> = serde_json::from_str(&args_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            format!("args_json: {e}").into(),
        )
    })?;
    let env: Vec<EnvEntry> = serde_json::from_str(&env_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            format!("env_json: {e}").into(),
        )
    })?;
    Ok(McpServer {
        name: row.get(0)?,
        transport: row.get(1)?,
        command: row.get(2)?,
        args,
        env,
        url: row.get(5)?,
        lifecycle_seconds: row.get(6)?,
        reported_name: row.get(7)?,
        reported_version: row.get(8)?,
        last_probed_at: row.get(9)?,
        last_probe_error: row.get(10)?,
        last_auth_error_msg: row.get(11)?,
    })
}

fn decode_tool_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<McpTool> {
    let decision_str: Option<String> = row.get(5)?;
    Ok(McpTool {
        server: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        input_schema_json: row.get(3)?,
        discovered_at: row.get(4)?,
        decision: decision_str.and_then(|s| Decision::from_db_str(&s)),
    })
}

impl Db {
    pub async fn upsert_server(&self, server: McpServer) -> Result<(), DbError> {
        self.send_cmd(|reply| DbCmd::UpsertServer { server, reply })
            .await
    }

    pub async fn get_server(&self, name: &str) -> Result<Option<McpServer>, DbError> {
        self.send_cmd(|reply| DbCmd::GetServer {
            name: name.to_string(),
            reply,
        })
        .await
    }

    pub async fn list_servers(&self) -> Result<Vec<McpServer>, DbError> {
        self.send_cmd(|reply| DbCmd::ListServers { reply }).await
    }

    pub async fn delete_server(&self, name: &str) -> Result<(), DbError> {
        self.send_cmd(|reply| DbCmd::DeleteServer {
            name: name.to_string(),
            reply,
        })
        .await
    }

    pub async fn set_server_auth_error(
        &self,
        server: &str,
        message: &str,
    ) -> Result<bool, DbError> {
        self.send_cmd(|reply| DbCmd::SetServerAuthError {
            server: server.to_string(),
            message: message.to_string(),
            reply,
        })
        .await
    }

    pub async fn clear_server_auth_error(&self, server: &str) -> Result<bool, DbError> {
        self.send_cmd(|reply| DbCmd::ClearServerAuthError {
            server: server.to_string(),
            reply,
        })
        .await
    }

    pub async fn upsert_server_with_tools(
        &self,
        server: McpServer,
        tools: Vec<McpTool>,
    ) -> Result<(), DbError> {
        self.send_cmd(|reply| DbCmd::UpsertServerWithTools {
            server,
            tools,
            reply,
        })
        .await
    }

    pub async fn list_server_tools(&self, server: &str) -> Result<Vec<McpTool>, DbError> {
        self.send_cmd(|reply| DbCmd::ListServerTools {
            server: server.to_string(),
            reply,
        })
        .await
    }

    pub async fn rename_server(&self, old: &str, new: &str) -> Result<(), DbError> {
        self.send_cmd(|reply| DbCmd::RenameServer {
            old: old.to_string(),
            new: new.to_string(),
            reply,
        })
        .await
    }

    pub async fn commit_cascade_delete(
        &self,
        server_name: &str,
        delete_audit_history: bool,
    ) -> Result<Vec<String>, DbError> {
        self.send_cmd(|reply| DbCmd::CommitCascadeDelete {
            server_name: server_name.to_string(),
            delete_audit_history,
            reply,
        })
        .await
    }

    /// Set the per-tool decision (allow/deny/ask).
    pub async fn set_tool_decision(
        &self,
        server: &str,
        tool: &str,
        decision: Decision,
        source: DecisionSource,
    ) -> Result<(), DbError> {
        self.send_cmd(|reply| DbCmd::SetToolDecision {
            server: server.to_string(),
            tool: tool.to_string(),
            decision,
            source,
            reply,
        })
        .await
    }

    /// Get the per-tool decision. Returns `None` if the tool doesn't exist or
    /// its decision is NULL (inherit global).
    pub async fn get_tool_decision(
        &self,
        server: &str,
        tool: &str,
    ) -> Result<Option<Decision>, DbError> {
        self.send_cmd(|reply| DbCmd::GetToolDecision {
            server: server.to_string(),
            tool: tool.to_string(),
            reply,
        })
        .await
    }

    /// 3-tier policy resolution: tool → global default → Ask.
    pub async fn resolve_policy(&self, server: &str, tool: &str) -> Result<Decision, DbError> {
        self.send_cmd(|reply| DbCmd::ResolvePolicy {
            server: server.to_string(),
            tool: tool.to_string(),
            reply,
        })
        .await
    }
}
