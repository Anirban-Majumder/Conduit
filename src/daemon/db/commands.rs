//! The `DbCmd` request enum sent to the DB actor.
//!
//! Each variant carries its arguments plus an embedded `oneshot::Sender` for
//! the typed reply. `mod::dispatch` matches on these and calls the matching
//! `query_*` function in a domain submodule. Adding a DB operation means
//! adding a variant here, a dispatch arm in `mod.rs`, and the query fn.

use tokio::sync::oneshot;

use crate::daemon::db::types::{
    ApprovalStatus, AuditEntry, AuditFilter, ClientRow, Decision, DecisionSource, McpServer,
    McpTool, PendingApproval, Skill, SkillRepo,
};
use crate::daemon::errors::DbError;

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
    ListColumns {
        table: String,
        reply: oneshot::Sender<Result<Vec<String>, DbError>>,
    },

    // ── MCP Servers ────────────────────────────────────────────────────
    UpsertServer {
        server: McpServer,
        reply: oneshot::Sender<Result<(), DbError>>,
    },
    GetServer {
        name: String,
        reply: oneshot::Sender<Result<Option<McpServer>, DbError>>,
    },
    ListServers {
        reply: oneshot::Sender<Result<Vec<McpServer>, DbError>>,
    },
    DeleteServer {
        name: String,
        reply: oneshot::Sender<Result<(), DbError>>,
    },
    RenameServer {
        old: String,
        new: String,
        reply: oneshot::Sender<Result<(), DbError>>,
    },
    UpsertServerWithTools {
        server: McpServer,
        tools: Vec<McpTool>,
        reply: oneshot::Sender<Result<(), DbError>>,
    },
    ListServerTools {
        server: String,
        reply: oneshot::Sender<Result<Vec<McpTool>, DbError>>,
    },
    SetServerAuthError {
        server: String,
        message: String,
        reply: oneshot::Sender<Result<bool, DbError>>,
    },
    ClearServerAuthError {
        server: String,
        reply: oneshot::Sender<Result<bool, DbError>>,
    },
    CommitCascadeDelete {
        server_name: String,
        delete_audit_history: bool,
        reply: oneshot::Sender<Result<Vec<String>, DbError>>,
    },

    // ── Tool decisions (replaces policies) ─────────────────────────────
    SetToolDecision {
        server: String,
        tool: String,
        decision: Decision,
        source: DecisionSource,
        reply: oneshot::Sender<Result<(), DbError>>,
    },
    GetToolDecision {
        server: String,
        tool: String,
        reply: oneshot::Sender<Result<Option<Decision>, DbError>>,
    },
    ResolvePolicy {
        server: String,
        tool: String,
        reply: oneshot::Sender<Result<Decision, DbError>>,
    },

    // ── Clients (replaces ides) ────────────────────────────────────────
    ProjectBuiltin {
        id: String,
        name: String,
        mcp_config_path: String,
        skills_dir: Option<String>,
        schema_json: String,
        reply: oneshot::Sender<Result<ClientRow, DbError>>,
    },
    GetClient {
        id: String,
        reply: oneshot::Sender<Result<Option<ClientRow>, DbError>>,
    },
    LookupClientKey {
        key: String,
        reply: oneshot::Sender<Result<Option<ClientRow>, DbError>>,
    },
    ListClients {
        reply: oneshot::Sender<Result<Vec<ClientRow>, DbError>>,
    },
    ListTrackedClients {
        reply: oneshot::Sender<Result<Vec<ClientRow>, DbError>>,
    },
    SetClientTracked {
        id: String,
        tracked: bool,
        reply: oneshot::Sender<Result<(), DbError>>,
    },
    ListManagedServers {
        client_id: String,
        reply: oneshot::Sender<Result<Vec<String>, DbError>>,
    },
    SetManagedServers {
        client_id: String,
        server_names: Vec<String>,
        reply: oneshot::Sender<Result<(), DbError>>,
    },
    ListAllManagedMcp {
        reply: oneshot::Sender<Result<Vec<(String, Vec<String>)>, DbError>>,
    },

    // ── Approvals ──────────────────────────────────────────────────────
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
        reply: oneshot::Sender<Result<Vec<String>, DbError>>,
    },

    // ── Audit ──────────────────────────────────────────────────────────
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

    // ── Settings ───────────────────────────────────────────────────────
    GetSettingsKv {
        key: String,
        reply: oneshot::Sender<Result<Option<String>, DbError>>,
    },
    SetSettingsKv {
        key: String,
        value: String,
        reply: oneshot::Sender<Result<(), DbError>>,
    },

    // ── Skills ─────────────────────────────────────────────────────────
    ListSkillRepos {
        reply: oneshot::Sender<Result<Vec<SkillRepo>, DbError>>,
    },
    UpsertSkillRepo {
        repo: SkillRepo,
        reply: oneshot::Sender<Result<(), DbError>>,
    },
    DeleteSkillRepo {
        owner: String,
        name: String,
        reply: oneshot::Sender<Result<(), DbError>>,
    },
    SetSkillRepoEnabled {
        owner: String,
        name: String,
        enabled: bool,
        reply: oneshot::Sender<Result<(), DbError>>,
    },
    UpsertSkillWithTargets {
        skill: Skill,
        reply: oneshot::Sender<Result<(), DbError>>,
    },
    SetSkillTargets {
        directory: String,
        targets: Vec<String>,
        reply: oneshot::Sender<Result<(), DbError>>,
    },
    DeleteSkill {
        directory: String,
        reply: oneshot::Sender<Result<(), DbError>>,
    },
    GetSkill {
        directory: String,
        reply: oneshot::Sender<Result<Option<Skill>, DbError>>,
    },
    ListSkills {
        reply: oneshot::Sender<Result<Vec<Skill>, DbError>>,
    },

    // ── OAuth drafts ───────────────────────────────────────────────────
    InsertOAuthDraft {
        draft_uuid: String,
        reply: oneshot::Sender<Result<(), DbError>>,
    },
    DeleteOAuthDraft {
        draft_uuid: String,
        reply: oneshot::Sender<Result<(), DbError>>,
    },
    ListOAuthDrafts {
        reply: oneshot::Sender<Result<Vec<String>, DbError>>,
    },
}
