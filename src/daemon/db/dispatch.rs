//! `DbCmd` dispatch: the synchronous match the DB actor runs per command.
//!
//! `actor_loop` (in `mod.rs`) hands each received `DbCmd` to `dispatch`, which
//! calls the matching `query_*` in a domain submodule and replies via the
//! command's embedded `oneshot`. Split out of `mod.rs` so the actor loop and
//! `Db` handle stay readable; adding a DB op means a variant in `commands`, a
//! query fn in a submodule, and one arm here.

use rusqlite::Connection;

use super::commands::DbCmd;

use super::approvals::{
    query_create_pending_approval, query_get_pending_approval, query_list_pending_approvals,
    query_resolve_pending_approval, query_sweep_stale_pending_approvals,
};
use super::audit::{
    query_insert_audit_entry, query_purge_old_audit_entries, query_query_audit_log,
};
use super::clients::{
    query_add_managed_server, query_get_client, query_list_clients,
    query_list_clients_managing_server, query_list_known_repos, query_list_managed_servers,
    query_list_scopes_for_client, query_list_tracked_clients, query_lookup_client_key,
    query_project_builtin, query_remove_managed_server, query_remove_server_all_scopes,
    query_set_client_tracked, query_set_managed_servers, query_touch_client,
};
use super::migrations::{query_list_columns, query_list_tables, query_schema_version};
use super::servers::{
    query_clear_server_auth_error, query_commit_cascade_delete, query_delete_server,
    query_get_server, query_get_tool_decision, query_list_all_tools, query_list_server_tools,
    query_list_servers, query_rename_server, query_resolve_policy, query_set_server_auth_error,
    query_set_tool_decision, query_upsert_server, query_upsert_server_with_tools,
};
use super::settings::{query_get_settings_kv, query_set_settings_kv};
use super::skills::{
    query_add_skill_target, query_delete_skill, query_delete_skill_repo, query_get_skill,
    query_list_skill_repos, query_list_skill_targets_scoped, query_list_skills,
    query_remove_skill_all_scopes, query_remove_skill_target, query_set_skill_repo_enabled,
    query_set_skill_targets, query_upsert_skill_metadata, query_upsert_skill_repo,
};

#[expect(clippy::too_many_lines, reason = "thin match over many DbCmd variants")]
pub(super) fn dispatch(conn: &Connection, cmd: DbCmd) {
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
        DbCmd::ListAllTools { reply } => {
            reply.send(query_list_all_tools(conn)).ok();
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
            default,
            reply,
        } => {
            reply
                .send(query_resolve_policy(conn, &server, &tool, default))
                .ok();
        }

        // ── Clients ────────────────────────────────────────────────────
        DbCmd::ProjectBuiltin {
            id,
            name,
            mcp_config_path,
            skills_dir,
            schema_json,
            project_config_rel,
            project_skills_rel,
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
                    project_config_rel.as_deref(),
                    project_skills_rel.as_deref(),
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
        DbCmd::ListManagedServers {
            client_id,
            scope,
            reply,
        } => {
            reply
                .send(query_list_managed_servers(conn, &client_id, &scope))
                .ok();
        }
        DbCmd::SetManagedServers {
            client_id,
            scope,
            server_names,
            reply,
        } => {
            reply
                .send(query_set_managed_servers(
                    conn,
                    &client_id,
                    &scope,
                    &server_names,
                ))
                .ok();
        }
        DbCmd::AddManagedServer {
            client_id,
            scope,
            server_name,
            reply,
        } => {
            reply
                .send(query_add_managed_server(
                    conn,
                    &client_id,
                    &scope,
                    &server_name,
                ))
                .ok();
        }
        DbCmd::RemoveManagedServer {
            client_id,
            scope,
            server_name,
            reply,
        } => {
            reply
                .send(query_remove_managed_server(
                    conn,
                    &client_id,
                    &scope,
                    &server_name,
                ))
                .ok();
        }
        DbCmd::RemoveServerAllScopes {
            client_id,
            server_name,
            reply,
        } => {
            reply
                .send(query_remove_server_all_scopes(
                    conn,
                    &client_id,
                    &server_name,
                ))
                .ok();
        }
        DbCmd::ListClientsManagingServer { server_name, reply } => {
            reply
                .send(query_list_clients_managing_server(conn, &server_name))
                .ok();
        }
        DbCmd::ListKnownRepos { reply } => {
            reply.send(query_list_known_repos(conn)).ok();
        }
        DbCmd::ListScopesForClient { client_id, reply } => {
            reply
                .send(query_list_scopes_for_client(conn, &client_id))
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
            scope,
            targets,
            reply,
        } => {
            reply
                .send(query_set_skill_targets(conn, &directory, &scope, &targets))
                .ok();
        }
        DbCmd::AddSkillTarget {
            directory,
            client_id,
            scope,
            reply,
        } => {
            reply
                .send(query_add_skill_target(conn, &directory, &client_id, &scope))
                .ok();
        }
        DbCmd::RemoveSkillTarget {
            directory,
            client_id,
            scope,
            reply,
        } => {
            reply
                .send(query_remove_skill_target(
                    conn, &directory, &client_id, &scope,
                ))
                .ok();
        }
        DbCmd::RemoveSkillAllScopes {
            directory,
            client_id,
            reply,
        } => {
            reply
                .send(query_remove_skill_all_scopes(conn, &directory, &client_id))
                .ok();
        }
        DbCmd::ListSkillTargetsScoped { directory, reply } => {
            reply
                .send(query_list_skill_targets_scoped(conn, &directory))
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
    }
}
