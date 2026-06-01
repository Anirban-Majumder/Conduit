//! Per-built-in skills-directory layout, resolved via `PathSpec` under a fake
//! home. "Supports skills" = `def.skills.is_some()` (no `SKILL_TARGET_IDES`).

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test ergonomics")]

use std::path::Path;

use conduit::daemon::clients::builtin_by_id;
use conduit::daemon::clients::paths::resolve_under;
use conduit::paths::Paths;

/// Resolve a built-in's skills dir under a fake home, or `None` if unsupported.
fn skills_under(id: &str, home: &Path) -> Option<std::path::PathBuf> {
    let def = builtin_by_id(id)?;
    def.skills.as_ref().map(|spec| resolve_under(spec, home))
}

#[test]
fn skills_dir_under_matches_per_ide_layout() {
    let home = Path::new("/test-home");
    assert_eq!(
        skills_under("ClaudeCode", home).unwrap(),
        home.join(".claude").join("skills")
    );
    assert_eq!(
        skills_under("Cursor", home).unwrap(),
        home.join(".cursor").join("skills")
    );
    assert_eq!(
        skills_under("OpenClaude", home).unwrap(),
        home.join(".openclaude").join("skills")
    );
    assert_eq!(
        skills_under("OpenCode", home).unwrap(),
        home.join(".config").join("opencode").join("skills")
    );
    assert_eq!(
        skills_under("Codex", home).unwrap(),
        home.join(".codex").join("skills")
    );
}

#[test]
fn vscode_copilot_has_no_skills_dir() {
    let home = Path::new("/test-home");
    assert!(skills_under("VsCodeCopilot", home).is_none());
}

#[test]
fn paths_skills_root_under_state_root() {
    let dir = std::env::temp_dir().join("conduit-skills-root-paths-test");
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
