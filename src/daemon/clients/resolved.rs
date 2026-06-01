//! `ResolvedClient` — the single client view every consumer reads.
//!
//! Built from a `client` DB row (`ClientRow`); carries no reference to any
//! `ClientDef` or IDE name. Built-in and custom clients are indistinguishable
//! here — both are just resolved identity + config schema. After the startup
//! projection, the renderer / proxy / cascade all take this type, so no
//! downstream code imports an IDE module.

use std::path::PathBuf;

use crate::daemon::db::types::ClientRow;
use crate::daemon::db::Db;
use crate::daemon::errors::DbError;

use super::schema::ConfigSchema;

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
