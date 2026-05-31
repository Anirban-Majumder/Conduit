//! Schema bootstrap, run once by the DB actor.
//!
//! `init_conn` sets connection PRAGMAs and applies the compiled baseline
//! `0001_init.sql`. The `schema_version` table is a forward seam: the first
//! post-v0 change lands as an additive `0002_*.sql` — no wipe.

use rusqlite::Connection;

use crate::daemon::errors::DbError;

use super::DbCmd;

/// The single compiled schema baseline. `CREATE TABLE IF NOT EXISTS` /
/// `INSERT OR IGNORE` make it safe to re-run on an existing database.
const INIT_SQL: &str = include_str!("migrations/0001_init.sql");

#[cfg(all(unix, not(debug_assertions)))]
pub(super) fn restrict_db_perms(path: &std::path::Path) {
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt;
    for suffix in ["", "-wal", "-shm"] {
        let mut name: OsString = path.as_os_str().to_os_string();
        name.push(suffix);
        let p = std::path::PathBuf::from(name);
        if !p.exists() {
            continue;
        }
        if let Err(e) = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)) {
            tracing::warn!(path = %p.display(), error = %e, "chmod 0600 failed");
        }
    }
}

#[cfg(all(windows, not(debug_assertions)))]
pub(super) fn restrict_db_perms(path: &std::path::Path) {
    use std::ffi::OsString;
    let user = match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
        (Ok(domain), Ok(name)) => format!("{domain}\\{name}"),
        (_, Ok(name)) => name,
        _ => {
            tracing::warn!("could not determine current user for DB permissions");
            return;
        }
    };
    for suffix in ["", "-wal", "-shm"] {
        let mut name: OsString = path.as_os_str().to_os_string();
        name.push(suffix);
        let p = std::path::PathBuf::from(&name);
        if !p.exists() {
            continue;
        }
        let status = std::process::Command::new("icacls")
            .arg(&p)
            .args([
                "/inheritance:r",
                "/grant:r",
                &format!("{user}:F"),
                "*S-1-5-18:F",
                "*S-1-5-32-544:F",
            ])
            .status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => tracing::warn!(path = %p.display(), status = %s, "icacls failed"),
            Err(e) => tracing::warn!(path = %p.display(), error = %e, "icacls spawn failed"),
        }
    }
}

pub(super) fn init_conn(conn: &Connection) -> Result<(), DbError> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000; PRAGMA foreign_keys = ON;",
    )?;
    conn.execute_batch(INIT_SQL)?;
    Ok(())
}

pub(super) fn query_schema_version(conn: &Connection) -> Result<i64, DbError> {
    let mut stmt = conn.prepare_cached("SELECT MAX(version) FROM schema_version")?;
    let version: i64 = stmt.query_row([], |row| row.get(0))?;
    Ok(version)
}

pub(super) fn query_list_tables(conn: &Connection) -> Result<Vec<String>, DbError> {
    let mut stmt =
        conn.prepare_cached("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub(super) fn query_list_columns(conn: &Connection, table: &str) -> Result<Vec<String>, DbError> {
    let sql = format!("PRAGMA table_info({table})");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

impl super::Db {
    pub async fn schema_version(&self) -> Result<i64, DbError> {
        self.send_cmd(|reply| DbCmd::GetSchemaVersion { reply })
            .await
    }

    pub async fn list_tables(&self) -> Result<Vec<String>, DbError> {
        self.send_cmd(|reply| DbCmd::ListTables { reply }).await
    }

    pub async fn table_columns(&self, table: &str) -> Result<Vec<String>, DbError> {
        self.send_cmd(|reply| DbCmd::ListColumns {
            table: table.to_string(),
            reply,
        })
        .await
    }
}
