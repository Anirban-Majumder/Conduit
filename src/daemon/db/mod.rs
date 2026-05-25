use std::path::Path;
use std::thread;

use rusqlite::Connection;
use tokio::sync::{mpsc, oneshot};

use crate::daemon::errors::DbError;

const INIT_SQL: &str = include_str!("migrations/0001_init.sql");
const CMD_CHANNEL_DEPTH: usize = 64;

#[derive(Debug)]
pub enum DbCmd {
    Ping {
        reply: oneshot::Sender<()>,
    },
    GetSchemaVersion {
        reply: oneshot::Sender<Result<i64, DbError>>,
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
    }
}

fn query_schema_version(conn: &Connection) -> Result<i64, DbError> {
    let mut stmt = conn.prepare_cached("SELECT MAX(version) FROM schema_version")?;
    let version: i64 = stmt.query_row([], |row| row.get(0))?;
    Ok(version)
}
