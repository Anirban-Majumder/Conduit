//! DB-actor entry point and command dispatcher.
//!
//! A single `std::thread` owns the `rusqlite::Connection`; callers reach it
//! only through `Db`, a cheap `mpsc::Sender<DbCmd>` clone. `actor_loop` runs
//! migrations once, then dispatches each `DbCmd` synchronously to the domain
//! submodules (servers, clients, approvals, audit, skills, ...) and replies via
//! the embedded `oneshot`. The `Connection` never crosses `.await`.

pub mod types;

mod approvals;
mod audit;
mod clients;
mod commands;
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

use approvals::{
    query_create_pending_approval, query_get_pending_approval, query_list_pending_approvals,
    query_resolve_pending_approval, query_sweep_stale_pending_approvals,
};
use audit::{query_insert_audit_entry, query_purge_old_audit_entries, query_query_audit_log};
use clients::{
    query_get_client, query_list_clients, query_list_clients_managing_server,
    query_list_managed_servers, query_list_tracked_clients, query_lookup_client_key,
    query_project_builtin, query_set_client_tracked, query_set_managed_servers, query_touch_client,
};
use migrations::{init_conn, query_list_columns, query_list_tables, query_schema_version};

#[cfg(any(all(unix, not(debug_assertions)), all(windows, not(debug_assertions))))]
use migrations::restrict_db_perms;
use oauth_drafts::{delete_oauth_draft, insert_oauth_draft, list_oauth_drafts};
use servers::{
    query_clear_server_auth_error, query_commit_cascade_delete, query_delete_server,
    query_get_server, query_get_tool_decision, query_list_server_tools, query_list_servers,
    query_rename_server, query_resolve_policy, query_set_server_auth_error,
    query_set_tool_decision, query_upsert_server, query_upsert_server_with_tools,
};
use settings::{query_get_settings_kv, query_set_settings_kv};
use skills::{
    query_add_skill_target, query_delete_skill, query_delete_skill_repo, query_get_skill,
    query_list_skill_repos, query_list_skills, query_remove_skill_target,
    query_set_skill_repo_enabled, query_set_skill_targets, query_upsert_skill_metadata,
    query_upsert_skill_repo,
};

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
        dispatch(&conn, cmd);
    }
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
        DbCmd::ListColumns { table, reply } => {
            reply.send(query_list_columns(conn, &table)).ok();
        }

        // ── MCP Servers ────────────────────────────────────────────────
        DbCmd::UpsertServer { server, reply } => {
            reply.send(query_upsert_server(conn, &server)).ok();
        }
        DbCmd::GetServer { name, reply } => {
            reply.send(query_get_server(conn, &name)).ok();
        }
        DbCmd::ListServers { reply } => {
            reply.send(query_list_servers(conn)).ok();
        }
        DbCmd::DeleteServer { name, reply } => {
            reply.send(query_delete_server(conn, &name)).ok();
        }
        DbCmd::RenameServer { old, new, reply } => {
            reply.send(query_rename_server(conn, &old, &new)).ok();
        }
        DbCmd::UpsertServerWithTools {
            server,
            tools,
            reply,
        } => {
            reply
                .send(query_upsert_server_with_tools(conn, &server, &tools))
                .ok();
        }
        DbCmd::ListServerTools { server, reply } => {
            reply.send(query_list_server_tools(conn, &server)).ok();
        }
        DbCmd::SetServerAuthError {
            server,
            message,
            reply,
        } => {
            reply
                .send(query_set_server_auth_error(conn, &server, &message))
                .ok();
        }
        DbCmd::ClearServerAuthError { server, reply } => {
            reply
                .send(query_clear_server_auth_error(conn, &server))
                .ok();
        }
        DbCmd::CommitCascadeDelete {
            server_name,
            delete_audit_history,
            reply,
        } => {
            reply
                .send(query_commit_cascade_delete(
                    conn,
                    &server_name,
                    delete_audit_history,
                ))
                .ok();
        }

        // ── Tool decisions ─────────────────────────────────────────────
        DbCmd::SetToolDecision {
            server,
            tool,
            decision,
            source,
            reply,
        } => {
            reply
                .send(query_set_tool_decision(
                    conn, &server, &tool, decision, source,
                ))
                .ok();
        }
        DbCmd::GetToolDecision {
            server,
            tool,
            reply,
        } => {
            reply
                .send(query_get_tool_decision(conn, &server, &tool))
                .ok();
        }
        DbCmd::ResolvePolicy {
            server,
            tool,
            reply,
        } => {
            reply.send(query_resolve_policy(conn, &server, &tool)).ok();
        }

        // ── Clients ────────────────────────────────────────────────────
        DbCmd::ProjectBuiltin {
            id,
            name,
            mcp_config_path,
            skills_dir,
            schema_json,
            reply,
        } => {
            reply
                .send(query_project_builtin(
                    conn,
                    &id,
                    &name,
                    &mcp_config_path,
                    skills_dir.as_deref(),
                    &schema_json,
                ))
                .ok();
        }
        DbCmd::TouchClient { id, reply } => {
            reply.send(query_touch_client(conn, &id)).ok();
        }
        DbCmd::GetClient { id, reply } => {
            reply.send(query_get_client(conn, &id)).ok();
        }
        DbCmd::LookupClientKey { key, reply } => {
            reply.send(query_lookup_client_key(conn, &key)).ok();
        }
        DbCmd::ListClients { reply } => {
            reply.send(query_list_clients(conn)).ok();
        }
        DbCmd::ListTrackedClients { reply } => {
            reply.send(query_list_tracked_clients(conn)).ok();
        }
        DbCmd::SetClientTracked { id, tracked, reply } => {
            reply
                .send(query_set_client_tracked(conn, &id, tracked))
                .ok();
        }
        DbCmd::ListManagedServers { client_id, reply } => {
            reply
                .send(query_list_managed_servers(conn, &client_id))
                .ok();
        }
        DbCmd::SetManagedServers {
            client_id,
            server_names,
            reply,
        } => {
            reply
                .send(query_set_managed_servers(conn, &client_id, &server_names))
                .ok();
        }
        DbCmd::ListClientsManagingServer { server_name, reply } => {
            reply
                .send(query_list_clients_managing_server(conn, &server_name))
                .ok();
        }

        // ── Approvals ──────────────────────────────────────────────────
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

        // ── Audit ──────────────────────────────────────────────────────
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

        // ── Settings ───────────────────────────────────────────────────
        DbCmd::GetSettingsKv { key, reply } => {
            reply.send(query_get_settings_kv(conn, &key)).ok();
        }
        DbCmd::SetSettingsKv { key, value, reply } => {
            reply.send(query_set_settings_kv(conn, &key, &value)).ok();
        }

        // ── Skills ─────────────────────────────────────────────────────
        DbCmd::ListSkillRepos { reply } => {
            reply.send(query_list_skill_repos(conn)).ok();
        }
        DbCmd::UpsertSkillRepo { repo, reply } => {
            reply.send(query_upsert_skill_repo(conn, &repo)).ok();
        }
        DbCmd::DeleteSkillRepo { owner, name, reply } => {
            reply
                .send(query_delete_skill_repo(conn, &owner, &name))
                .ok();
        }
        DbCmd::SetSkillRepoEnabled {
            owner,
            name,
            enabled,
            reply,
        } => {
            reply
                .send(query_set_skill_repo_enabled(conn, &owner, &name, enabled))
                .ok();
        }
        DbCmd::UpsertSkillMetadata { skill, reply } => {
            reply.send(query_upsert_skill_metadata(conn, &skill)).ok();
        }
        DbCmd::SetSkillTargets {
            directory,
            targets,
            reply,
        } => {
            reply
                .send(query_set_skill_targets(conn, &directory, &targets))
                .ok();
        }
        DbCmd::AddSkillTarget {
            directory,
            client_id,
            reply,
        } => {
            reply
                .send(query_add_skill_target(conn, &directory, &client_id))
                .ok();
        }
        DbCmd::RemoveSkillTarget {
            directory,
            client_id,
            reply,
        } => {
            reply
                .send(query_remove_skill_target(conn, &directory, &client_id))
                .ok();
        }
        DbCmd::DeleteSkill { directory, reply } => {
            reply.send(query_delete_skill(conn, &directory)).ok();
        }
        DbCmd::GetSkill { directory, reply } => {
            reply.send(query_get_skill(conn, &directory)).ok();
        }
        DbCmd::ListSkills { reply } => {
            reply.send(query_list_skills(conn)).ok();
        }

        // ── OAuth drafts ───────────────────────────────────────────────
        DbCmd::InsertOAuthDraft { draft_uuid, reply } => {
            reply.send(insert_oauth_draft(conn, &draft_uuid)).ok();
        }
        DbCmd::DeleteOAuthDraft { draft_uuid, reply } => {
            reply.send(delete_oauth_draft(conn, &draft_uuid)).ok();
        }
        DbCmd::ListOAuthDrafts { reply } => {
            reply.send(list_oauth_drafts(conn)).ok();
        }
    }
}
