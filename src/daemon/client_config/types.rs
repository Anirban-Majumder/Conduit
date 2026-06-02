//! Client config types: the compile-time `ConfigSpec`, its runtime DB form
//! `ConfigSchema`, and `ResolvedClient` — the single client view consumers read.
//!
//! `ConfigSpec` lives in each `ClientDef` (`&'static`); `ConfigSchema` is the
//! owned, serde-ready form stored in `client.schema_json` (`From<&ConfigSpec>`).
//! `ResolvedClient` is built from a `client` DB row and references no `ClientDef`
//! or IDE name — built-in and custom clients are identical here.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::daemon::db::types::ClientRow;
use crate::daemon::db::Db;
use crate::daemon::errors::DbError;

/// MCP config file format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConfigFormat {
    #[default]
    Json,
    Jsonc,
    Toml,
}

/// Static config writing spec — lives in `ClientDef`, compile-time.
#[derive(Debug, Clone, Copy)]
pub struct ConfigSpec {
    pub format: ConfigFormat,
    pub key_path: &'static [&'static str],
    /// JSON literal with `${url}` (and optionally `${name}`, `${key}`, `${port}`)
    /// placeholders, parsed at render time.
    pub entry_template: &'static str,
}

/// Runtime config schema — serialized into `client.schema_json` in the DB.
///
/// All fields have `#[serde(default)]` so a partial/empty JSON object
/// deserializes without error (forward-compat for schema additions).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ConfigSchema {
    #[serde(default)]
    pub format: ConfigFormat,
    #[serde(default)]
    pub key_path: Vec<String>,
    #[serde(default)]
    pub entry_template: serde_json::Value,
}

impl From<&ConfigSpec> for ConfigSchema {
    fn from(spec: &ConfigSpec) -> Self {
        Self {
            format: spec.format,
            key_path: spec.key_path.iter().map(|s| (*s).to_owned()).collect(),
            // entry_template is a JSON literal string — parse it into a Value.
            // If it's not valid JSON (shouldn't happen for built-ins), store as a string Value.
            entry_template: serde_json::from_str(spec.entry_template)
                .unwrap_or_else(|_| serde_json::Value::String(spec.entry_template.to_owned())),
        }
    }
}

/// Resolved, ready-to-use view of a client row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedClient {
    pub id: String,
    pub display_name: String,
    pub is_builtin: bool,
    pub mcp_key: String,
    /// Absolute, already-resolved config path.
    pub config_path: PathBuf,
    /// `None` = no skills support.
    pub skills_dir: Option<PathBuf>,
    /// Deserialized from `client.schema_json`.
    pub schema: ConfigSchema,
    /// Repo-root-relative MCP config path (Phase 5b). `None` = no repo MCP support.
    pub project_config_rel: Option<PathBuf>,
    /// Repo-root-relative skills dir (Phase 5b). `None` = no repo skills support.
    pub project_skills_rel: Option<PathBuf>,
}

/// Where a managed row applies: global config, or one repo directory.
///
/// Serialized to/from the `managed_mcp.scope` / `managed_skill.scope` TEXT column
/// via [`Scope::from_db`] / [`Scope::as_db`]. `Repo` holds the normalized absolute
/// path produced by [`normalize_scope_path`] — no fs I/O happens here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    Global,
    Repo(PathBuf),
}

impl Scope {
    pub fn from_db(s: &str) -> Self {
        if s == "global" {
            Self::Global
        } else {
            Self::Repo(PathBuf::from(s))
        }
    }

    pub fn as_db(&self) -> String {
        match self {
            Self::Global => "global".to_owned(),
            Self::Repo(p) => p.to_string_lossy().into_owned(),
        }
    }
}

impl ResolvedClient {
    /// Build from a raw DB row, parsing `schema_json`. A malformed schema falls
    /// back to `ConfigSchema::default()` (every field is `#[serde(default)]`),
    /// so a single corrupt row never blocks the whole Editors list.
    pub fn from_row(row: ClientRow) -> Self {
        let schema = serde_json::from_str(&row.schema_json).unwrap_or_default();
        Self {
            id: row.id,
            display_name: row.name,
            is_builtin: row.is_builtin,
            mcp_key: row.mcp_key,
            config_path: PathBuf::from(row.mcp_config_path),
            skills_dir: row.skills_dir.map(PathBuf::from),
            schema,
            project_config_rel: row.project_config_rel.map(PathBuf::from),
            project_skills_rel: row.project_skills_rel.map(PathBuf::from),
        }
    }

    /// Absolute config file to write for a scope. `None` = this client has no
    /// repo-local config support (so a repo scope can't target it).
    pub fn config_target(&self, scope: &Scope) -> Option<PathBuf> {
        match scope {
            Scope::Global => Some(self.config_path.clone()),
            Scope::Repo(dir) => self.project_config_rel.as_ref().map(|rel| dir.join(rel)),
        }
    }

    /// Absolute skills dir to mirror into for a scope. `None` = this client has
    /// no skills support at that scope (e.g. VS Code Copilot has no repo skills).
    pub fn skills_target(&self, scope: &Scope) -> Option<PathBuf> {
        match scope {
            Scope::Global => self.skills_dir.clone(),
            Scope::Repo(dir) => self.project_skills_rel.as_ref().map(|rel| dir.join(rel)),
        }
    }
}

/// Normalize a user-typed repo path into the canonical absolute `scope` string (L10).
///
/// Expands a leading `~`, canonicalizes (resolving symlinks, stripping any
/// trailing separator), and rejects anything that isn't an existing directory.
/// Run this ONLY when the user adds a new repo path — the stored canonical string
/// is thereafter the verbatim key for toggle/remove, so a since-deleted repo can
/// still be un-scoped without re-canonicalizing. Does blocking `std::fs` I/O;
/// call it on the worker via `spawn_blocking`.
pub fn normalize_scope_path(
    input: &str,
) -> Result<String, crate::daemon::client_config::ClientError> {
    use crate::daemon::client_config::ClientError;

    let expanded = expand_tilde(input);
    let canonical = std::fs::canonicalize(&expanded)?;
    if !canonical.is_dir() {
        return Err(ClientError::NotADirectory(canonical));
    }
    canonical
        .to_str()
        .map(str::to_owned)
        .ok_or(ClientError::NonUtf8Path)
}

/// Expand a leading `~` (alone or `~/…`) to the home directory. Any other input
/// is returned verbatim — `canonicalize` later rejects what doesn't exist.
fn expand_tilde(input: &str) -> PathBuf {
    let Some(rest) = input.strip_prefix('~') else {
        return PathBuf::from(input);
    };
    let is_home_ref =
        rest.is_empty() || rest.starts_with('/') || rest.starts_with(std::path::MAIN_SEPARATOR);
    match (is_home_ref, dirs::home_dir()) {
        (true, Some(home)) => {
            let rel = rest.trim_start_matches(['/', std::path::MAIN_SEPARATOR]);
            if rel.is_empty() {
                home
            } else {
                home.join(rel)
            }
        }
        _ => PathBuf::from(input),
    }
}

/// Resolve a single client by id from the DB. `None` if no such row.
pub async fn resolve_client(db: &Db, id: &str) -> Result<Option<ResolvedClient>, DbError> {
    Ok(db.get_client(id).await?.map(ResolvedClient::from_row))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, reason = "test ergonomics")]

    use super::*;
    use crate::daemon::time_util::now_unix_nanos;

    fn client(config_rel: Option<&str>, skills_rel: Option<&str>) -> ResolvedClient {
        ResolvedClient {
            id: "ClaudeCode".into(),
            display_name: "Claude Code".into(),
            is_builtin: true,
            mcp_key: "abc12345".into(),
            config_path: PathBuf::from("/home/u/.claude.json"),
            skills_dir: Some(PathBuf::from("/home/u/.claude/skills")),
            schema: ConfigSchema::default(),
            project_config_rel: config_rel.map(PathBuf::from),
            project_skills_rel: skills_rel.map(PathBuf::from),
        }
    }

    #[test]
    fn scope_round_trips() {
        assert_eq!(Scope::from_db("global"), Scope::Global);
        assert_eq!(Scope::Global.as_db(), "global");
        let repo = Scope::Repo(PathBuf::from("/home/u/work/foo"));
        assert_eq!(Scope::from_db("/home/u/work/foo"), repo);
        assert_eq!(repo.as_db(), "/home/u/work/foo");
        assert_eq!(Scope::from_db(&repo.as_db()), repo);
    }

    #[test]
    fn config_target_resolves_global_and_repo() {
        let c = client(Some(".mcp.json"), Some(".claude/skills"));
        assert_eq!(
            c.config_target(&Scope::Global),
            Some(PathBuf::from("/home/u/.claude.json"))
        );
        assert_eq!(
            c.config_target(&Scope::Repo(PathBuf::from("/w/foo"))),
            Some(PathBuf::from("/w/foo/.mcp.json"))
        );
    }

    #[test]
    fn skills_target_resolves_global_and_repo() {
        let c = client(Some(".mcp.json"), Some(".claude/skills"));
        assert_eq!(
            c.skills_target(&Scope::Global),
            Some(PathBuf::from("/home/u/.claude/skills"))
        );
        assert_eq!(
            c.skills_target(&Scope::Repo(PathBuf::from("/w/foo"))),
            Some(PathBuf::from("/w/foo/.claude/skills"))
        );
    }

    #[test]
    fn targets_none_when_client_lacks_repo_support() {
        // VS Code Copilot shape: repo MCP config but no repo skills.
        let c = client(Some(".vscode/mcp.json"), None);
        assert_eq!(c.skills_target(&Scope::Repo(PathBuf::from("/w/foo"))), None);
        assert!(c
            .config_target(&Scope::Repo(PathBuf::from("/w/foo")))
            .is_some());
    }

    #[test]
    fn normalize_scope_path_canonicalizes_and_strips_trailing_sep() {
        let dir = std::env::temp_dir().join(format!("conduit-scope-ok-{}", now_unix_nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        let with_sep = format!("{}{}", dir.to_str().unwrap(), std::path::MAIN_SEPARATOR);
        let normalized = normalize_scope_path(&with_sep).unwrap();
        assert!(std::path::Path::new(&normalized).is_absolute());
        assert!(!normalized.ends_with(std::path::MAIN_SEPARATOR));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn normalize_scope_path_expands_tilde() {
        let home = dirs::home_dir().expect("home dir in test env");
        let normalized = normalize_scope_path("~").unwrap();
        assert_eq!(
            std::path::Path::new(&normalized),
            home.canonicalize().unwrap()
        );
    }

    #[test]
    fn normalize_scope_path_errors_on_missing() {
        let missing =
            std::env::temp_dir().join(format!("conduit-scope-missing-{}", now_unix_nanos()));
        assert!(normalize_scope_path(missing.to_str().unwrap()).is_err());
    }

    #[test]
    fn normalize_scope_path_errors_on_file() {
        let file = std::env::temp_dir().join(format!("conduit-scope-file-{}", now_unix_nanos()));
        std::fs::write(&file, "x").unwrap();
        assert!(normalize_scope_path(file.to_str().unwrap()).is_err());
        std::fs::remove_file(&file).ok();
    }
}
