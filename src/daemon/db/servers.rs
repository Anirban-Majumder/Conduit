//! Server-row CRUD plus the multi-table operations that hang off it.
//!
//! Covers `servers` upserts/reads, the atomic add-server-with-tools write
//! (server row + `server_tools` + default `policies`), rename (FK-cascaded),
//! auth-error transition tracking, and `commit_cascade_delete` which removes a
//! server while trimming IDE entries and cancelling its pending approvals.

use super::{Db, DbCmd};
use rusqlite::{params, Connection, OptionalExtension};
use tokio::sync::oneshot;

use super::{now_unix, CascadeTrim};
use crate::daemon::db::types::{EnvEntry, Server, ToolMeta};
use crate::daemon::errors::DbError;

const SERVERS_SELECT: &str = "SELECT name, transport, command, args_json, env_json, url,
       lifecycle_seconds, last_init_duration_ms, package_version,
       mcp_self_version, created_at,
       server_name, server_version, last_probed_at, last_probe_error,
       last_auth_error_msg, last_auth_error_at
 FROM servers";

pub(super) fn query_commit_cascade_delete(
    conn: &Connection,
    server_name: &str,
    trimmed: &[CascadeTrim],
    delete_audit_history: bool,
) -> Result<Vec<String>, DbError> {
    let tx = conn.unchecked_transaction()?;
    let now = now_unix();
    for trim in trimmed {
        let json =
            serde_json::to_string(&trim.remaining).map_err(|e| DbError::Encoding(e.to_string()))?;
        tx.prepare_cached(
            "UPDATE ides
             SET managed_entries_json = ?1, last_seen_at = ?2
             WHERE ide_kind = ?3",
        )?
        .execute(params![json, now, trim.ide_kind])?;
    }

    let mut cancelled_ids: Vec<String> = Vec::new();
    {
        let mut stmt = tx.prepare_cached(
            "SELECT id FROM pending_approvals WHERE server = ?1 AND status = 'pending'",
        )?;
        let rows = stmt.query_map(params![server_name], |row| row.get::<_, String>(0))?;
        for r in rows {
            cancelled_ids.push(r?);
        }
    }
    if !cancelled_ids.is_empty() {
        let placeholders = cancelled_ids
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "UPDATE pending_approvals SET status = 'cancelled' WHERE id IN ({placeholders})"
        );
        let mut params_vec: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(cancelled_ids.len());
        for id in &cancelled_ids {
            params_vec.push(id);
        }
        tx.prepare(&sql)?.execute(&params_vec[..])?;
    }

    if delete_audit_history {
        tx.prepare_cached("DELETE FROM audit_log WHERE server = ?1")?
            .execute(params![server_name])?;
    }

    tx.prepare_cached("DELETE FROM servers WHERE name = ?1")?
        .execute(params![server_name])?;
    tx.commit()?;
    Ok(cancelled_ids)
}

pub(super) fn query_rename_server(conn: &Connection, old: &str, new: &str) -> Result<(), DbError> {
    conn.prepare_cached("UPDATE servers SET name = ?1 WHERE name = ?2")?
        .execute(params![new, old])?;
    Ok(())
}

pub(super) fn query_upsert_server_with_tools(
    conn: &Connection,
    server: &Server,
    tools: &[ToolMeta],
) -> Result<(), DbError> {
    let tx = conn.unchecked_transaction()?;
    query_upsert_server(&tx, server)?;

    // Sync server_tools to exactly the probed set.
    let tool_names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    let placeholders = tool_names
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");
    if tool_names.is_empty() {
        tx.prepare_cached("DELETE FROM server_tools WHERE server = ?1")?
            .execute(params![&server.name])?;
        tx.prepare_cached("DELETE FROM policies WHERE server = ?1 AND scope = 'tool'")?
            .execute(params![&server.name])?;
    } else {
        let del_tools_sql =
            format!("DELETE FROM server_tools WHERE server = ? AND tool NOT IN ({placeholders})");
        let del_pols_sql = format!(
            "DELETE FROM policies WHERE server = ? AND scope = 'tool' AND tool NOT IN ({placeholders})"
        );
        let mut params_vec: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(tool_names.len() + 1);
        params_vec.push(&server.name);
        for t in &tool_names {
            params_vec.push(t);
        }
        tx.prepare(&del_tools_sql)?.execute(&params_vec[..])?;
        tx.prepare(&del_pols_sql)?.execute(&params_vec[..])?;
    }

    for tool in tools {
        tx.prepare_cached(
            "INSERT INTO server_tools (server, tool, description, input_schema, discovered_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(server, tool) DO UPDATE SET
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
        ])?;

        // Default per-tool policy: 'ask' / 'probe'. ON CONFLICT preserves any
        // existing user-set decision (Re-test must not clobber user choices).
        tx.prepare_cached(
            "INSERT INTO policies (server, tool, scope, decision, decision_source, created_at)
             VALUES (?1, ?2, 'tool', 'ask', 'probe', ?3)
             ON CONFLICT(server, tool, scope) DO NOTHING",
        )?
        .execute(params![&server.name, &tool.name, tool.discovered_at])?;
    }

    // Server-wide fallback row (scope='server', tool='').
    let now = now_unix();
    tx.prepare_cached(
        "INSERT INTO policies (server, tool, scope, decision, decision_source, created_at)
         VALUES (?1, '', 'server', 'ask', 'probe', ?2)
         ON CONFLICT(server, tool, scope) DO NOTHING",
    )?
    .execute(params![&server.name, now])?;

    tx.commit()?;
    Ok(())
}

pub(super) fn query_list_server_tools(
    conn: &Connection,
    server: &str,
) -> Result<Vec<ToolMeta>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT tool, description, input_schema, discovered_at
         FROM server_tools WHERE server = ?1 ORDER BY tool",
    )?;
    let rows = stmt.query_map(params![server], |row| {
        Ok(ToolMeta {
            name: row.get(0)?,
            description: row.get(1)?,
            input_schema_json: row.get(2)?,
            discovered_at: row.get(3)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn decode_server_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Server> {
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
    Ok(Server {
        name: row.get(0)?,
        transport: row.get(1)?,
        command: row.get(2)?,
        args,
        env,
        url: row.get(5)?,
        lifecycle_seconds: row.get(6)?,
        last_init_duration_ms: row.get(7)?,
        package_version: row.get(8)?,
        mcp_self_version: row.get(9)?,
        created_at: row.get(10)?,
        server_name: row.get(11).ok(),
        server_version: row.get(12).ok(),
        last_probed_at: row.get(13).ok(),
        last_probe_error: row.get(14).ok(),
        last_auth_error_msg: row.get(15).ok().flatten(),
        last_auth_error_at: row.get(16).ok().flatten(),
    })
}

pub(super) fn query_upsert_server(conn: &Connection, server: &Server) -> Result<(), DbError> {
    let args_json =
        serde_json::to_string(&server.args).map_err(|e| DbError::Encoding(e.to_string()))?;
    let env_json =
        serde_json::to_string(&server.env).map_err(|e| DbError::Encoding(e.to_string()))?;
    conn.prepare_cached(
        "INSERT INTO servers
         (name, transport, command, args_json, env_json, url, lifecycle_seconds,
          last_init_duration_ms, package_version, mcp_self_version, created_at,
          server_name, server_version, last_probed_at, last_probe_error)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
         ON CONFLICT(name) DO UPDATE SET
             transport = excluded.transport,
             command = excluded.command,
             args_json = excluded.args_json,
             env_json = excluded.env_json,
             url = excluded.url,
             lifecycle_seconds = excluded.lifecycle_seconds,
             last_init_duration_ms = excluded.last_init_duration_ms,
             package_version = excluded.package_version,
             mcp_self_version = excluded.mcp_self_version,
             server_name = excluded.server_name,
             server_version = excluded.server_version,
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
        server.last_init_duration_ms,
        &server.package_version,
        &server.mcp_self_version,
        server.created_at,
        &server.server_name,
        &server.server_version,
        server.last_probed_at,
        &server.last_probe_error,
    ])?;
    Ok(())
}

pub(super) fn query_get_server(conn: &Connection, name: &str) -> Result<Option<Server>, DbError> {
    let sql = format!("{SERVERS_SELECT} WHERE name = ?1");
    let row = conn
        .prepare_cached(&sql)?
        .query_row(params![name], decode_server_row)
        .optional()?;
    Ok(row)
}

pub(super) fn query_list_servers(conn: &Connection) -> Result<Vec<Server>, DbError> {
    let sql = format!("{SERVERS_SELECT} ORDER BY name");
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map([], decode_server_row)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

pub(super) fn query_delete_server(conn: &Connection, name: &str) -> Result<(), DbError> {
    conn.prepare_cached("DELETE FROM servers WHERE name = ?1")?
        .execute(params![name])?;
    Ok(())
}

pub(super) fn query_set_server_auth_error(
    conn: &Connection,
    server: &str,
    message: &str,
) -> Result<bool, DbError> {
    let prev: Option<String> = conn
        .prepare_cached("SELECT last_auth_error_msg FROM servers WHERE name = ?1")?
        .query_row(params![server], |row| row.get::<_, Option<String>>(0))
        .optional()?
        .flatten();
    let transition = prev.as_deref() != Some(message);
    let now = now_unix();
    conn.prepare_cached(
        "UPDATE servers SET last_auth_error_msg = ?1, last_auth_error_at = ?2 WHERE name = ?3",
    )?
    .execute(params![message, now, server])?;
    Ok(transition)
}

pub(super) fn query_clear_server_auth_error(
    conn: &Connection,
    server: &str,
) -> Result<bool, DbError> {
    let prev: Option<String> = conn
        .prepare_cached("SELECT last_auth_error_msg FROM servers WHERE name = ?1")?
        .query_row(params![server], |row| row.get::<_, Option<String>>(0))
        .optional()?
        .flatten();
    let cleared = prev.is_some();
    if cleared {
        conn.prepare_cached(
            "UPDATE servers SET last_auth_error_msg = NULL, last_auth_error_at = NULL WHERE name = ?1",
        )?
        .execute(params![server])?;
    }
    Ok(cleared)
}

impl Db {
    pub async fn upsert_server(&self, server: Server) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::UpsertServer { server, reply })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn get_server(&self, name: &str) -> Result<Option<Server>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::GetServer {
                name: name.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn list_servers(&self) -> Result<Vec<Server>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::ListServers { reply })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn delete_server(&self, name: &str) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::DeleteServer {
                name: name.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    /// Returns `true` if this is a transition (prior auth-error was NULL or different).
    pub async fn set_server_auth_error(
        &self,
        server: &str,
        message: &str,
    ) -> Result<bool, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::SetServerAuthError {
                server: server.to_string(),
                message: message.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    /// Returns `true` if a non-null auth-error was actually cleared.
    pub async fn clear_server_auth_error(&self, server: &str) -> Result<bool, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::ClearServerAuthError {
                server: server.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    /// Atomic Save for the Add Server wizard: inserts the server row, per-tool
    /// policies (defaulting to `ask`/`probe`), the server-wide fallback policy
    /// (`scope='server'`), and one `server_tools` row per probed tool. All in
    /// a single transaction.
    pub async fn upsert_server_with_tools(
        &self,
        server: Server,
        tools: Vec<ToolMeta>,
    ) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::UpsertServerWithTools {
                server,
                tools,
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn list_server_tools(&self, server: &str) -> Result<Vec<ToolMeta>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::ListServerTools {
                server: server.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    /// Rename a server. Cascades through `policies` and `server_tools` via FK
    /// ON UPDATE CASCADE. Caller rewrites IDE config files.
    pub async fn rename_server(&self, old: &str, new: &str) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::RenameServer {
                old: old.to_string(),
                new: new.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn commit_cascade_delete(
        &self,
        server_name: &str,
        trimmed: Vec<CascadeTrim>,
        delete_audit_history: bool,
    ) -> Result<Vec<String>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::CommitCascadeDelete {
                server_name: server_name.to_string(),
                trimmed,
                delete_audit_history,
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }
}
