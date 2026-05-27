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
        targets: vec!["ClaudeCode".to_string(), "OpenCode".to_string()],
    }
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
        .any(|r| r.owner == "ComposioHQ" && r.name == "awesome-claude-skills"));
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
async fn skill_upsert_then_get_round_trip() {
    let db = Db::spawn_in_memory().expect("spawn");
    let skill = fresh_skill("greenfield");
    db.upsert_skill_with_targets(skill.clone()).await.unwrap();

    let fetched = db.get_skill("greenfield").await.unwrap().unwrap();
    assert_eq!(fetched.skill_name, skill.skill_name);
    assert_eq!(fetched.targets, skill.targets);
}

#[tokio::test]
async fn skill_set_targets_replaces_set_atomically() {
    let db = Db::spawn_in_memory().expect("spawn");
    db.upsert_skill_with_targets(fresh_skill("foo"))
        .await
        .unwrap();
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
async fn skill_delete_cascades_targets() {
    let db = Db::spawn_in_memory().expect("spawn");
    db.upsert_skill_with_targets(fresh_skill("disposable"))
        .await
        .unwrap();
    db.delete_skill("disposable").await.unwrap();
    assert!(db.get_skill("disposable").await.unwrap().is_none());
    // skill_targets rows are removed via FK CASCADE; sanity-check via a
    // fresh row that should round-trip without target leaks.
    db.upsert_skill_with_targets(fresh_skill("disposable"))
        .await
        .unwrap();
    let row = db.get_skill("disposable").await.unwrap().unwrap();
    assert_eq!(
        row.targets,
        vec!["ClaudeCode".to_string(), "OpenCode".to_string()]
    );
}
