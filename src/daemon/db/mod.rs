pub mod types;

use std::path::{Path, PathBuf};
use std::thread;

use rusqlite::{params, Connection, OptionalExtension};
use tokio::sync::{mpsc, oneshot};

use crate::daemon::db::types::{fresh_ide_key, IdeKey, ManagedIdeConfig};
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
