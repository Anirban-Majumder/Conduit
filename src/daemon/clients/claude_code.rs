//! Claude Code config-path and skills-dir resolution.

// Repo:   N/A (closed-source CLI; SDK at https://github.com/anthropics/claude-code-sdk-ts)
// MCP:    ~/.claude.json — top-level `mcpServers` (user scope); schema `{type:"http", url}`. Source: https://code.claude.com/docs/en/mcp.md
// Skills: ~/.claude/skills/<name>/SKILL.md — auto-loaded at startup. Source: https://code.claude.com/docs/en/skills.md

use std::path::{Path, PathBuf};

pub(crate) const IDE_NAME: &str = "ClaudeCode";
pub(crate) const ICON_SVG: &[u8] = include_bytes!("../../../assets/client/claude-code.svg");

pub(crate) fn config_path_under(home: &Path) -> PathBuf {
    home.join(".claude.json")
}

pub(crate) fn config_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(config_path_under(&home))
}

pub(crate) fn skills_dir_under(home: &Path) -> PathBuf {
    home.join(".claude").join("skills")
}

pub(crate) fn skills_dir() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(skills_dir_under(&home))
}
