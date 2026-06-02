//! Phase 5b projection backfill (§3c).
//!
//! The detect→DB projection fills the two new `project_config_rel` /
//! `project_skills_rel` columns, and a stale/absent `client_def_version`
//! (what `0002`'s DELETE guarantees on upgrade) forces re-projection so rows
//! that predate `0002` (NULL columns) get backfilled rather than silently
//! no-op'ing on repo writes.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test ergonomics")]

use std::path::PathBuf;

use conduit::client::claude_code::DEF as CLAUDE;
use conduit::daemon::client_config::detect::{DetectedClient, DetectionSource};
use conduit::daemon::client_config::project::project_builtins;
use conduit::daemon::db::Db;

fn detected_claude() -> DetectedClient {
    DetectedClient {
        def: &CLAUDE,
        source: DetectionSource::Path(PathBuf::from("/usr/bin/claude")),
    }
}

#[tokio::test]
async fn projection_fills_repo_relative_columns() {
    let db = Db::spawn_in_memory().unwrap();
    project_builtins(&db, &[detected_claude()]).await.unwrap();

    let row = db.get_client("ClaudeCode").await.unwrap().expect("row");
    assert_eq!(row.project_config_rel.as_deref(), Some(".mcp.json"));
    assert_eq!(row.project_skills_rel.as_deref(), Some(".claude/skills"));
}

#[tokio::test]
async fn stale_version_reprojects_and_backfills_null_columns() {
    let db = Db::spawn_in_memory().unwrap();

    // Simulate an old projection: a row exists with NULL repo-relative columns.
    db.project_builtin(
        "ClaudeCode",
        "Claude Code",
        "/home/u/.claude.json",
        Some("/home/u/.claude/skills"),
        "{}",
        None,
        None,
    )
    .await
    .unwrap();
    db.set_settings_kv("client_def_version", "0.0.0-stale")
        .await
        .unwrap();
    let before = db.get_client("ClaudeCode").await.unwrap().unwrap();
    assert_eq!(before.project_config_rel, None);

    // Startup projection must re-fill the columns because the version changed.
    project_builtins(&db, &[detected_claude()]).await.unwrap();

    let after = db.get_client("ClaudeCode").await.unwrap().unwrap();
    assert_eq!(after.project_config_rel.as_deref(), Some(".mcp.json"));
    assert_eq!(after.project_skills_rel.as_deref(), Some(".claude/skills"));
}
