//! `OpenClaude` config-path and skills-dir resolution.

// Repo:   https://github.com/Gitlawb/openclaude
// MCP:    ~/.openclaude.json — top-level `mcpServers`; schema `{type:"http", url}` (McpHTTPServerConfigSchema). Source: https://raw.githubusercontent.com/Gitlawb/openclaude/main/src/services/mcp/types.ts
// Skills: ~/.openclaude/skills/<name>/SKILL.md (resolved via getClaudeConfigHomeDir()). Source: https://raw.githubusercontent.com/Gitlawb/openclaude/main/src/skills/loadSkillsDir.ts

use std::path::{Path, PathBuf};

pub(crate) const IDE_NAME: &str = "OpenClaude";
pub(crate) const ICON_SVG: &[u8] = include_bytes!("../../../assets/client/openclaude.svg");

pub(crate) fn config_path_under(home: &Path) -> PathBuf {
    home.join(".openclaude.json")
}

pub(crate) fn config_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(config_path_under(&home))
}

pub(crate) fn skills_dir_under(home: &Path) -> PathBuf {
    home.join(".openclaude").join("skills")
}

pub(crate) fn skills_dir() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(skills_dir_under(&home))
}
