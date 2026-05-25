pub mod types;

use std::path::{Path, PathBuf};
use std::thread;

use rusqlite::{params, params_from_iter, types::Value as SqlValue, Connection, OptionalExtension};
use tokio::sync::{mpsc, oneshot};

use crate::daemon::db::types::{
    fresh_ide_key, ApprovalStatus, AuditDecision, AuditEntry, AuditFilter, Decision,
    DecisionSource, IdeKey, ManagedIdeConfig, PendingApproval, Policy,
};
use crate::daemon::errors::DbError;

const INIT_SQL: &str = include_str!("migrations/0001_init.sql");
const CMD_CHANNEL_DEPTH: usize = 64;

fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    i64::try_from(secs).unwrap_or(i64::MAX)
}

#[derive(Debug)]
pub enum DbCmd {
    Ping {
        reply: oneshot::Sender<()>,
    },
    GetSchemaVersion {
        reply: oneshot::Sender<Result<i64, DbError>>,
    },
    ListTables {
        reply: oneshot::Sender<Result<Vec<String>, DbError>>,
    },
    CreateIdeKey {
        ide_name: String,
        reply: oneshot::Sender<Result<IdeKey, DbError>>,
    },
    LookupIdeKey {
        key: String,
        reply: oneshot::Sender<Result<Option<IdeKey>, DbError>>,
    },
    ListIdeKeysForName {
        ide_name: String,
        reply: oneshot::Sender<Result<Vec<IdeKey>, DbError>>,
    },
    DeleteIdeKey {
        key: String,
        reply: oneshot::Sender<Result<(), DbError>>,
    },
    RecordIdeConfigWrite {
        config: ManagedIdeConfig,
        reply: oneshot::Sender<Result<(), DbError>>,
    },
    ListIdeConfigsForIde {
        ide_name: String,
        reply: oneshot::Sender<Result<Vec<ManagedIdeConfig>, DbError>>,
    },
    DeleteIdeConfigsForIde {
        ide_name: String,
        reply: oneshot::Sender<Result<(), DbError>>,
    },
    SetPolicy {
        server: String,
        tool: String,
        decision: Decision,
        decision_source: DecisionSource,
        reply: oneshot::Sender<Result<i64, DbError>>,
    },
    GetPolicy {
        server: String,
        tool: String,
        reply: oneshot::Sender<Result<Option<Policy>, DbError>>,
    },
    ListPoliciesForServer {
        server: String,
        reply: oneshot::Sender<Result<Vec<Policy>, DbError>>,
    },
    ListAllPolicies {
        reply: oneshot::Sender<Result<Vec<Policy>, DbError>>,
    },
    DeletePoliciesForServer {
        server: String,
        reply: oneshot::Sender<Result<(), DbError>>,
    },
    CreatePendingApproval {
        approval: PendingApproval,
        reply: oneshot::Sender<Result<(), DbError>>,
    },
    GetPendingApproval {
        id: String,
        reply: oneshot::Sender<Result<Option<PendingApproval>, DbError>>,
    },
    ResolvePendingApproval {
        id: String,
        new_status: ApprovalStatus,
        reply: oneshot::Sender<Result<(), DbError>>,
    },
    ListPendingApprovals {
        reply: oneshot::Sender<Result<Vec<PendingApproval>, DbError>>,
    },
    SweepStalePendingApprovals {
        older_than_ts: i64,
        reply: oneshot::Sender<Result<u64, DbError>>,
    },
    InsertAuditEntry {
        entry: AuditEntry,
        reply: oneshot::Sender<Result<i64, DbError>>,
    },
    QueryAuditLog {
        filter: AuditFilter,
        reply: oneshot::Sender<Result<Vec<AuditEntry>, DbError>>,
    },
    PurgeOldAuditEntries {
        older_than_ts: i64,
        reply: oneshot::Sender<Result<u64, DbError>>,
    },
}

#[derive(Debug)]
pub struct Db {
    tx: mpsc::Sender<DbCmd>,
}

impl Db {
    pub fn spawn_at(path: &Path) -> Result<Self, DbError> {
        let conn = Connection::open(path)?;
        Self::spawn_with(conn)
    }

    pub fn spawn_in_memory() -> Result<Self, DbError> {
        let conn = Connection::open_in_memory()?;
        Self::spawn_with(conn)
    }

    fn spawn_with(conn: Connection) -> Result<Self, DbError> {
        let (tx, rx) = mpsc::channel::<DbCmd>(CMD_CHANNEL_DEPTH);
        thread::Builder::new()
            .name("mcp-router-db".into())
            .spawn(move || actor_loop(conn, rx))?;
        Ok(Self { tx })
    }

    pub async fn ping(&self) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::Ping { reply })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)
    }

    pub async fn schema_version(&self) -> Result<i64, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::GetSchemaVersion { reply })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn list_tables(&self) -> Result<Vec<String>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::ListTables { reply })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn create_ide_key(&self, ide_name: &str) -> Result<IdeKey, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::CreateIdeKey {
                ide_name: ide_name.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn lookup_ide_key(&self, key: &str) -> Result<Option<IdeKey>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::LookupIdeKey {
                key: key.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn list_ide_keys_for_name(&self, ide_name: &str) -> Result<Vec<IdeKey>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::ListIdeKeysForName {
                ide_name: ide_name.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn delete_ide_key(&self, key: &str) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::DeleteIdeKey {
                key: key.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn record_ide_config_write(&self, config: ManagedIdeConfig) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::RecordIdeConfigWrite { config, reply })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn list_ide_configs_for_ide(
        &self,
        ide_name: &str,
    ) -> Result<Vec<ManagedIdeConfig>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::ListIdeConfigsForIde {
                ide_name: ide_name.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn delete_ide_configs_for_ide(&self, ide_name: &str) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::DeleteIdeConfigsForIde {
                ide_name: ide_name.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn set_policy(
        &self,
        server: &str,
        tool: &str,
        decision: Decision,
        decision_source: DecisionSource,
    ) -> Result<i64, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::SetPolicy {
                server: server.to_string(),
                tool: tool.to_string(),
                decision,
                decision_source,
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn get_policy(&self, server: &str, tool: &str) -> Result<Option<Policy>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::GetPolicy {
                server: server.to_string(),
                tool: tool.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn list_policies_for_server(&self, server: &str) -> Result<Vec<Policy>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::ListPoliciesForServer {
                server: server.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn list_all_policies(&self) -> Result<Vec<Policy>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::ListAllPolicies { reply })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn delete_policies_for_server(&self, server: &str) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::DeletePoliciesForServer {
                server: server.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn create_pending_approval(&self, approval: PendingApproval) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::CreatePendingApproval { approval, reply })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn get_pending_approval(&self, id: &str) -> Result<Option<PendingApproval>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::GetPendingApproval {
                id: id.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn resolve_pending_approval(
        &self,
        id: &str,
        new_status: ApprovalStatus,
    ) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::ResolvePendingApproval {
                id: id.to_string(),
                new_status,
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn list_pending_approvals(&self) -> Result<Vec<PendingApproval>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::ListPendingApprovals { reply })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn sweep_stale_pending_approvals(&self, older_than_ts: i64) -> Result<u64, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::SweepStalePendingApprovals {
                older_than_ts,
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn insert_audit_entry(&self, entry: AuditEntry) -> Result<i64, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::InsertAuditEntry { entry, reply })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn query_audit_log(&self, filter: AuditFilter) -> Result<Vec<AuditEntry>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::QueryAuditLog { filter, reply })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn purge_old_audit_entries(&self, older_than_ts: i64) -> Result<u64, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::PurgeOldAuditEntries {
                older_than_ts,
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }
}

fn actor_loop(conn: Connection, mut rx: mpsc::Receiver<DbCmd>) {
    if let Err(e) = init_conn(&conn) {
        tracing::error!(error = %e, "db actor init failed; refusing further commands");
        return;
    }
    while let Some(cmd) = rx.blocking_recv() {
        dispatch(&conn, cmd);
    }
}

fn init_conn(conn: &Connection) -> Result<(), DbError> {
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000;")?;
    conn.execute_batch(INIT_SQL)?;
    Ok(())
}

#[expect(clippy::too_many_lines, reason = "thin match over many DbCmd variants")]
fn dispatch(conn: &Connection, cmd: DbCmd) {
    match cmd {
        DbCmd::Ping { reply } => {
            reply.send(()).ok();
        }
        DbCmd::GetSchemaVersion { reply } => {
            reply.send(query_schema_version(conn)).ok();
        }
        DbCmd::ListTables { reply } => {
            reply.send(query_list_tables(conn)).ok();
        }
        DbCmd::CreateIdeKey { ide_name, reply } => {
            reply.send(query_create_ide_key(conn, &ide_name)).ok();
        }
        DbCmd::LookupIdeKey { key, reply } => {
            reply.send(query_lookup_ide_key(conn, &key)).ok();
        }
        DbCmd::ListIdeKeysForName { ide_name, reply } => {
            reply
                .send(query_list_ide_keys_for_name(conn, &ide_name))
                .ok();
        }
        DbCmd::DeleteIdeKey { key, reply } => {
            reply.send(query_delete_ide_key(conn, &key)).ok();
        }
        DbCmd::RecordIdeConfigWrite { config, reply } => {
            reply
                .send(query_record_ide_config_write(conn, &config))
                .ok();
        }
        DbCmd::ListIdeConfigsForIde { ide_name, reply } => {
            reply
                .send(query_list_ide_configs_for_ide(conn, &ide_name))
                .ok();
        }
        DbCmd::DeleteIdeConfigsForIde { ide_name, reply } => {
            reply
                .send(query_delete_ide_configs_for_ide(conn, &ide_name))
                .ok();
        }
        DbCmd::SetPolicy {
            server,
            tool,
            decision,
            decision_source,
            reply,
        } => {
            reply
                .send(query_set_policy(
                    conn,
                    &server,
                    &tool,
                    decision,
                    decision_source,
                ))
                .ok();
        }
        DbCmd::GetPolicy {
            server,
            tool,
            reply,
        } => {
            reply.send(query_get_policy(conn, &server, &tool)).ok();
        }
        DbCmd::ListPoliciesForServer { server, reply } => {
            reply
                .send(query_list_policies_for_server(conn, &server))
                .ok();
        }
        DbCmd::ListAllPolicies { reply } => {
            reply.send(query_list_all_policies(conn)).ok();
        }
        DbCmd::DeletePoliciesForServer { server, reply } => {
            reply
                .send(query_delete_policies_for_server(conn, &server))
                .ok();
        }
        DbCmd::CreatePendingApproval { approval, reply } => {
            reply
                .send(query_create_pending_approval(conn, &approval))
                .ok();
        }
        DbCmd::GetPendingApproval { id, reply } => {
            reply.send(query_get_pending_approval(conn, &id)).ok();
        }
        DbCmd::ResolvePendingApproval {
            id,
            new_status,
            reply,
        } => {
            reply
                .send(query_resolve_pending_approval(conn, &id, new_status))
                .ok();
        }
        DbCmd::ListPendingApprovals { reply } => {
            reply.send(query_list_pending_approvals(conn)).ok();
        }
        DbCmd::SweepStalePendingApprovals {
            older_than_ts,
            reply,
        } => {
            reply
                .send(query_sweep_stale_pending_approvals(conn, older_than_ts))
                .ok();
        }
        DbCmd::InsertAuditEntry { entry, reply } => {
            reply.send(query_insert_audit_entry(conn, &entry)).ok();
        }
        DbCmd::QueryAuditLog { filter, reply } => {
            reply.send(query_query_audit_log(conn, &filter)).ok();
        }
        DbCmd::PurgeOldAuditEntries {
            older_than_ts,
            reply,
        } => {
            reply
                .send(query_purge_old_audit_entries(conn, older_than_ts))
                .ok();
        }
    }
}

fn query_schema_version(conn: &Connection) -> Result<i64, DbError> {
    let mut stmt = conn.prepare_cached("SELECT MAX(version) FROM schema_version")?;
    let version: i64 = stmt.query_row([], |row| row.get(0))?;
    Ok(version)
}

fn query_list_tables(conn: &Connection) -> Result<Vec<String>, DbError> {
    let mut stmt =
        conn.prepare_cached("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn query_create_ide_key(conn: &Connection, ide_name: &str) -> Result<IdeKey, DbError> {
    let now = now_unix();
    let key = fresh_ide_key();
    conn.prepare_cached("INSERT INTO ide_keys (key, ide_name, created_at) VALUES (?1, ?2, ?3)")?
        .execute(params![&key, ide_name, now])?;
    Ok(IdeKey {
        key,
        ide_name: ide_name.to_string(),
        created_at: now,
    })
}

fn query_lookup_ide_key(conn: &Connection, key: &str) -> Result<Option<IdeKey>, DbError> {
    let mut stmt =
        conn.prepare_cached("SELECT key, ide_name, created_at FROM ide_keys WHERE key = ?1")?;
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

fn query_list_ide_keys_for_name(conn: &Connection, ide_name: &str) -> Result<Vec<IdeKey>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT key, ide_name, created_at FROM ide_keys WHERE ide_name = ?1 ORDER BY created_at DESC",
    )?;
    let rows = stmt.query_map(params![ide_name], |row| {
        Ok(IdeKey {
            key: row.get(0)?,
            ide_name: row.get(1)?,
            created_at: row.get(2)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn query_delete_ide_key(conn: &Connection, key: &str) -> Result<(), DbError> {
    conn.prepare_cached("DELETE FROM ide_keys WHERE key = ?1")?
        .execute(params![key])?;
    Ok(())
}

fn query_record_ide_config_write(conn: &Connection, cfg: &ManagedIdeConfig) -> Result<(), DbError> {
    let entries_json = serde_json::to_string(&cfg.managed_entries)
        .map_err(|e| DbError::Encoding(e.to_string()))?;
    let path_str = cfg
        .config_path
        .to_str()
        .ok_or_else(|| DbError::Encoding("non-utf8 config_path".to_string()))?;
    conn.prepare_cached(
        "INSERT INTO managed_ide_configs (ide_name, ide_key, config_path, managed_entries_json, written_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(ide_name, ide_key) DO UPDATE SET
             config_path = excluded.config_path,
             managed_entries_json = excluded.managed_entries_json,
             written_at = excluded.written_at",
    )?
    .execute(params![
        &cfg.ide_name,
        &cfg.ide_key,
        path_str,
        entries_json,
        cfg.written_at,
    ])?;
    Ok(())
}

fn query_list_ide_configs_for_ide(
    conn: &Connection,
    ide_name: &str,
) -> Result<Vec<ManagedIdeConfig>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT ide_name, ide_key, config_path, managed_entries_json, written_at
         FROM managed_ide_configs WHERE ide_name = ?1 ORDER BY written_at DESC",
    )?;
    let rows = stmt.query_map(params![ide_name], |row| {
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
        let (ide_name, ide_key, config_path, entries_json, written_at) = r?;
        let managed_entries: Vec<String> =
            serde_json::from_str(&entries_json).map_err(|e| DbError::Encoding(e.to_string()))?;
        out.push(ManagedIdeConfig {
            ide_name,
            ide_key,
            config_path,
            managed_entries,
            written_at,
        });
    }
    Ok(out)
}

fn query_delete_ide_configs_for_ide(conn: &Connection, ide_name: &str) -> Result<(), DbError> {
    conn.prepare_cached("DELETE FROM managed_ide_configs WHERE ide_name = ?1")?
        .execute(params![ide_name])?;
    Ok(())
}

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

fn query_set_policy(
    conn: &Connection,
    server: &str,
    tool: &str,
    decision: Decision,
    decision_source: DecisionSource,
) -> Result<i64, DbError> {
    let now = now_unix();
    let id: i64 = conn
        .prepare_cached(
            "INSERT INTO policies (server, tool, decision, decision_source, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(server, tool) DO UPDATE SET
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

fn query_get_policy(
    conn: &Connection,
    server: &str,
    tool: &str,
) -> Result<Option<Policy>, DbError> {
    let row = conn
        .prepare_cached(
            "SELECT id, server, tool, decision, decision_source, created_at
             FROM policies WHERE server = ?1 AND tool = ?2",
        )?
        .query_row(params![server, tool], decode_policy_row)
        .optional()?;
    Ok(row)
}

fn query_list_policies_for_server(conn: &Connection, server: &str) -> Result<Vec<Policy>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, server, tool, decision, decision_source, created_at
         FROM policies WHERE server = ?1 ORDER BY tool",
    )?;
    let rows = stmt.query_map(params![server], decode_policy_row)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn query_list_all_policies(conn: &Connection) -> Result<Vec<Policy>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, server, tool, decision, decision_source, created_at
         FROM policies ORDER BY server, tool",
    )?;
    let rows = stmt.query_map([], decode_policy_row)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn query_delete_policies_for_server(conn: &Connection, server: &str) -> Result<(), DbError> {
    conn.prepare_cached("DELETE FROM policies WHERE server = ?1")?
        .execute(params![server])?;
    Ok(())
}

fn decode_pending_approval_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PendingApproval> {
    let status_str: String = row.get(6)?;
    let status = ApprovalStatus::from_db_str(&status_str).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            6,
            rusqlite::types::Type::Text,
            format!("unknown approval status: {status_str}").into(),
        )
    })?;
    Ok(PendingApproval {
        id: row.get(0)?,
        server: row.get(1)?,
        tool: row.get(2)?,
        args_json: row.get(3)?,
        ide_key: row.get(4)?,
        created_at: row.get(5)?,
        status,
    })
}

fn query_create_pending_approval(
    conn: &Connection,
    approval: &PendingApproval,
) -> Result<(), DbError> {
    conn.prepare_cached(
        "INSERT INTO pending_approvals (id, server, tool, args_json, ide_key, created_at, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?
    .execute(params![
        &approval.id,
        &approval.server,
        &approval.tool,
        &approval.args_json,
        &approval.ide_key,
        approval.created_at,
        approval.status.as_db_str(),
    ])?;
    Ok(())
}

fn query_get_pending_approval(
    conn: &Connection,
    id: &str,
) -> Result<Option<PendingApproval>, DbError> {
    let row = conn
        .prepare_cached(
            "SELECT id, server, tool, args_json, ide_key, created_at, status
             FROM pending_approvals WHERE id = ?1",
        )?
        .query_row(params![id], decode_pending_approval_row)
        .optional()?;
    Ok(row)
}

fn query_resolve_pending_approval(
    conn: &Connection,
    id: &str,
    new_status: ApprovalStatus,
) -> Result<(), DbError> {
    conn.prepare_cached("UPDATE pending_approvals SET status = ?1 WHERE id = ?2")?
        .execute(params![new_status.as_db_str(), id])?;
    Ok(())
}

fn query_list_pending_approvals(conn: &Connection) -> Result<Vec<PendingApproval>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, server, tool, args_json, ide_key, created_at, status
         FROM pending_approvals WHERE status = 'pending' ORDER BY created_at DESC",
    )?;
    let rows = stmt.query_map([], decode_pending_approval_row)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn query_sweep_stale_pending_approvals(
    conn: &Connection,
    older_than_ts: i64,
) -> Result<u64, DbError> {
    let n = conn
        .prepare_cached(
            "UPDATE pending_approvals SET status = 'timeout'
             WHERE status = 'pending' AND created_at < ?1",
        )?
        .execute(params![older_than_ts])?;
    Ok(n as u64)
}

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

fn query_insert_audit_entry(conn: &Connection, entry: &AuditEntry) -> Result<i64, DbError> {
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

fn query_query_audit_log(
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
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn query_purge_old_audit_entries(conn: &Connection, older_than_ts: i64) -> Result<u64, DbError> {
    let n = conn
        .prepare_cached("DELETE FROM audit_log WHERE ts < ?1")?
        .execute(params![older_than_ts])?;
    Ok(n as u64)
}
