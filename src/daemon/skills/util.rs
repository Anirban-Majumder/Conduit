//! Path, target-name, and timestamp helpers for the skills subsystem.
//!
//! Pure functions used by install/sync: traversal-safe filename and
//! relative-path sanitation, target-IDE normalization, SKILL.md location, and
//! unix timestamps.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::daemon::clients::{self, SKILL_TARGET_IDES};
use crate::daemon::errors::SkillError;

pub(super) fn normalize_targets(targets: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = targets
        .into_iter()
        .filter(|t| {
            SKILL_TARGET_IDES
                .iter()
                .any(|ide| clients::ide_db_name(*ide) == t)
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

pub(super) fn sanitize_segment(segment: &str) -> Option<String> {
    let s = segment.trim();
    if s.is_empty() || s == "." || s == ".." {
        return None;
    }
    if s.contains('/') || s.contains('\\') || s.contains('\0') {
        return None;
    }
    Some(s.to_string())
}

pub(super) fn sanitize_relative(value: &str) -> Option<String> {
    let raw = value.replace('\\', "/");
    let raw = raw.trim();
    if raw.is_empty() || raw.starts_with('/') || raw.contains('\0') {
        return None;
    }
    let parts: Vec<&str> = raw.split('/').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() || parts.iter().any(|p| *p == "." || *p == "..") {
        return None;
    }
    Some(parts.join("/"))
}

pub(super) fn last_segment(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

pub(super) fn relative_to_source(path: &str, source_dir: &str) -> String {
    if path == source_dir {
        last_segment(path).to_string()
    } else {
        let prefix = format!("{source_dir}/");
        path.strip_prefix(&prefix).unwrap_or(path).to_string()
    }
}

pub(super) async fn locate_skill_md(dir: &std::path::Path) -> Result<PathBuf, SkillError> {
    if let Some(p) = first_existing_skill_md(dir).await {
        return Ok(p);
    }
    Err(SkillError::MissingSkillFile {
        repo: dir.display().to_string(),
        dir: String::new(),
    })
}

pub(super) async fn first_existing_skill_md(dir: &std::path::Path) -> Option<PathBuf> {
    for name in ["SKILL.md", "Skill.md", "skill.md"] {
        let candidate = dir.join(name);
        if tokio::fs::try_exists(&candidate).await.unwrap_or(false) {
            return Some(candidate);
        }
    }
    None
}

pub(super) fn now_unix() -> i64 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    i64::try_from(secs).unwrap_or(i64::MAX)
}

pub(super) fn now_unix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "test ergonomics")]
    use super::*;

    #[test]
    fn sanitize_rejects_traversal() {
        assert!(sanitize_relative("../etc/passwd").is_none());
        assert!(sanitize_relative("/abs/path").is_none());
        assert!(sanitize_relative("a/./b").is_none());
        assert_eq!(sanitize_relative("a/b/c").unwrap(), "a/b/c");
        assert_eq!(sanitize_relative("a\\b").unwrap(), "a/b");
    }

    #[test]
    fn segment_strips_whitespace_and_separators() {
        assert_eq!(sanitize_segment(" my-skill ").unwrap(), "my-skill");
        assert!(sanitize_segment("..").is_none());
        assert!(sanitize_segment("a/b").is_none());
    }

    #[test]
    fn relative_to_source_strips_prefix() {
        assert_eq!(relative_to_source("dir/SKILL.md", "dir"), "SKILL.md");
        assert_eq!(relative_to_source("dir/a/b.md", "dir"), "a/b.md");
        // root-level file (path == source_dir) → use file_name
        assert_eq!(relative_to_source("SKILL.md", "SKILL.md"), "SKILL.md");
    }

    #[test]
    fn normalize_targets_keeps_known_ides_only() {
        let out = normalize_targets(vec![
            "ClaudeCode".into(),
            "Cursor".into(),
            "OpenCode".into(),
            "VsCodeCopilot".into(), // no global skills dir — filtered out
            "Bogus".into(),
            "ClaudeCode".into(),
        ]);
        assert_eq!(
            out,
            vec![
                "ClaudeCode".to_string(),
                "Cursor".to_string(),
                "OpenCode".to_string(),
            ]
        );
    }
}
