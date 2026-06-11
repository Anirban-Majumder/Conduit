//! The detect→DB projection (§1a). Runs ONCE at startup, before any consumer
//! reads the `client` table.
//!
//! Built-in identity (name / config path / schema / skills dir) is code, painted
//! into `client` rows here. It changes only on a Conduit upgrade (a const moved)
//! or the first time a client is seen — so identity columns are re-projected
//! only when `client_def_version` differs from `CARGO_PKG_VERSION`, or when the
//! row is absent. Every detect bumps `last_seen_at`. Custom rows (`is_builtin=0`)
//! are never in `detected` and are never touched.

use std::collections::HashMap;

use super::detect::DetectedClient;
use super::types::{ConfigSchema, ResolvedClient, Scope};
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
            // CC-13: skip projection when the base dir is unavailable (e.g. no
            // home dir in a headless CI environment). An empty-string path stored
            // in the DB later causes a misleading NonUtf8Path / empty-path error;
            // better to warn and skip than to persist a broken row.
            let Some(resolved_config) = super::paths::resolve(&def.config) else {
                tracing::warn!(
                    client_id = %def.id,
                    "project_builtins: cannot resolve config path (base dir unavailable); skipping"
                );
                continue;
            };
            let config_path = resolved_config.display().to_string();
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
                def.project_config_rel,
                def.project_skills_rel,
            )
            .await?;
        } else {
            db.touch_client(def.id).await?;
        }
    }

    if version_changed {
        db.set_settings_kv(VERSION_KEY, coded).await?;
    }

    reconcile_orphaned_builtins(db).await?;
    Ok(())
}

/// Untrack built-in rows whose `id` is no longer in `BUILTINS`.
///
/// A def removed or renamed across a Conduit upgrade leaves a row `builtin_by_id`
/// can't resolve; left tracked it'd still show "active" and stay a live
/// cascade/render target. We untrack rather than delete so a future
/// rename-migration can still reach the row's `mcp_key` + `managed_*` data.
/// Re-derive every tracked built-in's on-disk MCP config from current `managed_mcp`.
///
/// Runs for global and each repo scope a client manages. Repairs divergence
/// left by an interrupted cascade/toggle (DB committed, file rewrite never ran):
/// the DB is authoritative, the config file is a derived cache. Idempotent — the
/// renderer's write path skips byte-identical files, so a consistent tree writes
/// nothing. Best-effort: a single client/scope failure is logged and skipped.
pub async fn reconcile_configs(db: &Db, port: u16) -> Result<(), DbError> {
    let all_rows = db.list_tracked_clients().await?;
    // CC-12: check for clients sharing the same config file + key_path.
    let all_clients: Vec<ResolvedClient> = all_rows
        .iter()
        .filter(|r| r.is_builtin)
        .map(|r| ResolvedClient::from_row(r.clone()))
        .collect();
    warn_config_conflicts(&all_clients, &Scope::Global);

    for row in all_rows {
        if !row.is_builtin {
            continue;
        }
        let client = ResolvedClient::from_row(row);
        let mut scopes = vec!["global".to_string()];
        for s in db.list_scopes_for_client(&client.id).await? {
            if s != "global" {
                scopes.push(s);
            }
        }
        for scope_str in scopes {
            let remaining = db.list_managed_servers(&client.id, &scope_str).await?;
            let scope = Scope::from_db(&scope_str);
            let client_for_render = client.clone();
            let render = tokio::task::spawn_blocking(move || {
                super::render_scope(&client_for_render, &scope, &remaining, port)
            })
            .await;
            match render {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    tracing::warn!(client_id = %client.id, scope = %scope_str, error = %e, "reconcile_configs: render failed");
                }
                Err(e) => {
                    tracing::warn!(client_id = %client.id, scope = %scope_str, error = %e, "reconcile_configs: render join failed");
                }
            }
        }
    }
    Ok(())
}

/// CC-12: warn when two tracked clients write to the same physical config file
/// with the same `key_path` prefix (e.g. `ClaudeCode` + `OpenClaude` both targeting
/// `.mcp.json` / `mcpServers` in repo scope). Detection only — no behavior change.
fn warn_config_conflicts(clients: &[ResolvedClient], scope: &Scope) {
    // Key: (absolute config path, key_path as string) → first client id that owns it
    let mut seen: HashMap<(std::path::PathBuf, String), String> = HashMap::new();
    for client in clients {
        let Some(target) = client.config_target(scope) else {
            continue;
        };
        let key_str = client.schema.key_path.join(".");
        let map_key = (target, key_str);
        if let Some(first_id) = seen.get(&map_key) {
            tracing::warn!(
                client_a = %first_id,
                client_b = %client.id,
                path = %map_key.0.display(),
                key_path = %map_key.1,
                "CC-12: two tracked clients write to the same config file and key_path; \
                 they will clobber each other on every render"
            );
        } else {
            seen.insert(map_key, client.id.clone());
        }
    }
}

async fn reconcile_orphaned_builtins(db: &Db) -> Result<(), DbError> {
    let known: std::collections::HashSet<&str> = super::BUILTINS.iter().map(|b| b.id).collect();
    for row in db.list_clients().await? {
        if row.is_builtin && row.tracked && !known.contains(row.id.as_str()) {
            tracing::warn!(
                client_id = %row.id,
                "projection: built-in no longer in BUILTINS; untracking orphaned client row"
            );
            db.set_client_tracked(&row.id, false).await?;
        }
    }
    Ok(())
}
