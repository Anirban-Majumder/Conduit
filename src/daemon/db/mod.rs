pub mod types;

use std::path::{Path, PathBuf};
use std::thread;

use rusqlite::{params, params_from_iter, types::Value as SqlValue, Connection, OptionalExtension};
use tokio::sync::{mpsc, oneshot};

use crate::daemon::db::types::{
    fresh_ide_key, ApprovalStatus, AuditDecision, AuditEntry, AuditFilter, Decision,
    DecisionSource, EnvEntry, IdeKey, IdeRecord, IdeStatus, ManagedIdeConfig, PendingApproval,
    Policy, Server, Skill, SkillRepo, ToolMeta,
};
use crate::daemon::errors::DbError;

const INIT_SQL: &str = include_str!("migrations/0001_init.sql");
const V01_SQL: &str = include_str!("migrations/0002_v01.sql");
const SKILLS_SQL: &str = include_str!("migrations/0003_skills.sql");
const IDES_SQL: &str = include_str!("migrations/0004_ides.sql");
const OAUTH_DRAFTS_SQL: &str = include_str!("migrations/0005_oauth_drafts.sql");

#[derive(Debug, Clone)]
pub struct CascadeTrim {
    pub ide_kind: String,
    pub remaining: Vec<String>,
}
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

#[derive(Debug, Clone)]
pub struct Db {
    tx: mpsc::Sender<DbCmd>,
}

impl Db {
    pub fn spawn_at(path: &Path) -> Result<Self, DbError> {
        let conn = Connection::open(path)?;
        #[cfg(all(unix, not(debug_assertions)))]
        restrict_db_perms(path);
        Self::spawn_with(conn)
    }

    pub fn spawn_in_memory() -> Result<Self, DbError> {
        let conn = Connection::open_in_memory()?;
        Self::spawn_with(conn)
    }

    fn spawn_with(conn: Connection) -> Result<Self, DbError> {
        let (tx, rx) = mpsc::channel::<DbCmd>(CMD_CHANNEL_DEPTH);
        thread::Builder::new()
            .name("conduit-db".into())
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

    pub async fn upsert_detected_ide(
        &self,
        ide_kind: &str,
        display_name: &str,
        detection_source: Option<String>,
    ) -> Result<IdeRecord, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::UpsertDetectedIde {
                ide_kind: ide_kind.to_string(),
                display_name: display_name.to_string(),
                detection_source,
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn list_ides(&self) -> Result<Vec<IdeRecord>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::ListIdes { reply })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn list_active_ides(&self) -> Result<Vec<IdeRecord>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::ListActiveIdes { reply })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn get_ide_by_kind(&self, ide_kind: &str) -> Result<Option<IdeRecord>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::GetIdeByKind {
                ide_kind: ide_kind.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn mark_ides_stale(&self, kinds_present: Vec<String>) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::MarkIdesStale {
                kinds_present,
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn mark_ide_not_tracked(&self, ide_kind: &str) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::MarkIdeNotTracked {
                ide_kind: ide_kind.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn retrack_ide(&self, ide_kind: &str) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::RetrackIde {
                ide_kind: ide_kind.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn update_ide_managed_entries(
        &self,
        ide_kind: &str,
        config_path: PathBuf,
        managed_entries: Vec<String>,
    ) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::UpdateIdeManagedEntries {
                ide_kind: ide_kind.to_string(),
                config_path,
                managed_entries,
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn set_policy(
        &self,
        server: &str,
        tool: &str,
        decision: Decision,
        decision_source: DecisionSource,
    ) -> Result<i64, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::SetPolicy {
                server: server.to_string(),
                tool: tool.to_string(),
                decision,
                decision_source,
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    /// Three-tier lookup: (server, tool, 'tool') → (server, '', 'server') →
    /// `settings_kv.default_unknown_tool_policy`. Returns the first match.
    pub async fn resolve_policy(&self, server: &str, tool: &str) -> Result<Decision, DbError> {
        if let Some(p) = self.get_policy(server, tool).await? {
            return Ok(p.decision);
        }
        if let Some(p) = self.get_policy_scoped(server, "", "server").await? {
            return Ok(p.decision);
        }
        let global = self
            .get_settings_kv("default_unknown_tool_policy")
            .await?
            .as_deref()
            .and_then(Decision::from_db_str)
            .unwrap_or(Decision::Ask);
        Ok(global)
    }

    pub async fn get_policy_scoped(
        &self,
        server: &str,
        tool: &str,
        scope: &str,
    ) -> Result<Option<Policy>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::GetPolicyScoped {
                server: server.to_string(),
                tool: tool.to_string(),
                scope: scope.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn get_policy(&self, server: &str, tool: &str) -> Result<Option<Policy>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::GetPolicy {
                server: server.to_string(),
                tool: tool.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn list_policies_for_server(&self, server: &str) -> Result<Vec<Policy>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::ListPoliciesForServer {
                server: server.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn list_all_policies(&self) -> Result<Vec<Policy>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::ListAllPolicies { reply })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn delete_policies_for_server(&self, server: &str) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::DeletePoliciesForServer {
                server: server.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn create_pending_approval(&self, approval: PendingApproval) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::CreatePendingApproval { approval, reply })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn get_pending_approval(&self, id: &str) -> Result<Option<PendingApproval>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::GetPendingApproval {
                id: id.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn resolve_pending_approval(
        &self,
        id: &str,
        new_status: ApprovalStatus,
    ) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::ResolvePendingApproval {
                id: id.to_string(),
                new_status,
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn list_pending_approvals(&self) -> Result<Vec<PendingApproval>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::ListPendingApprovals { reply })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    /// Flips every `pending` row older than `older_than_ts` to `timeout`.
    /// Returns the list of ids that were swept so the caller can publish
    /// per-id `Event::ApprovalResolved` events.
    pub async fn sweep_stale_pending_approvals(
        &self,
        older_than_ts: i64,
    ) -> Result<Vec<String>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::SweepStalePendingApprovals {
                older_than_ts,
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn insert_audit_entry(&self, entry: AuditEntry) -> Result<i64, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::InsertAuditEntry { entry, reply })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn query_audit_log(&self, filter: AuditFilter) -> Result<Vec<AuditEntry>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::QueryAuditLog { filter, reply })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn purge_old_audit_entries(&self, older_than_ts: i64) -> Result<u64, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::PurgeOldAuditEntries {
                older_than_ts,
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn upsert_server(&self, server: Server) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::UpsertServer { server, reply })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn get_server(&self, name: &str) -> Result<Option<Server>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::GetServer {
                name: name.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn list_servers(&self) -> Result<Vec<Server>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::ListServers { reply })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn delete_server(&self, name: &str) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::DeleteServer {
                name: name.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    /// Returns `true` if this is a transition (prior auth-error was NULL or different).
    pub async fn set_server_auth_error(
        &self,
        server: &str,
        message: &str,
    ) -> Result<bool, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::SetServerAuthError {
                server: server.to_string(),
                message: message.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    /// Returns `true` if a non-null auth-error was actually cleared.
    pub async fn clear_server_auth_error(&self, server: &str) -> Result<bool, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::ClearServerAuthError {
                server: server.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn get_settings_kv(&self, key: &str) -> Result<Option<String>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::GetSettingsKv {
                key: key.to_string(),
                reply,
            })
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

    pub async fn set_settings_kv(&self, key: &str, value: &str) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::SetSettingsKv {
                key: key.to_string(),
                value: value.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    /// Atomic Save for the Add Server wizard: inserts the server row, per-tool
    /// policies (defaulting to `ask`/`probe`), the server-wide fallback policy
    /// (`scope='server'`), and one `server_tools` row per probed tool. All in
    /// a single transaction.
    pub async fn upsert_server_with_tools(
        &self,
        server: Server,
        tools: Vec<ToolMeta>,
    ) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::UpsertServerWithTools {
                server,
                tools,
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn list_server_tools(&self, server: &str) -> Result<Vec<ToolMeta>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::ListServerTools {
                server: server.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    /// Rename a server. Cascades through `policies` and `server_tools` via FK
    /// ON UPDATE CASCADE. Caller rewrites IDE config files.
    pub async fn rename_server(&self, old: &str, new: &str) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::RenameServer {
                old: old.to_string(),
                new: new.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn list_all_ide_configs(&self) -> Result<Vec<ManagedIdeConfig>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::ListAllIdeConfigs { reply })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn commit_cascade_delete(
        &self,
        server_name: &str,
        trimmed: Vec<CascadeTrim>,
        delete_audit_history: bool,
    ) -> Result<Vec<String>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::CommitCascadeDelete {
                server_name: server_name.to_string(),
                trimmed,
                delete_audit_history,
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn list_skill_repos(&self) -> Result<Vec<SkillRepo>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::ListSkillRepos { reply })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn upsert_skill_repo(&self, repo: SkillRepo) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::UpsertSkillRepo { repo, reply })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn delete_skill_repo(&self, owner: &str, name: &str) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::DeleteSkillRepo {
                owner: owner.to_string(),
                name: name.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn set_skill_repo_enabled(
        &self,
        owner: &str,
        name: &str,
        enabled: bool,
    ) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::SetSkillRepoEnabled {
                owner: owner.to_string(),
                name: name.to_string(),
                enabled,
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn upsert_skill_with_targets(&self, skill: Skill) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::UpsertSkillWithTargets { skill, reply })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn set_skill_targets(
        &self,
        directory: &str,
        targets: Vec<String>,
    ) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::SetSkillTargets {
                directory: directory.to_string(),
                targets,
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn delete_skill(&self, directory: &str) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::DeleteSkill {
                directory: directory.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn get_skill(&self, directory: &str) -> Result<Option<Skill>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::GetSkill {
                directory: directory.to_string(),
                reply,
            })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn list_skills(&self) -> Result<Vec<Skill>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::ListSkills { reply })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn insert_oauth_draft(&self, draft_uuid: String) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::InsertOAuthDraft { draft_uuid, reply })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn delete_oauth_draft(&self, draft_uuid: String) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::DeleteOAuthDraft { draft_uuid, reply })
            .await
            .map_err(|_closed| DbError::ActorGone)?;
        rx.await.map_err(|_closed| DbError::ActorGone)?
    }

    pub async fn list_oauth_drafts(&self) -> Result<Vec<String>, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DbCmd::ListOAuthDrafts { reply })
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

#[cfg(all(unix, not(debug_assertions)))]
fn restrict_db_perms(path: &Path) {
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

fn init_conn(conn: &Connection) -> Result<(), DbError> {
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
    ensure_column(conn, "servers", "oauth_authority", "TEXT")?;
    ensure_column(conn, "servers", "oauth_client_id", "TEXT")?;
    ensure_column(conn, "servers", "oauth_client_secret_ref", "TEXT")?;
    ensure_column(conn, "servers", "oauth_scopes", "TEXT")?;
    ensure_column(conn, "servers", "last_auth_error_msg", "TEXT")?;
    ensure_column(conn, "servers", "last_auth_error_at", "INTEGER")?;
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

fn query_list_skill_repos(conn: &Connection) -> Result<Vec<SkillRepo>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT owner, name, branch, enabled FROM skill_repos ORDER BY owner, name",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(SkillRepo {
            owner: row.get(0)?,
            name: row.get(1)?,
            branch: row.get(2)?,
            enabled: row.get::<_, i64>(3)? != 0,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn query_upsert_skill_repo(conn: &Connection, repo: &SkillRepo) -> Result<(), DbError> {
    conn.prepare_cached(
        "INSERT INTO skill_repos (owner, name, branch, enabled) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(owner, name) DO UPDATE SET
             branch = excluded.branch,
             enabled = excluded.enabled",
    )?
    .execute(params![
        &repo.owner,
        &repo.name,
        &repo.branch,
        i64::from(repo.enabled),
    ])?;
    Ok(())
}

fn query_delete_skill_repo(conn: &Connection, owner: &str, name: &str) -> Result<(), DbError> {
    conn.prepare_cached("DELETE FROM skill_repos WHERE owner = ?1 AND name = ?2")?
        .execute(params![owner, name])?;
    Ok(())
}

fn query_set_skill_repo_enabled(
    conn: &Connection,
    owner: &str,
    name: &str,
    enabled: bool,
) -> Result<(), DbError> {
    conn.prepare_cached("UPDATE skill_repos SET enabled = ?1 WHERE owner = ?2 AND name = ?3")?
        .execute(params![i64::from(enabled), owner, name])?;
    Ok(())
}

fn query_upsert_skill_with_targets(conn: &Connection, skill: &Skill) -> Result<(), DbError> {
    let tx = conn.unchecked_transaction()?;
    tx.prepare_cached(
        "INSERT INTO skills
             (directory, repo_owner, repo_name, repo_branch, source_directory,
              skill_name, description, readme_url, installed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(directory) DO UPDATE SET
             repo_owner = excluded.repo_owner,
             repo_name = excluded.repo_name,
             repo_branch = excluded.repo_branch,
             source_directory = excluded.source_directory,
             skill_name = excluded.skill_name,
             description = excluded.description,
             readme_url = excluded.readme_url,
             installed_at = excluded.installed_at",
    )?
    .execute(params![
        &skill.directory,
        &skill.repo_owner,
        &skill.repo_name,
        &skill.repo_branch,
        &skill.source_directory,
        &skill.skill_name,
        &skill.description,
        &skill.readme_url,
        skill.installed_at,
    ])?;

    tx.prepare_cached("DELETE FROM skill_targets WHERE directory = ?1")?
        .execute(params![&skill.directory])?;
    for ide in &skill.targets {
        tx.prepare_cached(
            "INSERT OR IGNORE INTO skill_targets (directory, ide_name) VALUES (?1, ?2)",
        )?
        .execute(params![&skill.directory, ide])?;
    }
    tx.commit()?;
    Ok(())
}

fn query_set_skill_targets(
    conn: &Connection,
    directory: &str,
    targets: &[String],
) -> Result<(), DbError> {
    let tx = conn.unchecked_transaction()?;
    tx.prepare_cached("DELETE FROM skill_targets WHERE directory = ?1")?
        .execute(params![directory])?;
    for ide in targets {
        tx.prepare_cached(
            "INSERT OR IGNORE INTO skill_targets (directory, ide_name) VALUES (?1, ?2)",
        )?
        .execute(params![directory, ide])?;
    }
    tx.commit()?;
    Ok(())
}

fn query_delete_skill(conn: &Connection, directory: &str) -> Result<(), DbError> {
    conn.prepare_cached("DELETE FROM skills WHERE directory = ?1")?
        .execute(params![directory])?;
    Ok(())
}

fn read_skill_targets(conn: &Connection, directory: &str) -> Result<Vec<String>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT ide_name FROM skill_targets WHERE directory = ?1 ORDER BY ide_name",
    )?;
    let rows = stmt.query_map(params![directory], |row| row.get::<_, String>(0))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn decode_skill_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Skill> {
    Ok(Skill {
        directory: row.get(0)?,
        repo_owner: row.get(1)?,
        repo_name: row.get(2)?,
        repo_branch: row.get(3)?,
        source_directory: row.get(4)?,
        skill_name: row.get(5)?,
        description: row.get(6)?,
        readme_url: row.get(7)?,
        installed_at: row.get(8)?,
        targets: Vec::new(),
    })
}

fn query_get_skill(conn: &Connection, directory: &str) -> Result<Option<Skill>, DbError> {
    let row = conn
        .prepare_cached(
            "SELECT directory, repo_owner, repo_name, repo_branch, source_directory,
                    skill_name, description, readme_url, installed_at
             FROM skills WHERE directory = ?1",
        )?
        .query_row(params![directory], decode_skill_row)
        .optional()?;
    match row {
        Some(mut s) => {
            s.targets = read_skill_targets(conn, &s.directory)?;
            Ok(Some(s))
        }
        None => Ok(None),
    }
}

fn query_list_skills(conn: &Connection) -> Result<Vec<Skill>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT directory, repo_owner, repo_name, repo_branch, source_directory,
                skill_name, description, readme_url, installed_at
         FROM skills ORDER BY skill_name",
    )?;
    let rows = stmt.query_map([], decode_skill_row)?;
    let mut out = Vec::new();
    for r in rows {
        let mut s = r?;
        s.targets = read_skill_targets(conn, &s.directory)?;
        out.push(s);
    }
    Ok(out)
}

fn query_list_all_ide_configs(conn: &Connection) -> Result<Vec<ManagedIdeConfig>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT ide_kind, ide_key, config_path, managed_entries_json, last_seen_at
         FROM ides
         WHERE config_path IS NOT NULL AND status != 'not_tracked'
         ORDER BY ide_kind",
    )?;
    let rows = stmt.query_map([], |row| {
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
        let (ide_kind, ide_key, config_path, entries_json, last_seen_at) = r?;
        let managed_entries: Vec<String> =
            serde_json::from_str(&entries_json).map_err(|e| DbError::Encoding(e.to_string()))?;
        out.push(ManagedIdeConfig {
            ide_name: ide_kind,
            ide_key,
            config_path,
            managed_entries,
            written_at: last_seen_at,
        });
    }
    Ok(out)
}

fn query_commit_cascade_delete(
    conn: &Connection,
    server_name: &str,
    trimmed: &[CascadeTrim],
    delete_audit_history: bool,
) -> Result<Vec<String>, DbError> {
    let tx = conn.unchecked_transaction()?;
    let now = now_unix();
    for trim in trimmed {
        let json =
            serde_json::to_string(&trim.remaining).map_err(|e| DbError::Encoding(e.to_string()))?;
        tx.prepare_cached(
            "UPDATE ides
             SET managed_entries_json = ?1, last_seen_at = ?2
             WHERE ide_kind = ?3",
        )?
        .execute(params![json, now, trim.ide_kind])?;
    }

    let mut cancelled_ids: Vec<String> = Vec::new();
    {
        let mut stmt = tx.prepare_cached(
            "SELECT id FROM pending_approvals WHERE server = ?1 AND status = 'pending'",
        )?;
        let rows = stmt.query_map(params![server_name], |row| row.get::<_, String>(0))?;
        for r in rows {
            cancelled_ids.push(r?);
        }
    }
    if !cancelled_ids.is_empty() {
        let placeholders = cancelled_ids
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "UPDATE pending_approvals SET status = 'cancelled' WHERE id IN ({placeholders})"
        );
        let mut params_vec: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(cancelled_ids.len());
        for id in &cancelled_ids {
            params_vec.push(id);
        }
        tx.prepare(&sql)?.execute(&params_vec[..])?;
    }

    if delete_audit_history {
        tx.prepare_cached("DELETE FROM audit_log WHERE server = ?1")?
            .execute(params![server_name])?;
    }

    tx.prepare_cached("DELETE FROM servers WHERE name = ?1")?
        .execute(params![server_name])?;
    tx.commit()?;
    Ok(cancelled_ids)
}

fn query_rename_server(conn: &Connection, old: &str, new: &str) -> Result<(), DbError> {
    conn.prepare_cached("UPDATE servers SET name = ?1 WHERE name = ?2")?
        .execute(params![new, old])?;
    Ok(())
}

fn query_upsert_server_with_tools(
    conn: &Connection,
    server: &Server,
    tools: &[ToolMeta],
) -> Result<(), DbError> {
    let tx = conn.unchecked_transaction()?;
    query_upsert_server(&tx, server)?;

    // Sync server_tools to exactly the probed set.
    let tool_names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    let placeholders = tool_names
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");
    if tool_names.is_empty() {
        tx.prepare_cached("DELETE FROM server_tools WHERE server = ?1")?
            .execute(params![&server.name])?;
        tx.prepare_cached("DELETE FROM policies WHERE server = ?1 AND scope = 'tool'")?
            .execute(params![&server.name])?;
    } else {
        let del_tools_sql =
            format!("DELETE FROM server_tools WHERE server = ? AND tool NOT IN ({placeholders})");
        let del_pols_sql = format!(
            "DELETE FROM policies WHERE server = ? AND scope = 'tool' AND tool NOT IN ({placeholders})"
        );
        let mut params_vec: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(tool_names.len() + 1);
        params_vec.push(&server.name);
        for t in &tool_names {
            params_vec.push(t);
        }
        tx.prepare(&del_tools_sql)?.execute(&params_vec[..])?;
        tx.prepare(&del_pols_sql)?.execute(&params_vec[..])?;
    }

    for tool in tools {
        tx.prepare_cached(
            "INSERT INTO server_tools (server, tool, description, input_schema, discovered_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(server, tool) DO UPDATE SET
                 description = excluded.description,
                 input_schema = excluded.input_schema,
                 discovered_at = excluded.discovered_at",
        )?
        .execute(params![
            &server.name,
            &tool.name,
            &tool.description,
            &tool.input_schema_json,
            tool.discovered_at,
        ])?;

        // Default per-tool policy: 'ask' / 'probe'. ON CONFLICT preserves any
        // existing user-set decision (Re-test must not clobber user choices).
        tx.prepare_cached(
            "INSERT INTO policies (server, tool, scope, decision, decision_source, created_at)
             VALUES (?1, ?2, 'tool', 'ask', 'probe', ?3)
             ON CONFLICT(server, tool, scope) DO NOTHING",
        )?
        .execute(params![&server.name, &tool.name, tool.discovered_at])?;
    }

    // Server-wide fallback row (scope='server', tool='').
    let now = now_unix();
    tx.prepare_cached(
        "INSERT INTO policies (server, tool, scope, decision, decision_source, created_at)
         VALUES (?1, '', 'server', 'ask', 'probe', ?2)
         ON CONFLICT(server, tool, scope) DO NOTHING",
    )?
    .execute(params![&server.name, now])?;

    tx.commit()?;
    Ok(())
}

fn query_list_server_tools(conn: &Connection, server: &str) -> Result<Vec<ToolMeta>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT tool, description, input_schema, discovered_at
         FROM server_tools WHERE server = ?1 ORDER BY tool",
    )?;
    let rows = stmt.query_map(params![server], |row| {
        Ok(ToolMeta {
            name: row.get(0)?,
            description: row.get(1)?,
            input_schema_json: row.get(2)?,
            discovered_at: row.get(3)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn query_set_settings_kv(conn: &Connection, key: &str, value: &str) -> Result<(), DbError> {
    conn.prepare_cached(
        "INSERT INTO settings_kv (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )?
    .execute(params![key, value])?;
    Ok(())
}

fn query_get_settings_kv(conn: &Connection, key: &str) -> Result<Option<String>, DbError> {
    let row = conn
        .prepare_cached("SELECT value FROM settings_kv WHERE key = ?1")?
        .query_row(params![key], |row| row.get::<_, String>(0))
        .optional()?;
    Ok(row)
}

fn query_list_columns(conn: &Connection, table: &str) -> Result<Vec<String>, DbError> {
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

fn query_lookup_ide_key(conn: &Connection, key: &str) -> Result<Option<IdeKey>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT ide_key, display_name, last_seen_at FROM ides
         WHERE ide_key = ?1 AND status != 'not_tracked'",
    )?;
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

fn decode_ide_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<IdeRecord> {
    let status_str: String = row.get(3)?;
    let status = IdeStatus::from_db_str(&status_str).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            format!("unknown ide status: {status_str}").into(),
        )
    })?;
    let entries_json: String = row.get(6)?;
    let managed_entries: Vec<String> = serde_json::from_str(&entries_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            6,
            rusqlite::types::Type::Text,
            format!("decode managed_entries_json: {e}").into(),
        )
    })?;
    let config_path: Option<String> = row.get(5)?;
    Ok(IdeRecord {
        ide_kind: row.get(0)?,
        display_name: row.get(1)?,
        ide_key: row.get(2)?,
        status,
        detection_source: row.get(4)?,
        config_path: config_path.map(PathBuf::from),
        managed_entries,
        first_seen_at: row.get(7)?,
        last_seen_at: row.get(8)?,
        not_tracked_at: row.get(9)?,
    })
}

const IDE_SELECT_COLUMNS: &str =
    "ide_kind, display_name, ide_key, status, detection_source, config_path,
     managed_entries_json, first_seen_at, last_seen_at, not_tracked_at";

fn query_list_ides(conn: &Connection) -> Result<Vec<IdeRecord>, DbError> {
    let sql = format!("SELECT {IDE_SELECT_COLUMNS} FROM ides ORDER BY display_name");
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map([], decode_ide_record)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn query_list_active_ides(conn: &Connection) -> Result<Vec<IdeRecord>, DbError> {
    let sql = format!(
        "SELECT {IDE_SELECT_COLUMNS} FROM ides WHERE status = 'active' ORDER BY display_name"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map([], decode_ide_record)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn query_get_ide_by_kind(conn: &Connection, ide_kind: &str) -> Result<Option<IdeRecord>, DbError> {
    let sql = format!("SELECT {IDE_SELECT_COLUMNS} FROM ides WHERE ide_kind = ?1");
    let row = conn
        .prepare_cached(&sql)?
        .query_row(params![ide_kind], decode_ide_record)
        .optional()?;
    Ok(row)
}

fn query_upsert_detected_ide(
    conn: &Connection,
    ide_kind: &str,
    display_name: &str,
    detection_source: Option<&str>,
) -> Result<IdeRecord, DbError> {
    let now = now_unix();
    let tx = conn.unchecked_transaction()?;
    let existing = {
        let sql = format!("SELECT {IDE_SELECT_COLUMNS} FROM ides WHERE ide_kind = ?1");
        tx.prepare_cached(&sql)?
            .query_row(params![ide_kind], decode_ide_record)
            .optional()?
    };

    if let Some(mut row) = existing {
        row.last_seen_at = now;
        row.display_name = display_name.to_string();
        row.detection_source = detection_source.map(str::to_string);
        if matches!(row.status, IdeStatus::Stale) {
            row.status = IdeStatus::Active;
        }
        tx.prepare_cached(
            "UPDATE ides SET display_name = ?1, detection_source = ?2,
                             last_seen_at = ?3,
                             status = CASE WHEN status = 'stale' THEN 'active' ELSE status END
             WHERE ide_kind = ?4",
        )?
        .execute(params![display_name, detection_source, now, ide_kind])?;
        tx.commit()?;
        return Ok(row);
    }

    let key = fresh_ide_key();
    tx.prepare_cached(
        "INSERT INTO ides
            (ide_kind, display_name, ide_key, status, detection_source,
             config_path, managed_entries_json, first_seen_at, last_seen_at, not_tracked_at)
         VALUES (?1, ?2, ?3, 'active', ?4, NULL, '[]', ?5, ?5, NULL)",
    )?
    .execute(params![ide_kind, display_name, key, detection_source, now])?;
    tx.commit()?;
    Ok(IdeRecord {
        ide_kind: ide_kind.to_string(),
        display_name: display_name.to_string(),
        ide_key: key,
        status: IdeStatus::Active,
        detection_source: detection_source.map(str::to_string),
        config_path: None,
        managed_entries: Vec::new(),
        first_seen_at: now,
        last_seen_at: now,
        not_tracked_at: None,
    })
}

fn query_mark_ides_stale(conn: &Connection, kinds_present: &[String]) -> Result<(), DbError> {
    let placeholders = if kinds_present.is_empty() {
        "''".to_string()
    } else {
        kinds_present
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(", ")
    };
    let sql = format!(
        "UPDATE ides SET status = 'stale'
         WHERE status = 'active' AND ide_kind NOT IN ({placeholders})"
    );
    let mut params_vec: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(kinds_present.len());
    for kind in kinds_present {
        params_vec.push(kind);
    }
    conn.prepare(&sql)?.execute(&params_vec[..])?;
    Ok(())
}

fn query_mark_ide_not_tracked(conn: &Connection, ide_kind: &str) -> Result<(), DbError> {
    let now = now_unix();
    conn.prepare_cached(
        "UPDATE ides SET status = 'not_tracked', config_path = NULL,
                         managed_entries_json = '[]', not_tracked_at = ?1
         WHERE ide_kind = ?2",
    )?
    .execute(params![now, ide_kind])?;
    Ok(())
}

fn query_retrack_ide(conn: &Connection, ide_kind: &str) -> Result<(), DbError> {
    let now = now_unix();
    conn.prepare_cached(
        "UPDATE ides SET status = 'active', not_tracked_at = NULL, last_seen_at = ?1
         WHERE ide_kind = ?2 AND status = 'not_tracked'",
    )?
    .execute(params![now, ide_kind])?;
    Ok(())
}

fn query_update_ide_managed_entries(
    conn: &Connection,
    ide_kind: &str,
    config_path: &Path,
    managed_entries: &[String],
) -> Result<(), DbError> {
    let json =
        serde_json::to_string(managed_entries).map_err(|e| DbError::Encoding(e.to_string()))?;
    let path_str = config_path
        .to_str()
        .ok_or_else(|| DbError::Encoding("non-utf8 config_path".to_string()))?;
    let now = now_unix();
    conn.prepare_cached(
        "UPDATE ides SET config_path = ?1, managed_entries_json = ?2, last_seen_at = ?3
         WHERE ide_kind = ?4",
    )?
    .execute(params![path_str, json, now, ide_kind])?;
    Ok(())
}

fn decode_policy_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Policy> {
    let decision_str: String = row.get(3)?;
    let source_str: String = row.get(4)?;
    let decision = Decision::from_db_str(&decision_str).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            format!("unknown decision: {decision_str}").into(),
        )
    })?;
    let decision_source = DecisionSource::from_db_str(&source_str).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            format!("unknown decision_source: {source_str}").into(),
        )
    })?;
    Ok(Policy {
        id: row.get(0)?,
        server: row.get(1)?,
        tool: row.get(2)?,
        decision,
        decision_source,
        created_at: row.get(5)?,
    })
}

fn query_set_policy(
    conn: &Connection,
    server: &str,
    tool: &str,
    decision: Decision,
    decision_source: DecisionSource,
) -> Result<i64, DbError> {
    let now = now_unix();
    let id: i64 = conn
        .prepare_cached(
            "INSERT INTO policies (server, tool, scope, decision, decision_source, created_at)
             VALUES (?1, ?2, 'tool', ?3, ?4, ?5)
             ON CONFLICT(server, tool, scope) DO UPDATE SET
                 decision = excluded.decision,
                 decision_source = excluded.decision_source,
                 created_at = excluded.created_at
             RETURNING id",
        )?
        .query_row(
            params![
                server,
                tool,
                decision.as_db_str(),
                decision_source.as_db_str(),
                now
            ],
            |row| row.get(0),
        )?;
    Ok(id)
}

fn query_get_policy(
    conn: &Connection,
    server: &str,
    tool: &str,
) -> Result<Option<Policy>, DbError> {
    let row = conn
        .prepare_cached(
            "SELECT id, server, tool, decision, decision_source, created_at
             FROM policies WHERE server = ?1 AND tool = ?2 AND scope = 'tool'",
        )?
        .query_row(params![server, tool], decode_policy_row)
        .optional()?;
    Ok(row)
}

fn query_get_policy_scoped(
    conn: &Connection,
    server: &str,
    tool: &str,
    scope: &str,
) -> Result<Option<Policy>, DbError> {
    let row = conn
        .prepare_cached(
            "SELECT id, server, tool, decision, decision_source, created_at
             FROM policies WHERE server = ?1 AND tool = ?2 AND scope = ?3",
        )?
        .query_row(params![server, tool, scope], decode_policy_row)
        .optional()?;
    Ok(row)
}

fn query_list_policies_for_server(conn: &Connection, server: &str) -> Result<Vec<Policy>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, server, tool, decision, decision_source, created_at
         FROM policies WHERE server = ?1 ORDER BY tool",
    )?;
    let rows = stmt.query_map(params![server], decode_policy_row)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn query_list_all_policies(conn: &Connection) -> Result<Vec<Policy>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, server, tool, decision, decision_source, created_at
         FROM policies ORDER BY server, tool",
    )?;
    let rows = stmt.query_map([], decode_policy_row)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn query_delete_policies_for_server(conn: &Connection, server: &str) -> Result<(), DbError> {
    conn.prepare_cached("DELETE FROM policies WHERE server = ?1")?
        .execute(params![server])?;
    Ok(())
}

fn decode_pending_approval_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PendingApproval> {
    let status_str: String = row.get(6)?;
    let status = ApprovalStatus::from_db_str(&status_str).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            6,
            rusqlite::types::Type::Text,
            format!("unknown approval status: {status_str}").into(),
        )
    })?;
    Ok(PendingApproval {
        id: row.get(0)?,
        server: row.get(1)?,
        tool: row.get(2)?,
        args_json: row.get(3)?,
        ide_key: row.get(4)?,
        created_at: row.get(5)?,
        status,
    })
}

fn query_create_pending_approval(
    conn: &Connection,
    approval: &PendingApproval,
) -> Result<(), DbError> {
    conn.prepare_cached(
        "INSERT INTO pending_approvals (id, server, tool, args_json, ide_key, created_at, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?
    .execute(params![
        &approval.id,
        &approval.server,
        &approval.tool,
        &approval.args_json,
        &approval.ide_key,
        approval.created_at,
        approval.status.as_db_str(),
    ])?;
    Ok(())
}

fn query_get_pending_approval(
    conn: &Connection,
    id: &str,
) -> Result<Option<PendingApproval>, DbError> {
    let row = conn
        .prepare_cached(
            "SELECT id, server, tool, args_json, ide_key, created_at, status
             FROM pending_approvals WHERE id = ?1",
        )?
        .query_row(params![id], decode_pending_approval_row)
        .optional()?;
    Ok(row)
}

fn query_resolve_pending_approval(
    conn: &Connection,
    id: &str,
    new_status: ApprovalStatus,
) -> Result<(), DbError> {
    conn.prepare_cached("UPDATE pending_approvals SET status = ?1 WHERE id = ?2")?
        .execute(params![new_status.as_db_str(), id])?;
    Ok(())
}

fn query_list_pending_approvals(conn: &Connection) -> Result<Vec<PendingApproval>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, server, tool, args_json, ide_key, created_at, status
         FROM pending_approvals WHERE status = 'pending' ORDER BY created_at DESC",
    )?;
    let rows = stmt.query_map([], decode_pending_approval_row)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn query_sweep_stale_pending_approvals(
    conn: &Connection,
    older_than_ts: i64,
) -> Result<Vec<String>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT id FROM pending_approvals
         WHERE status = 'pending' AND created_at < ?1",
    )?;
    let ids: Vec<String> = stmt
        .query_map(params![older_than_ts], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    if ids.is_empty() {
        return Ok(ids);
    }
    // SQLite caps `?N` parameters at 999; pending-approval volume is well under
    // that bound, but loop one-id-at-a-time to keep the cached prepare reusable.
    let mut update =
        conn.prepare_cached("UPDATE pending_approvals SET status = 'timeout' WHERE id = ?1")?;
    for id in &ids {
        update.execute(params![id])?;
    }
    Ok(ids)
}

fn decode_audit_entry_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AuditEntry> {
    let decision_str: String = row.get(6)?;
    let decision = AuditDecision::from_db_str(&decision_str).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            6,
            rusqlite::types::Type::Text,
            format!("unknown audit decision: {decision_str}").into(),
        )
    })?;
    let decision_source = row
        .get::<_, Option<String>>(7)?
        .map(|s| {
            DecisionSource::from_db_str(&s).ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    7,
                    rusqlite::types::Type::Text,
                    format!("unknown decision_source: {s}").into(),
                )
            })
        })
        .transpose()?;
    Ok(AuditEntry {
        id: Some(row.get(0)?),
        ts: row.get(1)?,
        ide_key: row.get(2)?,
        ide_name: row.get(3)?,
        server: row.get(4)?,
        tool: row.get(5)?,
        decision,
        decision_source,
        latency_ms: row.get(8)?,
        request_args: row.get(9)?,
        response_status: row.get(10)?,
        response_length: row.get(11)?,
        error_code: row.get(12)?,
    })
}

fn query_insert_audit_entry(conn: &Connection, entry: &AuditEntry) -> Result<i64, DbError> {
    let id: i64 = conn
        .prepare_cached(
            "INSERT INTO audit_log
             (ts, ide_key, ide_name, server, tool, decision, decision_source,
              latency_ms, request_args, response_status, response_length, error_code)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             RETURNING id",
        )?
        .query_row(
            params![
                entry.ts,
                &entry.ide_key,
                &entry.ide_name,
                &entry.server,
                &entry.tool,
                entry.decision.as_db_str(),
                entry.decision_source.map(DecisionSource::as_db_str),
                entry.latency_ms,
                &entry.request_args,
                &entry.response_status,
                entry.response_length,
                &entry.error_code,
            ],
            |row| row.get(0),
        )?;
    Ok(id)
}

fn query_query_audit_log(
    conn: &Connection,
    filter: &AuditFilter,
) -> Result<Vec<AuditEntry>, DbError> {
    let mut sql = String::from(
        "SELECT id, ts, ide_key, ide_name, server, tool, decision, decision_source,
                latency_ms, request_args, response_status, response_length, error_code
         FROM audit_log WHERE 1=1",
    );
    let mut args: Vec<SqlValue> = Vec::new();

    if let Some(k) = &filter.ide_key {
        sql.push_str(" AND ide_key = ?");
        args.push(SqlValue::Text(k.clone()));
    }
    if let Some(s) = &filter.server {
        sql.push_str(" AND server = ?");
        args.push(SqlValue::Text(s.clone()));
    }
    if let Some(t) = &filter.tool {
        sql.push_str(" AND tool = ?");
        args.push(SqlValue::Text(t.clone()));
    }
    if let Some(ts) = filter.since_ts {
        sql.push_str(" AND ts >= ?");
        args.push(SqlValue::Integer(ts));
    }
    if let Some(ts) = filter.until_ts {
        sql.push_str(" AND ts < ?");
        args.push(SqlValue::Integer(ts));
    }
    sql.push_str(" ORDER BY ts DESC");
    if let Some(limit) = filter.limit {
        sql.push_str(" LIMIT ?");
        args.push(SqlValue::Integer(i64::from(limit)));
    }
    if let Some(offset) = filter.offset {
        sql.push_str(" OFFSET ?");
        args.push(SqlValue::Integer(i64::from(offset)));
    }

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(args.iter()), decode_audit_entry_row)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn query_purge_old_audit_entries(conn: &Connection, older_than_ts: i64) -> Result<u64, DbError> {
    let n = conn
        .prepare_cached("DELETE FROM audit_log WHERE ts < ?1")?
        .execute(params![older_than_ts])?;
    Ok(n as u64)
}

fn decode_server_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Server> {
    let args_json: String = row.get(3)?;
    let env_json: String = row.get(4)?;
    let args: Vec<String> = serde_json::from_str(&args_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            format!("args_json: {e}").into(),
        )
    })?;
    let env: Vec<EnvEntry> = serde_json::from_str(&env_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            format!("env_json: {e}").into(),
        )
    })?;
    Ok(Server {
        name: row.get(0)?,
        transport: row.get(1)?,
        command: row.get(2)?,
        args,
        env,
        url: row.get(5)?,
        lifecycle_seconds: row.get(6)?,
        last_init_duration_ms: row.get(7)?,
        package_version: row.get(8)?,
        mcp_self_version: row.get(9)?,
        created_at: row.get(10)?,
        server_name: row.get(11).ok(),
        server_version: row.get(12).ok(),
        last_probed_at: row.get(13).ok(),
        last_probe_error: row.get(14).ok(),
        last_auth_error_msg: row.get(15).ok().flatten(),
        last_auth_error_at: row.get(16).ok().flatten(),
        oauth_authority: row.get(17).ok().flatten(),
        oauth_client_id: row.get(18).ok().flatten(),
        oauth_scopes: row.get(19).ok().flatten(),
    })
}

fn query_upsert_server(conn: &Connection, server: &Server) -> Result<(), DbError> {
    let args_json =
        serde_json::to_string(&server.args).map_err(|e| DbError::Encoding(e.to_string()))?;
    let env_json =
        serde_json::to_string(&server.env).map_err(|e| DbError::Encoding(e.to_string()))?;
    conn.prepare_cached(
        "INSERT INTO servers
         (name, transport, command, args_json, env_json, url, lifecycle_seconds,
          last_init_duration_ms, package_version, mcp_self_version, created_at,
          server_name, server_version, last_probed_at, last_probe_error,
          oauth_authority, oauth_client_id, oauth_scopes)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
         ON CONFLICT(name) DO UPDATE SET
             transport = excluded.transport,
             command = excluded.command,
             args_json = excluded.args_json,
             env_json = excluded.env_json,
             url = excluded.url,
             lifecycle_seconds = excluded.lifecycle_seconds,
             last_init_duration_ms = excluded.last_init_duration_ms,
             package_version = excluded.package_version,
             mcp_self_version = excluded.mcp_self_version,
             server_name = excluded.server_name,
             server_version = excluded.server_version,
             last_probed_at = excluded.last_probed_at,
             last_probe_error = excluded.last_probe_error,
             oauth_authority = excluded.oauth_authority,
             oauth_client_id = excluded.oauth_client_id,
             oauth_scopes = excluded.oauth_scopes",
    )?
    .execute(params![
        &server.name,
        &server.transport,
        &server.command,
        args_json,
        env_json,
        &server.url,
        server.lifecycle_seconds,
        server.last_init_duration_ms,
        &server.package_version,
        &server.mcp_self_version,
        server.created_at,
        &server.server_name,
        &server.server_version,
        server.last_probed_at,
        &server.last_probe_error,
        &server.oauth_authority,
        &server.oauth_client_id,
        &server.oauth_scopes,
    ])?;
    Ok(())
}

const SERVERS_SELECT: &str = "SELECT name, transport, command, args_json, env_json, url,
       lifecycle_seconds, last_init_duration_ms, package_version,
       mcp_self_version, created_at,
       server_name, server_version, last_probed_at, last_probe_error,
       last_auth_error_msg, last_auth_error_at,
       oauth_authority, oauth_client_id, oauth_scopes
 FROM servers";

fn query_get_server(conn: &Connection, name: &str) -> Result<Option<Server>, DbError> {
    let sql = format!("{SERVERS_SELECT} WHERE name = ?1");
    let row = conn
        .prepare_cached(&sql)?
        .query_row(params![name], decode_server_row)
        .optional()?;
    Ok(row)
}

fn query_list_servers(conn: &Connection) -> Result<Vec<Server>, DbError> {
    let sql = format!("{SERVERS_SELECT} ORDER BY name");
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map([], decode_server_row)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn query_delete_server(conn: &Connection, name: &str) -> Result<(), DbError> {
    conn.prepare_cached("DELETE FROM servers WHERE name = ?1")?
        .execute(params![name])?;
    Ok(())
}

fn query_set_server_auth_error(
    conn: &Connection,
    server: &str,
    message: &str,
) -> Result<bool, DbError> {
    let prev: Option<String> = conn
        .prepare_cached("SELECT last_auth_error_msg FROM servers WHERE name = ?1")?
        .query_row(params![server], |row| row.get::<_, Option<String>>(0))
        .optional()?
        .flatten();
    let transition = prev.as_deref() != Some(message);
    let now = now_unix();
    conn.prepare_cached(
        "UPDATE servers SET last_auth_error_msg = ?1, last_auth_error_at = ?2 WHERE name = ?3",
    )?
    .execute(params![message, now, server])?;
    Ok(transition)
}

fn query_clear_server_auth_error(conn: &Connection, server: &str) -> Result<bool, DbError> {
    let prev: Option<String> = conn
        .prepare_cached("SELECT last_auth_error_msg FROM servers WHERE name = ?1")?
        .query_row(params![server], |row| row.get::<_, Option<String>>(0))
        .optional()?
        .flatten();
    let cleared = prev.is_some();
    if cleared {
        conn.prepare_cached(
            "UPDATE servers SET last_auth_error_msg = NULL, last_auth_error_at = NULL WHERE name = ?1",
        )?
        .execute(params![server])?;
    }
    Ok(cleared)
}

fn insert_oauth_draft(conn: &Connection, draft_uuid: &str) -> Result<(), DbError> {
    conn.prepare_cached(
        "INSERT OR IGNORE INTO pending_oauth_drafts (draft_uuid) VALUES (?1)",
    )?
    .execute(params![draft_uuid])?;
    Ok(())
}

fn delete_oauth_draft(conn: &Connection, draft_uuid: &str) -> Result<(), DbError> {
    conn.prepare_cached("DELETE FROM pending_oauth_drafts WHERE draft_uuid = ?1")?
        .execute(params![draft_uuid])?;
    Ok(())
}

fn list_oauth_drafts(conn: &Connection) -> Result<Vec<String>, DbError> {
    let mut stmt = conn.prepare_cached("SELECT draft_uuid FROM pending_oauth_drafts")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut drafts = Vec::new();
    for row in rows {
        drafts.push(row?);
    }
    Ok(drafts)
}
