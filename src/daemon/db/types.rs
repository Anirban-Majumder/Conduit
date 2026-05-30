//! Shared row/value types for the DB layer.
//!
//! Plain data structs mirroring the `state.db` tables (`Server`, `Policy`,
//! `PendingApproval`, `AuditEntry`, `IdeRecord`, `Skill`, ...) plus the small
//! enums (`Decision`, `ApprovalStatus`, `IdeStatus`, ...) with `as_db_str` /
//! `from_db_str` mapping to their textual DB representations. No SQL here.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
    Ask,
}

impl Decision {
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
            Self::Ask => "ask",
        }
    }

    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "allow" => Some(Self::Allow),
            "deny" => Some(Self::Deny),
            "ask" => Some(Self::Ask),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionSource {
    Default,
    User,
    ToolListChanged,
    Probe,
    Notification,
}

impl DecisionSource {
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::User => "user",
            Self::ToolListChanged => "tool_list_changed",
            Self::Probe => "probe",
            Self::Notification => "notification",
        }
    }

    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "default" => Some(Self::Default),
            "user" => Some(Self::User),
            "tool_list_changed" => Some(Self::ToolListChanged),
            "probe" => Some(Self::Probe),
            "notification" => Some(Self::Notification),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    pub id: i64,
    pub server: String,
    pub tool: String,
    pub decision: Decision,
    pub decision_source: DecisionSource,
    pub created_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Denied,
    Timeout,
    Cancelled,
}

impl ApprovalStatus {
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Denied => "denied",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "approved" => Some(Self::Approved),
            "denied" => Some(Self::Denied),
            "timeout" => Some(Self::Timeout),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingApproval {
    pub id: String,
    pub server: String,
    pub tool: String,
    pub args_json: String,
    pub ide_key: String,
    pub created_at: i64,
    pub status: ApprovalStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditDecision {
    Allow,
    Deny,
    AskApproved,
    AskDenied,
    AskTimeout,
    Error,
    Cancelled,
}

impl AuditDecision {
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
            Self::AskApproved => "ask-approved",
            Self::AskDenied => "ask-denied",
            Self::AskTimeout => "ask-timeout",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "allow" => Some(Self::Allow),
            "deny" => Some(Self::Deny),
            "ask-approved" => Some(Self::AskApproved),
            "ask-denied" => Some(Self::AskDenied),
            "ask-timeout" => Some(Self::AskTimeout),
            "error" => Some(Self::Error),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntry {
    pub id: Option<i64>,
    pub ts: i64,
    pub ide_key: Option<String>,
    pub ide_name: Option<String>,
    pub server: String,
    pub tool: Option<String>,
    pub decision: AuditDecision,
    pub decision_source: Option<DecisionSource>,
    pub latency_ms: Option<i64>,
    pub request_args: Option<String>,
    pub response_status: Option<String>,
    pub response_length: Option<i64>,
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AuditFilter {
    pub ide_key: Option<String>,
    pub server: Option<String>,
    pub tool: Option<String>,
    pub since_ts: Option<i64>,
    pub until_ts: Option<i64>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "value_kind", content = "value_or_ref", rename_all = "lowercase")]
pub enum EnvValue {
    Plain(String),
    Secret(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvEntry {
    pub name: String,
    #[serde(flatten)]
    pub value: EnvValue,
}

/// Transport label as stored in `servers.transport`. "stdio" | "http".
pub const TRANSPORT_STDIO: &str = "stdio";
pub const TRANSPORT_HTTP: &str = "http";

#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::struct_field_names,
    reason = "field names match DB column names"
)]
pub struct Server {
    pub name: String,
    pub transport: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: Vec<EnvEntry>,
    pub url: Option<String>,
    pub lifecycle_seconds: i32,
    pub last_init_duration_ms: Option<i64>,
    pub package_version: Option<String>,
    pub mcp_self_version: Option<String>,
    pub created_at: i64,
    pub server_name: Option<String>,
    pub server_version: Option<String>,
    pub last_probed_at: Option<i64>,
    pub last_probe_error: Option<String>,
    pub last_auth_error_msg: Option<String>,
    pub last_auth_error_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdeKey {
    pub key: String,
    pub ide_name: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdeStatus {
    Active,
    Stale,
    NotTracked,
}

impl IdeStatus {
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Stale => "stale",
            Self::NotTracked => "not_tracked",
        }
    }

    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "active" => Some(Self::Active),
            "stale" => Some(Self::Stale),
            "not_tracked" => Some(Self::NotTracked),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdeRecord {
    pub ide_kind: String,
    pub display_name: String,
    pub ide_key: String,
    pub status: IdeStatus,
    pub detection_source: Option<String>,
    pub config_path: Option<PathBuf>,
    pub managed_entries: Vec<String>,
    pub first_seen_at: i64,
    pub last_seen_at: i64,
    pub not_tracked_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolMeta {
    pub name: String,
    pub description: Option<String>,
    pub input_schema_json: String,
    pub discovered_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedIdeConfig {
    pub ide_name: String,
    pub ide_key: String,
    pub config_path: PathBuf,
    pub managed_entries: Vec<String>,
    pub written_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRepo {
    pub owner: String,
    pub name: String,
    pub branch: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::struct_field_names,
    reason = "field names match DB column names"
)]
pub struct Skill {
    pub directory: String,
    pub repo_owner: String,
    pub repo_name: String,
    pub repo_branch: String,
    pub source_directory: String,
    pub skill_name: String,
    pub description: String,
    pub readme_url: Option<String>,
    pub installed_at: i64,
    pub targets: Vec<String>,
}

/// Generates an 8-char alphanumeric key per spec §"Add IDE flow".
///
/// 8 chars × 62 alphabet = ~47 bits — the IDE key is a *capability*, not a
/// cryptographic secret; the threat model (spec §"Threat model") explicitly
/// excludes same-uid attackers, and the daemon binds 127.0.0.1.
pub fn fresh_ide_key() -> String {
    use rand::distr::{Alphanumeric, SampleString};
    Alphanumeric.sample_string(&mut rand::rng(), 8)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, reason = "test ergonomics")]

    use super::*;

    #[test]
    fn fresh_ide_key_is_8_alphanumeric_chars() {
        for _ in 0..50 {
            let k = fresh_ide_key();
            assert_eq!(k.len(), 8);
            assert!(k.chars().all(|c| c.is_ascii_alphanumeric()), "got {k}");
        }
    }

    #[test]
    fn fresh_ide_key_collisions_are_rare() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            assert!(seen.insert(fresh_ide_key()));
        }
    }

    #[test]
    fn decision_round_trips() {
        use Decision::*;
        for v in [Allow, Deny, Ask] {
            assert_eq!(Decision::from_db_str(v.as_db_str()), Some(v), "round-trip failed for {v:?}");
        }
        assert_eq!(Decision::from_db_str("bogus_value_that_will_never_exist"), None);
    }

    #[test]
    fn decision_source_round_trips() {
        use DecisionSource::*;
        for v in [Default, User, ToolListChanged, Probe, Notification] {
            assert_eq!(DecisionSource::from_db_str(v.as_db_str()), Some(v), "round-trip failed for {v:?}");
        }
        assert_eq!(DecisionSource::from_db_str("bogus_value_that_will_never_exist"), None);
    }

    #[test]
    fn approval_status_round_trips() {
        use ApprovalStatus::*;
        for v in [Pending, Approved, Denied, Timeout, Cancelled] {
            assert_eq!(ApprovalStatus::from_db_str(v.as_db_str()), Some(v), "round-trip failed for {v:?}");
        }
        assert_eq!(ApprovalStatus::from_db_str("bogus_value_that_will_never_exist"), None);
    }

    #[test]
    fn audit_decision_round_trips() {
        use AuditDecision::*;
        for v in [Allow, Deny, AskApproved, AskDenied, AskTimeout, Error, Cancelled] {
            assert_eq!(AuditDecision::from_db_str(v.as_db_str()), Some(v), "round-trip failed for {v:?}");
        }
        assert_eq!(AuditDecision::from_db_str("bogus_value_that_will_never_exist"), None);
    }

    #[test]
    fn ide_status_round_trips() {
        use IdeStatus::*;
        for v in [Active, Stale, NotTracked] {
            assert_eq!(IdeStatus::from_db_str(v.as_db_str()), Some(v), "round-trip failed for {v:?}");
        }
        assert_eq!(IdeStatus::from_db_str("bogus_value_that_will_never_exist"), None);
    }
}
