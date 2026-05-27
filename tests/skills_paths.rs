#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test ergonomics")]

use std::path::Path;

use mcp_router::daemon::ide_writer::{
    ide_db_name, ide_kind_from_db_name, skills_dir_under, SKILL_TARGET_IDES,
};
use mcp_router::paths::Paths;

#[test]
fn ide_db_name_round_trips_through_kind_lookup() {
    for ide in SKILL_TARGET_IDES {
        let name = ide_db_name(*ide);
        let back = ide_kind_from_db_name(name).expect("known db name");
        assert_eq!(back, *ide);
    }
}

#[test]
fn skills_dir_under_matches_per_ide_layout() {
    let home = Path::new("/test-home");
    let claude = skills_dir_under(ide_kind_from_db_name("ClaudeCode").unwrap(), home).unwrap();
    assert_eq!(claude, home.join(".claude").join("skills"));

    let openclaude = skills_dir_under(ide_kind_from_db_name("OpenClaude").unwrap(), home).unwrap();
    assert_eq!(openclaude, home.join(".openclaude").join("skills"));

    let opencode = skills_dir_under(ide_kind_from_db_name("OpenCode").unwrap(), home).unwrap();
    assert_eq!(
        opencode,
        home.join(".config").join("opencode").join("skills")
    );
}

#[test]
fn skills_dir_under_returns_none_for_non_agentic_ides() {
    let home = Path::new("/test-home");
    assert!(skills_dir_under(ide_kind_from_db_name("Cursor").unwrap(), home,).is_none());
    assert!(skills_dir_under(ide_kind_from_db_name("VsCodeCopilot").unwrap(), home,).is_none());
}

#[test]
fn paths_skills_root_under_state_root() {
    let dir = std::env::temp_dir().join("mcp-router-skills-root-paths-test");
    let _ignored = std::fs::remove_dir_all(&dir);
    let paths = Paths::under(&dir);
    assert_eq!(paths.skills_root(), dir.join("skills"));
    assert_eq!(
        paths.skills_managed_dir(),
        dir.join("skills").join("managed")
    );

    paths.ensure_created().unwrap();
    assert!(paths.skills_managed_dir().is_dir());
    let _ignored = std::fs::remove_dir_all(&dir);
}
