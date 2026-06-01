//! The detect→DB projection (§1a). Runs ONCE at startup, before any consumer
//! reads the `client` table.
//!
//! Built-in identity (name / config path / schema / skills dir) is code, painted
//! into `client` rows here. It changes only on a Conduit upgrade (a const moved)
//! or the first time a client is seen — so identity columns are re-projected
//! only when `client_def_version` differs from `CARGO_PKG_VERSION`, or when the
//! row is absent. Every detect bumps `last_seen_at`. Custom rows (`is_builtin=0`)
//! are never in `detected` and are never touched.

use super::detect::DetectedClient;
use super::types::ConfigSchema;
use crate::daemon::db::Db;
use crate::daemon::errors::DbError;

const VERSION_KEY: &str = "client_def_version";

/// Project all detected built-ins into the `client` table. Identity columns are
/// rewritten only on a version change or first detect (invariant §1a.1).
pub async fn project_builtins(db: &Db, detected: &[DetectedClient]) -> Result<(), DbError> {
    let coded = env!("CARGO_PKG_VERSION");
    let stored = db.get_settings_kv(VERSION_KEY).await?;
    let version_changed = stored.as_deref() != Some(coded);

    for d in detected {
        let def = d.def;
        let existing = db.get_client(def.id).await?;
        if version_changed || existing.is_none() {
            let config_path = super::paths::resolve(&def.config)
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            let skills_dir = def
                .skills
                .as_ref()
                .and_then(super::paths::resolve)
                .map(|p| p.display().to_string());
            let schema = ConfigSchema::from(&def.config_spec);
            let schema_json =
                serde_json::to_string(&schema).map_err(|e| DbError::Encoding(e.to_string()))?;
            db.project_builtin(
                def.id,
                def.display_name,
                &config_path,
                skills_dir.as_deref(),
                &schema_json,
            )
            .await?;
        } else {
            db.touch_client(def.id).await?;
        }
    }

    if version_changed {
        db.set_settings_kv(VERSION_KEY, coded).await?;
    }
    Ok(())
}
