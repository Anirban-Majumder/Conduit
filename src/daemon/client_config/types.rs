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
        }
    }
}

/// Resolve a single client by id from the DB. `None` if no such row.
pub async fn resolve_client(db: &Db, id: &str) -> Result<Option<ResolvedClient>, DbError> {
    Ok(db.get_client(id).await?.map(ResolvedClient::from_row))
}
