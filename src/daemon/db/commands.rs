//! The `DbCmd` request enum sent to the DB actor.
//!
//! Each variant carries its arguments plus an embedded `oneshot::Sender` for
//! the typed reply. `mod::dispatch` matches on these and calls the matching
//! `query_*` function in a domain submodule. Adding a DB operation means
//! adding a variant here, a dispatch arm in `mod.rs`, and the query fn.

use std::path::PathBuf;
use tokio::sync::oneshot;

use crate::daemon::db::types::{
    ApprovalStatus, AuditEntry, AuditFilter, Decision, DecisionSource, IdeKey, IdeRecord,
    ManagedIdeConfig, PendingApproval, Policy, Server, Skill, SkillRepo, ToolMeta,
};
use crate::daemon::errors::DbError;

#[derive(Debug, Clone)]
pub struct CascadeTrim {
    pub ide_kind: String,
    pub remaining: Vec<String>,
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
    LookupIdeKey {
        key: String,
        reply: oneshot::Sender<Result<Option<IdeKey>, DbError>>,
    },
    UpsertDetectedIde {
        ide_kind: String,
        display_name: String,
        detection_source: Option<String>,
        reply: oneshot::Sender<Result<IdeRecord, DbError>>,
    },
    ListIdes {
        reply: oneshot::Sender<Result<Vec<IdeRecord>, DbError>>,
    },
    ListActiveIdes {
        reply: oneshot::Sender<Result<Vec<IdeRecord>, DbError>>,
    },
    GetIdeByKind {
        ide_kind: String,
        reply: oneshot::Sender<Result<Option<IdeRecord>, DbError>>,
    },
    MarkIdesStale {
        kinds_present: Vec<String>,
        reply: oneshot::Sender<Result<(), DbError>>,
    },
    MarkIdeNotTracked {
        ide_kind: String,
        reply: oneshot::Sender<Result<(), DbError>>,
    },
    RetrackIde {
        ide_kind: String,
        reply: oneshot::Sender<Result<(), DbError>>,
    },
    UpdateIdeManagedEntries {
        ide_kind: String,
        config_path: PathBuf,
        managed_entries: Vec<String>,
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
    GetPolicyScoped {
        server: String,
        tool: String,
        scope: String,
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
        reply: oneshot::Sender<Result<Vec<String>, DbError>>,
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
    UpsertServer {
        server: Server,
        reply: oneshot::Sender<Result<(), DbError>>,
    },
    GetServer {
        name: String,
        reply: oneshot::Sender<Result<Option<Server>, DbError>>,
    },
    ListServers {
        reply: oneshot::Sender<Result<Vec<Server>, DbError>>,
    },
    DeleteServer {
        name: String,
        reply: oneshot::Sender<Result<(), DbError>>,
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
    GetSettingsKv {
        key: String,
        reply: oneshot::Sender<Result<Option<String>, DbError>>,
    },
    ListColumns {
        table: String,
        reply: oneshot::Sender<Result<Vec<String>, DbError>>,
    },
    SetSettingsKv {
        key: String,
        value: String,
        reply: oneshot::Sender<Result<(), DbError>>,
    },
    UpsertServerWithTools {
        server: Server,
        tools: Vec<ToolMeta>,
        reply: oneshot::Sender<Result<(), DbError>>,
    },
    ListServerTools {
        server: String,
        reply: oneshot::Sender<Result<Vec<ToolMeta>, DbError>>,
    },
    RenameServer {
        old: String,
        new: String,
        reply: oneshot::Sender<Result<(), DbError>>,
    },
    ListAllIdeConfigs {
        reply: oneshot::Sender<Result<Vec<ManagedIdeConfig>, DbError>>,
    },
    CommitCascadeDelete {
        server_name: String,
        trimmed: Vec<CascadeTrim>,
        delete_audit_history: bool,
        reply: oneshot::Sender<Result<Vec<String>, DbError>>,
    },
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
