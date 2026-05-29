//! Schema bootstrap and forward-migration logic, run once by the DB actor.
//!
//! `init_conn` sets connection PRAGMAs, applies the bundled `migrations/*.sql`
//! batches in order, then performs idempotent ad-hoc fixups (`ensure_column`,
//! FK rebuild, legacy `ide_keys`/`managed_ide_configs` → `ides` migration).
//! Everything here must be safe to re-run on an already-migrated database.

use std::path::{Path, PathBuf};

use super::{Db, DbCmd};
use rusqlite::{params, Connection, OptionalExtension};
use tokio::sync::oneshot;

use crate::daemon::errors::DbError;

const INIT_SQL: &str = include_str!("migrations/0001_init.sql");

const V01_SQL: &str = include_str!("migrations/0002_v01.sql");

const SKILLS_SQL: &str = include_str!("migrations/0003_skills.sql");

const IDES_SQL: &str = include_str!("migrations/0004_ides.sql");

const OAUTH_DRAFTS_SQL: &str = include_str!("migrations/0005_oauth_drafts.sql");

#[cfg(all(unix, not(debug_assertions)))]
pub(super) fn restrict_db_perms(path: &Path) {
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt;
    for suffix in ["", "-wal", "-shm"] {
        let mut name: OsString = path.as_os_str().to_os_string();
        name.push(suffix);
        let p = PathBuf::from(name);
        if !p.exists() {
            continue;
        }
        if let Err(e) = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)) {
            tracing::warn!(path = %p.display(), error = %e, "chmod 0600 failed");
        }
    }
}

#[cfg(all(windows, not(debug_assertions)))]
pub(super) fn restrict_db_perms(path: &Path) {
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
        let p = PathBuf::from(&name);
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
    conn.execute_batch(V01_SQL)?;
    conn.execute_batch(SKILLS_SQL)?;
    conn.execute_batch(IDES_SQL)?;
    conn.execute_batch(OAUTH_DRAFTS_SQL)?;
    ensure_column(conn, "servers", "server_name", "TEXT")?;
    ensure_column(conn, "servers", "server_version", "TEXT")?;
    ensure_column(conn, "servers", "last_probed_at", "INTEGER")?;
    ensure_column(conn, "servers", "last_probe_error", "TEXT")?;
    ensure_column(conn, "servers", "last_auth_error_msg", "TEXT")?;
    ensure_column(conn, "servers", "last_auth_error_at", "INTEGER")?;
    // OAuth client_id + scopes now live in the keyring credential blob; drop the
    // legacy write-only columns from pre-migration databases.
    for col in [
        "oauth_authority",
        "oauth_client_id",
        "oauth_client_secret_ref",
        "oauth_scopes",
    ] {
        drop_column_if_exists(conn, "servers", col)?;
    }
    ensure_column(conn, "policies", "scope", "TEXT NOT NULL DEFAULT 'tool'")?;
    ensure_policies_fk(conn)?;
    migrate_legacy_ide_tables(conn)?;
    Ok(())
}

struct LegacyIdeRow {
    ide_name: String,
    key: String,
    created_at: i64,
    config_path: Option<String>,
    entries_json: Option<String>,
    written_at: Option<i64>,
}

fn migrate_legacy_ide_tables(conn: &Connection) -> Result<(), DbError> {
    let legacy_present: bool = conn
        .prepare("SELECT 1 FROM sqlite_master WHERE type='table' AND name='ide_keys'")?
        .query_row([], |_| Ok(()))
        .optional()?
        .is_some();
    if !legacy_present {
        return Ok(());
    }

    let tx = conn.unchecked_transaction()?;
    let mut select = tx.prepare(
        "SELECT k.ide_name, k.key, k.created_at, m.config_path, m.managed_entries_json, m.written_at
         FROM ide_keys k
         LEFT JOIN managed_ide_configs m
           ON m.ide_name = k.ide_name AND m.ide_key = k.key
         ORDER BY k.created_at DESC",
    )?;
    let rows: Vec<LegacyIdeRow> = select
        .query_map([], |r| {
            Ok(LegacyIdeRow {
                ide_name: r.get::<_, String>(0)?,
                key: r.get::<_, String>(1)?,
                created_at: r.get::<_, i64>(2)?,
                config_path: r.get::<_, Option<String>>(3)?,
                entries_json: r.get::<_, Option<String>>(4)?,
                written_at: r.get::<_, Option<i64>>(5)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    drop(select);

    let mut seen_kinds = std::collections::HashSet::<String>::new();
    for row in rows {
        let Some(kind) = legacy_ide_name_to_kind(&row.ide_name) else {
            continue;
        };
        if !seen_kinds.insert(kind.to_string()) {
            continue;
        }
        let display = legacy_ide_name_to_display(&row.ide_name).to_string();
        let last_seen = row.written_at.unwrap_or(row.created_at);
        tx.prepare_cached(
            "INSERT OR IGNORE INTO ides
                (ide_kind, display_name, ide_key, status, detection_source,
                 config_path, managed_entries_json, first_seen_at, last_seen_at, not_tracked_at)
             VALUES (?1, ?2, ?3, 'active', NULL, ?4, ?5, ?6, ?7, NULL)",
        )?
        .execute(params![
            kind,
            display,
            row.key,
            row.config_path,
            row.entries_json.unwrap_or_else(|| "[]".to_string()),
            row.created_at,
            last_seen,
        ])?;
    }

    tx.execute_batch(
        "DROP TABLE IF EXISTS managed_ide_configs;
         DROP TABLE IF EXISTS ide_keys;",
    )?;
    tx.commit()?;
    Ok(())
}

const fn legacy_ide_name_to_kind(name: &str) -> Option<&'static str> {
    match name.as_bytes() {
        b"ClaudeCode" | b"Claude Code" => Some("ClaudeCode"),
        b"Cursor" => Some("Cursor"),
        b"VsCodeCopilot" | b"VS Code Copilot" | b"VS Code" => Some("VsCodeCopilot"),
        b"OpenCode" => Some("OpenCode"),
        b"OpenClaude" => Some("OpenClaude"),
        b"Codex" => Some("Codex"),
        _ => None,
    }
}

const fn legacy_ide_name_to_display(name: &str) -> &'static str {
    match name.as_bytes() {
        b"ClaudeCode" | b"Claude Code" => "Claude Code",
        b"Cursor" => "Cursor",
        b"VsCodeCopilot" | b"VS Code Copilot" | b"VS Code" => "VS Code Copilot",
        b"OpenCode" => "OpenCode",
        b"OpenClaude" => "OpenClaude",
        b"Codex" => "Codex",
        _ => "Unknown",
    }
}

fn ensure_policies_fk(conn: &Connection) -> Result<(), DbError> {
    let has_fk = {
        let mut stmt = conn.prepare("PRAGMA foreign_key_list(policies)")?;
        let mut rows = stmt.query([])?;
        let mut found = false;
        while let Some(r) = rows.next()? {
            if r.get::<_, String>(2)? == "servers" {
                found = true;
                break;
            }
        }
        found
    };
    if has_fk {
        return Ok(());
    }
    conn.execute_batch(
        "PRAGMA foreign_keys = OFF;
         BEGIN;
         CREATE TABLE policies_new (
             id              INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
             server          TEXT NOT NULL,
             tool            TEXT NOT NULL,
             scope           TEXT NOT NULL DEFAULT 'tool',
             decision        TEXT NOT NULL,
             decision_source TEXT NOT NULL,
             created_at      INTEGER NOT NULL,
             UNIQUE(server, tool, scope),
             FOREIGN KEY (server) REFERENCES servers(name) ON UPDATE CASCADE ON DELETE CASCADE
         );
         INSERT INTO policies_new (id, server, tool, scope, decision, decision_source, created_at)
             SELECT id, server, tool,
                    COALESCE(NULLIF(scope, ''), 'tool'),
                    decision, decision_source, created_at
               FROM policies;
         DROP TABLE policies;
         ALTER TABLE policies_new RENAME TO policies;
         CREATE INDEX IF NOT EXISTS idx_policies_server ON policies(server);
         COMMIT;
         PRAGMA foreign_keys = ON;",
    )?;
    Ok(())
}

pub(super) fn query_list_columns(conn: &Connection, table: &str) -> Result<Vec<String>, DbError> {
    let sql = format!("PRAGMA table_info({table})");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn ensure_column(conn: &Connection, table: &str, col: &str, decl: &str) -> Result<(), DbError> {
    let exists = query_list_columns(conn, table)?.iter().any(|c| c == col);
    if !exists {
        conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {col} {decl}"), [])?;
    }
    Ok(())
}

fn drop_column_if_exists(conn: &Connection, table: &str, col: &str) -> Result<(), DbError> {
    let exists = query_list_columns(conn, table)?.iter().any(|c| c == col);
    if exists {
        conn.execute(&format!("ALTER TABLE {table} DROP COLUMN {col}"), [])?;
    }
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
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

impl Db {
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

    pub async fn table_columns(&self, table: &str) -> Result<Vec<String>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::ListColumns {
                table: table.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }
}
