//! Phase 5b scoped skills sync (§7).
//!
//! The pure `sync` module mirrors a skill into an absolute destination dir using
//! the configured `sync_mode` (symlink|copy) — no IDE-module reach. The manager
//! drives the `(client, scope)` target list from the DB and resolves each via
//! `ResolvedClient::skills_target`, skipping a client that can't host skills at
//! that scope (VS Code at a repo).

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test ergonomics")]

use std::fs;
use std::path::PathBuf;

use conduit::daemon::config::toml::SyncMode;
use conduit::daemon::db::types::Skill;
use conduit::daemon::db::Db;
use conduit::daemon::events::Bus;
use conduit::daemon::paths::Paths;
use conduit::daemon::skills::{sync, SkillManager};

fn fresh_tmp(label: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("conduit-skills-scope-{label}-{ns}"))
}

fn skill(directory: &str) -> Skill {
    Skill {
        directory: directory.into(),
        repo_owner: "o".into(),
        repo_name: "r".into(),
        repo_branch: "main".into(),
        source_directory: directory.into(),
        skill_name: directory.into(),
        description: String::new(),
        readme_url: None,
        installed_at: 0,
        targets: Vec::new(),
    }
}

fn seed_source(managed_dir: &std::path::Path, name: &str) -> PathBuf {
    let src = managed_dir.join(name);
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("SKILL.md"), "---\nname: My\ndescription: d\n---\n").unwrap();
    src
}

#[test]
fn sync_to_target_copy_mode_creates_real_directory() {
    let root = fresh_tmp("copy");
    let src = seed_source(&root.join("src"), "my-skill");
    let dest_dir = root.join("repo/.cursor/skills");

    sync::sync_to_target(&src, &dest_dir, "my-skill", SyncMode::Copy, true).unwrap();

    let dest = dest_dir.join("my-skill");
    assert!(dest.join("SKILL.md").is_file());
    assert!(
        !fs::symlink_metadata(&dest)
            .unwrap()
            .file_type()
            .is_symlink(),
        "copy mode must produce a real directory"
    );
    let _ignored = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn sync_to_target_symlink_mode_creates_symlink() {
    let root = fresh_tmp("symlink");
    let src = seed_source(&root.join("src"), "my-skill");
    let dest_dir = root.join("repo/.cursor/skills");

    sync::sync_to_target(&src, &dest_dir, "my-skill", SyncMode::Symlink, true).unwrap();

    let dest = dest_dir.join("my-skill");
    assert!(
        fs::symlink_metadata(&dest)
            .unwrap()
            .file_type()
            .is_symlink(),
        "symlink mode must produce a symlink"
    );
    assert!(
        dest.join("SKILL.md").is_file(),
        "symlink resolves to source"
    );
    let _ignored = fs::remove_dir_all(&root);
}

#[test]
fn remove_from_target_removes_mirrored_skill() {
    let root = fresh_tmp("remove");
    let src = seed_source(&root.join("src"), "my-skill");
    let dest_dir = root.join("repo/.cursor/skills");
    sync::sync_to_target(&src, &dest_dir, "my-skill", SyncMode::Copy, true).unwrap();
    assert!(dest_dir.join("my-skill").exists());

    sync::remove_from_target(&dest_dir, "my-skill");
    assert!(!dest_dir.join("my-skill").exists());
    let _ignored = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn manager_set_skill_scope_mirrors_repo_and_skips_vscode() {
    let root = fresh_tmp("mgr");
    let home = root.join("home");
    let repo = root.join("repo");
    fs::create_dir_all(&repo).unwrap();
    let paths = Paths::under(root.join("conduit"));
    paths.ensure_created().unwrap();
    let db = Db::spawn_in_memory().unwrap();
    let mgr = SkillManager::new(paths.clone(), db.clone(), Bus::new()).unwrap();

    seed_source(&paths.skills_managed_dir(), "my-skill");
    db.upsert_skill_metadata(skill("my-skill")).await.unwrap();

    // Cursor has repo skills; VS Code Copilot does not.
    db.project_builtin(
        "Cursor",
        "Cursor",
        "/cfg",
        Some(home.join(".cursor/skills").to_str().unwrap()),
        "{}",
        Some(".cursor/mcp.json"),
        Some(".cursor/skills"),
    )
    .await
    .unwrap();
    db.project_builtin(
        "VsCodeCopilot",
        "VS Code",
        "/cfg",
        None,
        "{}",
        Some(".vscode/mcp.json"),
        None,
    )
    .await
    .unwrap();

    let repo_scope = repo.display().to_string();

    // Cursor repo scope → mirrored + DB row recorded.
    mgr.set_skill_scope("my-skill", "Cursor", &repo_scope, true)
        .await
        .unwrap();
    assert!(
        repo.join(".cursor/skills/my-skill").exists(),
        "skill mirrored into repo .cursor/skills"
    );
    let targets = db.list_skill_targets_scoped("my-skill").await.unwrap();
    assert!(targets.contains(&("Cursor".to_string(), repo_scope.clone())));

    // VS Code at a repo scope: no project skills → fully skipped (no file, no row).
    mgr.set_skill_scope("my-skill", "VsCodeCopilot", &repo_scope, true)
        .await
        .unwrap();
    assert!(
        !repo.join(".vscode").exists(),
        "VS Code has no repo skills dir; nothing written"
    );
    let targets = db.list_skill_targets_scoped("my-skill").await.unwrap();
    assert!(
        !targets.iter().any(|(c, _)| c == "VsCodeCopilot"),
        "no managed_skill row for an unsupported (client, scope)"
    );

    // Toggling off removes the mirror + the row.
    mgr.set_skill_scope("my-skill", "Cursor", &repo_scope, false)
        .await
        .unwrap();
    assert!(!repo.join(".cursor/skills/my-skill").exists());
    assert!(db
        .list_skill_targets_scoped("my-skill")
        .await
        .unwrap()
        .is_empty());

    let _ignored = fs::remove_dir_all(&root);
}

/// SK-1 (install path): when a global skills dir already holds user-authored
/// content that Conduit has no `managed_skill` row for, reconcile must refuse
/// with `TargetExists` and leave the content untouched.
///
/// This mirrors the fix in `install()`: reconcile runs BEFORE `set_skill_targets`
/// writes new rows, so `we_manage=false` for a brand-new target.
#[tokio::test]
async fn reconcile_global_targets_refuses_unmanaged_destination() {
    let root = fresh_tmp("install-sk1");
    let home = root.join("home");
    let paths = Paths::under(root.join("conduit"));
    paths.ensure_created().unwrap();
    let db = Db::spawn_in_memory().unwrap();
    let mgr = SkillManager::new(paths.clone(), db.clone(), Bus::new()).unwrap();

    seed_source(&paths.skills_managed_dir(), "my-skill");
    db.upsert_skill_metadata(skill("my-skill")).await.unwrap();

    let global_skills_dir = home.join(".cursor/skills");
    // Pre-existing user-authored content at the destination — no managed_skill row.
    let user_dest = global_skills_dir.join("my-skill");
    fs::create_dir_all(&user_dest).unwrap();
    fs::write(user_dest.join("user-file.txt"), "user content").unwrap();

    db.project_builtin(
        "Cursor",
        "Cursor",
        "/cfg",
        Some(global_skills_dir.to_str().unwrap()),
        "{}",
        Some(".cursor/mcp.json"),
        Some(".cursor/skills"),
    )
    .await
    .unwrap();

    // set_targets calls reconcile_global_targets with no prior managed_skill rows
    // → we_manage=false → must refuse TargetExists, not overwrite.
    let result = mgr
        .set_targets("my-skill", vec!["Cursor".to_string()])
        .await;
    assert!(
        result.is_err(),
        "set_targets must refuse to overwrite an unmanaged global destination"
    );
    assert!(
        user_dest.join("user-file.txt").exists(),
        "user-authored file must not be deleted"
    );

    let _ignored = fs::remove_dir_all(&root);
}
