//! DB-actor entry point and `Db` handle.
//!
//! A single `std::thread` owns the `rusqlite::Connection`; callers reach it
//! only through `Db`, a cheap `mpsc::Sender<DbCmd>` clone. `actor_loop` runs
//! migrations once, then hands each `DbCmd` to `dispatch::dispatch`, which calls
//! the matching domain submodule and replies via the embedded `oneshot`. The
//! `Connection` never crosses `.await`.

pub mod types;

mod approvals;
mod audit;
mod clients;
mod commands;
mod dispatch;
mod migrations;
mod oauth_drafts;
mod servers;
mod settings;
mod skills;

use std::path::Path;
use std::thread;

use rusqlite::Connection;
use tokio::sync::{mpsc, oneshot};

use crate::daemon::errors::DbError;

pub use commands::DbCmd;

use migrations::init_conn;

#[cfg(any(all(unix, not(debug_assertions)), all(windows, not(debug_assertions))))]
use migrations::restrict_db_perms;

const CMD_CHANNEL_DEPTH: usize = 64;

pub(super) use crate::daemon::time_util::now_unix_secs as now_unix;

/// Build the `?, ?…` placeholder string and matching param refs for an IN/NOT-IN clause.
///
/// Returns `("''", vec![])` when `items` is empty so the generated SQL stays
/// well-formed without an empty `IN ()` (which `SQLite` rejects).
pub(super) fn bind_in_clause<'a, T: rusqlite::ToSql + 'a>(
    items: &'a [T],
) -> (String, Vec<&'a dyn rusqlite::ToSql>) {
    if items.is_empty() {
        return ("''".to_string(), vec![]);
    }
    let placeholders = items.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let params = items.iter().map(|i| i as &dyn rusqlite::ToSql).collect();
    (placeholders, params)
}

#[derive(Debug, Clone)]
pub struct Db {
    tx: mpsc::Sender<DbCmd>,
}

impl Db {
    pub fn spawn_at(path: &Path) -> Result<Self, DbError> {
        let conn = Connection::open(path)?;
        let db_path = path.to_path_buf();
        Self::spawn_with_inner(conn, Some(db_path))
    }

    pub fn spawn_in_memory() -> Result<Self, DbError> {
        let conn = Connection::open_in_memory()?;
        Self::spawn_with_inner(conn, None)
    }

    fn spawn_with_inner(
        conn: Connection,
        db_path: Option<std::path::PathBuf>,
    ) -> Result<Self, DbError> {
        let (tx, rx) = mpsc::channel::<DbCmd>(CMD_CHANNEL_DEPTH);
        thread::Builder::new()
            .name("conduit-db".into())
            .spawn(move || actor_loop(conn, rx, db_path))?;
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

    /// Send a command to the DB actor and await its `Result<T, DbError>` reply.
    ///
    /// `make` receives the reply sender and wraps it into the concrete `DbCmd`
    /// variant. All public async methods on `Db` collapse to a single call to
    /// this helper, eliminating the repeated oneshot ceremony.
    pub(super) async fn send_cmd<T: Send + 'static>(
        &self,
        make: impl FnOnce(oneshot::Sender<Result<T, DbError>>) -> DbCmd,
    ) -> Result<T, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(make(reply))
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }
}

fn actor_loop(
    conn: Connection,
    mut rx: mpsc::Receiver<DbCmd>,
    db_path: Option<std::path::PathBuf>,
) {
    if let Err(e) = init_conn(&conn) {
        tracing::error!(error = %e, "db actor init failed; refusing further commands");
        return;
    }
    // Restrict perms AFTER init_conn — WAL mode + migrations create -wal/-shm
    // sidecars that must also be locked down.
    #[cfg(any(all(unix, not(debug_assertions)), all(windows, not(debug_assertions))))]
    if let Some(ref path) = db_path {
        restrict_db_perms(path);
    }
    // db_path is only used in release builds for restrict_db_perms above.
    let _ = &db_path;
    while let Some(cmd) = rx.blocking_recv() {
        dispatch::dispatch(&conn, cmd);
    }
}
