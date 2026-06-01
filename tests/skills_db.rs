#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test ergonomics")]

use conduit::daemon::db::types::{Skill, SkillRepo};
use conduit::daemon::db::Db;

fn fresh_repo(owner: &str) -> SkillRepo {
    SkillRepo {
        owner: owner.to_string(),
        name: "skills".to_string(),
        branch: "main".to_string(),
        enabled: true,
    }
}

fn fresh_skill(directory: &str) -> Skill {
    Skill {
        directory: directory.to_string(),
        repo_owner: "anthropics".to_string(),
        repo_name: "skills".to_string(),
        repo_branch: "main".to_string(),
        source_directory: format!("category/{directory}"),
        skill_name: directory.replace('-', " "),
        description: "auto-generated for test".to_string(),
        readme_url: Some(format!("https://example/{directory}")),
        installed_at: 1_700_000_000,
        // `targets` is read-only now; the write path is split (metadata vs targets).
        targets: Vec::new(),
    }
}

/// Register a built-in client so `managed_skill` FK inserts succeed.
async fn seed_client(db: &Db, id: &str) {
    db.project_builtin(id, id, "/tmp/cfg.json", Some("/tmp/skills"), "{}")
        .await
        .unwrap();
}

#[tokio::test]
async fn skill_repos_default_two_seed_rows() {
    let db = Db::spawn_in_memory().expect("spawn");
    let repos = db.list_skill_repos().await.expect("list");
    assert!(repos
        .iter()
        .any(|r| r.owner == "anthropics" && r.name == "skills"));
    assert!(repos
        .iter()
        .any(|r| r.owner == "mattpocock" && r.name == "skills"));
}

#[tokio::test]
async fn skill_repo_upsert_and_remove() {
    let db = Db::spawn_in_memory().expect("spawn");
    db.upsert_skill_repo(fresh_repo("user-x")).await.unwrap();
    let before = db.list_skill_repos().await.unwrap();
    assert!(before.iter().any(|r| r.owner == "user-x"));

    db.delete_skill_repo("user-x", "skills").await.unwrap();
    let after = db.list_skill_repos().await.unwrap();
    assert!(!after.iter().any(|r| r.owner == "user-x"));
}

#[tokio::test]
async fn skill_repo_toggle_persists() {
    let db = Db::spawn_in_memory().expect("spawn");
    db.set_skill_repo_enabled("anthropics", "skills", false)
        .await
        .unwrap();
    let row = db
        .list_skill_repos()
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.owner == "anthropics")
        .unwrap();
    assert!(!row.enabled);
}

#[tokio::test]
async fn skill_metadata_upsert_then_get_round_trip() {
    let db = Db::spawn_in_memory().expect("spawn");
    let skill = fresh_skill("greenfield");
    db.upsert_skill_metadata(skill.clone()).await.unwrap();

    let fetched = db.get_skill("greenfield").await.unwrap().unwrap();
    assert_eq!(fetched.skill_name, skill.skill_name);
    assert!(fetched.targets.is_empty());
}

#[tokio::test]
async fn skill_set_targets_replaces_set_atomically() {
    let db = Db::spawn_in_memory().expect("spawn");
    seed_client(&db, "OpenClaude").await;
    db.upsert_skill_metadata(fresh_skill("foo")).await.unwrap();

    db.set_skill_targets("foo", vec!["OpenClaude".to_string()])
        .await
        .unwrap();
    let row = db.get_skill("foo").await.unwrap().unwrap();
    assert_eq!(row.targets, vec!["OpenClaude".to_string()]);

    db.set_skill_targets("foo", Vec::new()).await.unwrap();
    let row = db.get_skill("foo").await.unwrap().unwrap();
    assert!(row.targets.is_empty());
}

#[tokio::test]
async fn skill_add_and_remove_single_target() {
    let db = Db::spawn_in_memory().expect("spawn");
    seed_client(&db, "ClaudeCode").await;
    seed_client(&db, "OpenCode").await;
    db.upsert_skill_metadata(fresh_skill("bar")).await.unwrap();

    db.add_skill_target("bar", "ClaudeCode").await.unwrap();
    db.add_skill_target("bar", "OpenCode").await.unwrap();
    // Idempotent add.
    db.add_skill_target("bar", "OpenCode").await.unwrap();
    let mut targets = db.get_skill("bar").await.unwrap().unwrap().targets;
    targets.sort();
    assert_eq!(
        targets,
        vec!["ClaudeCode".to_string(), "OpenCode".to_string()]
    );

    db.remove_skill_target("bar", "ClaudeCode").await.unwrap();
    let targets = db.get_skill("bar").await.unwrap().unwrap().targets;
    assert_eq!(targets, vec!["OpenCode".to_string()]);
}

/// D2 gate: a metadata re-sync must NOT wipe user-set client targets.
///
/// The write path is deliberately split — `upsert_skill_metadata` touches only
/// the `skills` row. The old combined `upsert_skill_with_targets`
/// delete-then-reinserted `managed_skill`, so any refresh carrying a stale
/// (empty) targets vec destroyed user intent under `ON DELETE CASCADE`. This
/// test fails against that old behavior.
#[tokio::test]
async fn metadata_resync_preserves_user_targets() {
    let db = Db::spawn_in_memory().expect("spawn");
    seed_client(&db, "ClaudeCode").await;
    seed_client(&db, "OpenCode").await;

    // Install + the user picks two targets.
    db.upsert_skill_metadata(fresh_skill("research"))
        .await
        .unwrap();
    db.set_skill_targets(
        "research",
        vec!["ClaudeCode".to_string(), "OpenCode".to_string()],
    )
    .await
    .unwrap();

    // A later refresh re-syncs metadata only (note: fresh_skill has NO targets,
    // exactly the stale vec that used to wipe intent).
    let mut refreshed = fresh_skill("research");
    refreshed.description = "updated upstream description".to_string();
    db.upsert_skill_metadata(refreshed).await.unwrap();

    let row = db.get_skill("research").await.unwrap().unwrap();
    assert_eq!(row.description, "updated upstream description");
    let mut targets = row.targets;
    targets.sort();
    assert_eq!(
        targets,
        vec!["ClaudeCode".to_string(), "OpenCode".to_string()],
        "metadata re-sync must not wipe user-set targets",
    );
}

#[tokio::test]
async fn skill_delete_cascades_targets() {
    let db = Db::spawn_in_memory().expect("spawn");
    seed_client(&db, "ClaudeCode").await;
    db.upsert_skill_metadata(fresh_skill("disposable"))
        .await
        .unwrap();
    db.add_skill_target("disposable", "ClaudeCode")
        .await
        .unwrap();

    db.delete_skill("disposable").await.unwrap();
    assert!(db.get_skill("disposable").await.unwrap().is_none());

    // A fresh row with the same directory starts with zero targets — the old
    // managed_skill rows were removed via FK CASCADE, no leak.
    db.upsert_skill_metadata(fresh_skill("disposable"))
        .await
        .unwrap();
    let row = db.get_skill("disposable").await.unwrap().unwrap();
    assert!(row.targets.is_empty());
}
