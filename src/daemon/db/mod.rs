//! DB-actor entry point and command dispatcher.
//!
//! A single `std::thread` owns the `rusqlite::Connection`; callers reach it
//! only through `Db`, a cheap `mpsc::Sender<DbCmd>` clone. `actor_loop` runs
//! migrations once, then dispatches each `DbCmd` synchronously to the domain
//! submodules (servers, policy, approvals, audit, ides, skills, ...) and
//! replies via the embedded `oneshot`. The `Connection` never crosses `.await`.

pub mod types;

mod approvals;
mod audit;
mod commands;
mod ides;
mod migrations;
mod oauth_drafts;
mod policy;
mod servers;
mod settings;
mod skills;

use std::path::Path;
use std::thread;

use rusqlite::Connection;
use tokio::sync::{mpsc, oneshot};

use crate::daemon::errors::DbError;

pub use commands::{CascadeTrim, DbCmd};

use approvals::{
    query_create_pending_approval, query_get_pending_approval, query_list_pending_approvals,
    query_resolve_pending_approval, query_sweep_stale_pending_approvals,
};
use audit::{query_insert_audit_entry, query_purge_old_audit_entries, query_query_audit_log};
use ides::{
    query_get_ide_by_kind, query_list_active_ides, query_list_all_ide_configs, query_list_ides,
    query_lookup_ide_key, query_mark_ide_not_tracked, query_mark_ides_stale, query_retrack_ide,
    query_update_ide_managed_entries, query_upsert_detected_ide,
};
use migrations::{init_conn, query_list_columns, query_list_tables, query_schema_version};

#[cfg(any(
    all(unix, not(debug_assertions)),
    all(windows, not(debug_assertions))
))]
use migrations::restrict_db_perms;
use oauth_drafts::{delete_oauth_draft, insert_oauth_draft, list_oauth_drafts};
use policy::{
    query_delete_policies_for_server, query_get_policy, query_get_policy_scoped,
    query_list_all_policies, query_list_policies_for_server, query_set_policy,
};
use servers::{
    query_clear_server_auth_error, query_commit_cascade_delete, query_delete_server,
    query_get_server, query_list_server_tools, query_list_servers, query_rename_server,
    query_set_server_auth_error, query_upsert_server, query_upsert_server_with_tools,
};
use settings::{query_get_settings_kv, query_set_settings_kv};
use skills::{
    query_delete_skill, query_delete_skill_repo, query_get_skill, query_list_skill_repos,
    query_list_skills, query_set_skill_repo_enabled, query_set_skill_targets,
    query_upsert_skill_repo, query_upsert_skill_with_targets,
};

const CMD_CHANNEL_DEPTH: usize = 64;

pub(super) fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    i64::try_from(secs).unwrap_or(i64::MAX)
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

    fn spawn_with_inner(conn: Connection, db_path: Option<std::path::PathBuf>) -> Result<Self, DbError> {
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
}

fn actor_loop(conn: Connection, mut rx: mpsc::Receiver<DbCmd>, db_path: Option<std::path::PathBuf>) {
    if let Err(e) = init_conn(&conn) {
        tracing::error!(error = %e, "db actor init failed; refusing further commands");
        return;
    }
    // Restrict perms AFTER init_conn — WAL mode + migrations create -wal/-shm
    // sidecars that must also be locked down.
    #[cfg(any(
        all(unix, not(debug_assertions)),
        all(windows, not(debug_assertions))
    ))]
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
        DbCmd::LookupIdeKey { key, reply } => {
            reply.send(query_lookup_ide_key(conn, &key)).ok();
        }
        DbCmd::UpsertDetectedIde {
            ide_kind,
            display_name,
            detection_source,
            reply,
        } => {
            reply
                .send(query_upsert_detected_ide(
                    conn,
                    &ide_kind,
                    &display_name,
                    detection_source.as_deref(),
                ))
                .ok();
        }
        DbCmd::ListIdes { reply } => {
            reply.send(query_list_ides(conn)).ok();
        }
        DbCmd::ListActiveIdes { reply } => {
            reply.send(query_list_active_ides(conn)).ok();
        }
        DbCmd::GetIdeByKind { ide_kind, reply } => {
            reply.send(query_get_ide_by_kind(conn, &ide_kind)).ok();
        }
        DbCmd::MarkIdesStale {
            kinds_present,
            reply,
        } => {
            reply.send(query_mark_ides_stale(conn, &kinds_present)).ok();
        }
        DbCmd::MarkIdeNotTracked { ide_kind, reply } => {
            reply.send(query_mark_ide_not_tracked(conn, &ide_kind)).ok();
        }
        DbCmd::RetrackIde { ide_kind, reply } => {
            reply.send(query_retrack_ide(conn, &ide_kind)).ok();
        }
        DbCmd::UpdateIdeManagedEntries {
            ide_kind,
            config_path,
            managed_entries,
            reply,
        } => {
            reply
                .send(query_update_ide_managed_entries(
                    conn,
                    &ide_kind,
                    &config_path,
                    &managed_entries,
                ))
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
        DbCmd::GetPolicyScoped {
            server,
            tool,
            scope,
            reply,
        } => {
            reply
                .send(query_get_policy_scoped(conn, &server, &tool, &scope))
                .ok();
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
        DbCmd::GetSettingsKv { key, reply } => {
            reply.send(query_get_settings_kv(conn, &key)).ok();
        }
        DbCmd::ListColumns { table, reply } => {
            reply.send(query_list_columns(conn, &table)).ok();
        }
        DbCmd::SetSettingsKv { key, value, reply } => {
            reply.send(query_set_settings_kv(conn, &key, &value)).ok();
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
        DbCmd::RenameServer { old, new, reply } => {
            reply.send(query_rename_server(conn, &old, &new)).ok();
        }
        DbCmd::ListAllIdeConfigs { reply } => {
            reply.send(query_list_all_ide_configs(conn)).ok();
        }
        DbCmd::CommitCascadeDelete {
            server_name,
            trimmed,
            delete_audit_history,
            reply,
        } => {
            reply
                .send(query_commit_cascade_delete(
                    conn,
                    &server_name,
                    &trimmed,
                    delete_audit_history,
                ))
                .ok();
        }
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
        DbCmd::UpsertSkillWithTargets { skill, reply } => {
            reply
                .send(query_upsert_skill_with_targets(conn, &skill))
                .ok();
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
        DbCmd::DeleteSkill { directory, reply } => {
            reply.send(query_delete_skill(conn, &directory)).ok();
        }
        DbCmd::GetSkill { directory, reply } => {
            reply.send(query_get_skill(conn, &directory)).ok();
        }
        DbCmd::ListSkills { reply } => {
            reply.send(query_list_skills(conn)).ok();
        }
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
