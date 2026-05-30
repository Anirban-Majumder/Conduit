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

pub(super) static SPEC: Spec = Spec;

pub(super) struct Spec;

impl super::ClientSpec for Spec {
    fn ide_name(&self) -> &'static str { IDE_NAME }
    fn icon_svg(&self) -> &'static [u8] { ICON_SVG }
    fn json_schema(&self) -> Option<super::JsonSchema> {
        Some(super::JsonSchema { top_level_key: "mcpServers", server_type: Some("http") })
    }
    fn config_path(&self) -> Option<PathBuf> { config_path() }
    fn config_path_under(&self, home: &Path) -> PathBuf { config_path_under(home) }
    fn skills_dir(&self) -> Option<PathBuf> { skills_dir() }
    fn skills_dir_under(&self, home: &Path) -> Option<PathBuf> { Some(skills_dir_under(home)) }
}
