//! `client` table CRUD — replaces the old `ides.rs`.
//!
//! Each row is a detected (or manually registered) editor. Built-in rows are
//! projected from `client_config::BUILTINS` at startup; custom rows are user-added.
//! `mcp_key` is the URL routing token; `tracked` is user-intent (never manage
//! if false). `managed_mcp` and `managed_skill` are the N:M join tables.

use rusqlite::{params, Connection, OptionalExtension};

use super::{now_unix, Db, DbCmd};
use crate::daemon::db::types::{fresh_mcp_key, ClientRow};
use crate::daemon::errors::DbError;

/// Upsert a built-in client row projected from code. Identity fields are always
/// overwritten; `mcp_key` is preserved if the row exists. `tracked` is NOT
/// touched on update (user intent persists across projections).
pub(super) fn query_project_builtin(
    conn: &Connection,
    id: &str,
    name: &str,
    mcp_config_path: &str,
    skills_dir: Option<&str>,
    schema_json: &str,
    project_config_rel: Option<&str>,
    project_skills_rel: Option<&str>,
) -> Result<ClientRow, DbError> {
    let now = now_unix();
    // Try insert first; ON CONFLICT updates identity fields but preserves mcp_key+tracked.
    conn.prepare_cached(
        "INSERT INTO client (id, mcp_key, tracked, is_builtin, first_seen_at, last_seen_at,
                             name, mcp_config_path, skills_dir, schema_json,
                             project_config_rel, project_skills_rel)
         VALUES (?1, ?2, 1, 1, ?3, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(id) DO UPDATE SET
             last_seen_at = excluded.last_seen_at,
             name = excluded.name,
             mcp_config_path = excluded.mcp_config_path,
             skills_dir = excluded.skills_dir,
             schema_json = excluded.schema_json,
             project_config_rel = excluded.project_config_rel,
             project_skills_rel = excluded.project_skills_rel",
    )?
    .execute(params![
        id,
        fresh_mcp_key(),
        now,
        name,
        mcp_config_path,
        skills_dir,
        schema_json,
        project_config_rel,
        project_skills_rel,
    ])?;
    // Re-read to get the actual mcp_key (may have been preserved from a prior row).
    query_get_client(conn, id)?
        .ok_or_else(|| DbError::Encoding(format!("projected client '{id}' vanished after upsert")))
}

/// Bump `last_seen_at` without touching identity columns. Used by the projection
/// when the code version is unchanged and the row already exists (§1a).
pub(super) fn query_touch_client(conn: &Connection, id: &str) -> Result<(), DbError> {
    conn.prepare_cached("UPDATE client SET last_seen_at = ?1 WHERE id = ?2")?
        .execute(params![now_unix(), id])?;
    Ok(())
}

pub(super) fn query_get_client(conn: &Connection, id: &str) -> Result<Option<ClientRow>, DbError> {
    conn.prepare_cached(
        "SELECT id, mcp_key, tracked, is_builtin, first_seen_at, last_seen_at,
                name, mcp_config_path, skills_dir, schema_json,
                project_config_rel, project_skills_rel
         FROM client WHERE id = ?1",
    )?
    .query_row(params![id], decode_client_row)
    .optional()
    .map_err(DbError::from)
}

pub(super) fn query_lookup_client_key(
    conn: &Connection,
    key: &str,
) -> Result<Option<ClientRow>, DbError> {
    conn.prepare_cached(
        "SELECT id, mcp_key, tracked, is_builtin, first_seen_at, last_seen_at,
                name, mcp_config_path, skills_dir, schema_json,
                project_config_rel, project_skills_rel
         FROM client WHERE mcp_key = ?1 AND tracked = 1",
    )?
    .query_row(params![key], decode_client_row)
    .optional()
    .map_err(DbError::from)
}

pub(super) fn query_list_clients(conn: &Connection) -> Result<Vec<ClientRow>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, mcp_key, tracked, is_builtin, first_seen_at, last_seen_at,
                name, mcp_config_path, skills_dir, schema_json,
                project_config_rel, project_skills_rel
         FROM client ORDER BY name",
    )?;
    let rows = stmt.query_map([], decode_client_row)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub(super) fn query_list_tracked_clients(conn: &Connection) -> Result<Vec<ClientRow>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, mcp_key, tracked, is_builtin, first_seen_at, last_seen_at,
                name, mcp_config_path, skills_dir, schema_json,
                project_config_rel, project_skills_rel
         FROM client WHERE tracked = 1 ORDER BY name",
    )?;
    let rows = stmt.query_map([], decode_client_row)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub(super) fn query_set_client_tracked(
    conn: &Connection,
    id: &str,
    tracked: bool,
) -> Result<(), DbError> {
    conn.prepare_cached("UPDATE client SET tracked = ?1 WHERE id = ?2")?
        .execute(params![tracked, id])?;
    Ok(())
}

/// List server names managed for a `(client, scope)` from `managed_mcp`.
pub(super) fn query_list_managed_servers(
    conn: &Connection,
    client_id: &str,
    scope: &str,
) -> Result<Vec<String>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT server_name FROM managed_mcp
         WHERE client_id = ?1 AND scope = ?2
         ORDER BY server_name",
    )?;
    let rows = stmt.query_map(params![client_id, scope], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Replace the set of managed servers for one `(client, scope)`.
pub(super) fn query_set_managed_servers(
    conn: &Connection,
    client_id: &str,
    scope: &str,
    server_names: &[String],
) -> Result<(), DbError> {
    let tx = conn.unchecked_transaction()?;
    tx.prepare_cached("DELETE FROM managed_mcp WHERE client_id = ?1 AND scope = ?2")?
        .execute(params![client_id, scope])?;
    for name in server_names {
        tx.prepare_cached(
            "INSERT OR IGNORE INTO managed_mcp (client_id, scope, server_name) VALUES (?1, ?2, ?3)",
        )?
        .execute(params![client_id, scope, name])?;
    }
    tx.commit()?;
    Ok(())
}

/// Add one managed server for a `(client, scope)`. Idempotent (popover toggle).
pub(super) fn query_add_managed_server(
    conn: &Connection,
    client_id: &str,
    scope: &str,
    server_name: &str,
) -> Result<(), DbError> {
    conn.prepare_cached(
        "INSERT OR IGNORE INTO managed_mcp (client_id, scope, server_name) VALUES (?1, ?2, ?3)",
    )?
    .execute(params![client_id, scope, server_name])?;
    Ok(())
}

/// Remove one managed server for a `(client, scope)` (popover ✕).
pub(super) fn query_remove_managed_server(
    conn: &Connection,
    client_id: &str,
    scope: &str,
    server_name: &str,
) -> Result<(), DbError> {
    conn.prepare_cached(
        "DELETE FROM managed_mcp WHERE client_id = ?1 AND scope = ?2 AND server_name = ?3",
    )?
    .execute(params![client_id, scope, server_name])?;
    Ok(())
}

/// Remove a server from every scope of one client ("Remove from all").
pub(super) fn query_remove_server_all_scopes(
    conn: &Connection,
    client_id: &str,
    server_name: &str,
) -> Result<(), DbError> {
    conn.prepare_cached("DELETE FROM managed_mcp WHERE client_id = ?1 AND server_name = ?2")?
        .execute(params![client_id, server_name])?;
    Ok(())
}

/// Every distinct repo scope (excludes `'global'`) that has ≥1 managed MCP or
/// skill row — the repo list backing the popover + Editors selector (L4).
pub(super) fn query_list_known_repos(conn: &Connection) -> Result<Vec<String>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT scope FROM managed_mcp WHERE scope != 'global'
         UNION
         SELECT scope FROM managed_skill WHERE scope != 'global'
         ORDER BY scope",
    )?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Every scope (incl. `'global'`) this client has any managed MCP or skill row
/// in — feeds the Editors-tab scope selector.
pub(super) fn query_list_scopes_for_client(
    conn: &Connection,
    client_id: &str,
) -> Result<Vec<String>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT scope FROM managed_mcp WHERE client_id = ?1
         UNION
         SELECT scope FROM managed_skill WHERE client_id = ?1
         ORDER BY scope",
    )?;
    let rows = stmt.query_map(params![client_id], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// List the `(client_id, scope)` of every client managing `server_name` — the
/// reverse lookup the cascade needs. Hits `idx_managed_mcp_server`.
pub(super) fn query_list_clients_managing_server(
    conn: &Connection,
    server_name: &str,
) -> Result<Vec<(String, String)>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT client_id, scope FROM managed_mcp
         WHERE server_name = ?1 ORDER BY client_id, scope",
    )?;
    let rows = stmt.query_map(params![server_name], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Return `true` if `client_id` manages `server_name` in any scope.
pub(super) fn query_client_manages_server(
    conn: &Connection,
    client_id: &str,
    server_name: &str,
) -> Result<bool, DbError> {
    let count: i64 = conn
        .prepare_cached(
            "SELECT COUNT(*) FROM managed_mcp
             WHERE client_id = ?1 AND server_name = ?2 LIMIT 1",
        )?
        .query_row(params![client_id, server_name], |row| row.get(0))?;
    Ok(count > 0)
}

fn decode_client_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ClientRow> {
    Ok(ClientRow {
        id: row.get(0)?,
        mcp_key: row.get(1)?,
        tracked: row.get::<_, i32>(2)? != 0,
        is_builtin: row.get::<_, i32>(3)? != 0,
        first_seen_at: row.get(4)?,
        last_seen_at: row.get(5)?,
        name: row.get(6)?,
        mcp_config_path: row.get(7)?,
        skills_dir: row.get(8)?,
        schema_json: row.get(9)?,
        project_config_rel: row.get(10)?,
        project_skills_rel: row.get(11)?,
    })
}

impl Db {
    pub async fn project_builtin(
        &self,
        id: &str,
        name: &str,
        mcp_config_path: &str,
        skills_dir: Option<&str>,
        schema_json: &str,
        project_config_rel: Option<&str>,
        project_skills_rel: Option<&str>,
    ) -> Result<ClientRow, DbError> {
        self.send_cmd(|reply| DbCmd::ProjectBuiltin {
            id: id.to_string(),
            name: name.to_string(),
            mcp_config_path: mcp_config_path.to_string(),
            skills_dir: skills_dir.map(String::from),
            schema_json: schema_json.to_string(),
            project_config_rel: project_config_rel.map(String::from),
            project_skills_rel: project_skills_rel.map(String::from),
            reply,
        })
        .await
    }

    pub async fn touch_client(&self, id: &str) -> Result<(), DbError> {
        self.send_cmd(|reply| DbCmd::TouchClient {
            id: id.to_string(),
            reply,
        })
        .await
    }

    pub async fn get_client(&self, id: &str) -> Result<Option<ClientRow>, DbError> {
        self.send_cmd(|reply| DbCmd::GetClient {
            id: id.to_string(),
            reply,
        })
        .await
    }

    pub async fn lookup_client_key(&self, key: &str) -> Result<Option<ClientRow>, DbError> {
        self.send_cmd(|reply| DbCmd::LookupClientKey {
            key: key.to_string(),
            reply,
        })
        .await
    }

    pub async fn list_clients(&self) -> Result<Vec<ClientRow>, DbError> {
        self.send_cmd(|reply| DbCmd::ListClients { reply }).await
    }

    pub async fn list_tracked_clients(&self) -> Result<Vec<ClientRow>, DbError> {
        self.send_cmd(|reply| DbCmd::ListTrackedClients { reply })
            .await
    }

    pub async fn set_client_tracked(&self, id: &str, tracked: bool) -> Result<(), DbError> {
        self.send_cmd(|reply| DbCmd::SetClientTracked {
            id: id.to_string(),
            tracked,
            reply,
        })
        .await
    }

    pub async fn list_managed_servers(
        &self,
        client_id: &str,
        scope: &str,
    ) -> Result<Vec<String>, DbError> {
        self.send_cmd(|reply| DbCmd::ListManagedServers {
            client_id: client_id.to_string(),
            scope: scope.to_string(),
            reply,
        })
        .await
    }

    pub async fn set_managed_servers(
        &self,
        client_id: &str,
        scope: &str,
        server_names: Vec<String>,
    ) -> Result<(), DbError> {
        self.send_cmd(|reply| DbCmd::SetManagedServers {
            client_id: client_id.to_string(),
            scope: scope.to_string(),
            server_names,
            reply,
        })
        .await
    }

    pub async fn add_managed_server(
        &self,
        client_id: &str,
        scope: &str,
        server_name: &str,
    ) -> Result<(), DbError> {
        self.send_cmd(|reply| DbCmd::AddManagedServer {
            client_id: client_id.to_string(),
            scope: scope.to_string(),
            server_name: server_name.to_string(),
            reply,
        })
        .await
    }

    pub async fn remove_managed_server(
        &self,
        client_id: &str,
        scope: &str,
        server_name: &str,
    ) -> Result<(), DbError> {
        self.send_cmd(|reply| DbCmd::RemoveManagedServer {
            client_id: client_id.to_string(),
            scope: scope.to_string(),
            server_name: server_name.to_string(),
            reply,
        })
        .await
    }

    pub async fn remove_server_all_scopes(
        &self,
        client_id: &str,
        server_name: &str,
    ) -> Result<(), DbError> {
        self.send_cmd(|reply| DbCmd::RemoveServerAllScopes {
            client_id: client_id.to_string(),
            server_name: server_name.to_string(),
            reply,
        })
        .await
    }

    pub async fn list_clients_managing_server(
        &self,
        server_name: &str,
    ) -> Result<Vec<(String, String)>, DbError> {
        self.send_cmd(|reply| DbCmd::ListClientsManagingServer {
            server_name: server_name.to_string(),
            reply,
        })
        .await
    }

    pub async fn client_manages_server(
        &self,
        client_id: &str,
        server_name: &str,
    ) -> Result<bool, DbError> {
        self.send_cmd(|reply| DbCmd::ClientManagesServer {
            client_id: client_id.to_string(),
            server_name: server_name.to_string(),
            reply,
        })
        .await
    }

    pub async fn list_known_repos(&self) -> Result<Vec<String>, DbError> {
        self.send_cmd(|reply| DbCmd::ListKnownRepos { reply }).await
    }

    pub async fn list_scopes_for_client(&self, client_id: &str) -> Result<Vec<String>, DbError> {
        self.send_cmd(|reply| DbCmd::ListScopesForClient {
            client_id: client_id.to_string(),
            reply,
        })
        .await
    }
}
