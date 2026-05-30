//! Cursor config-path and skills-dir resolution.

// Repo:   N/A (closed-source; vendor: Anysphere)
// MCP:    ~/.cursor/mcp.json — top-level `mcpServers`; remote servers use `{url}` only (no `type`). Source: https://cursor.com/docs/context/mcp
// Skills: ~/.cursor/skills/ (auto-discovered SKILL.md folders) — distinct from `.cursor/rules`. Source: https://cursor.com/docs/context/skills

use std::path::{Path, PathBuf};

pub(crate) const IDE_NAME: &str = "Cursor";
pub(crate) const ICON_SVG: &[u8] = include_bytes!("../../../assets/client/cursor.svg");

pub(crate) fn config_path_under(home: &Path) -> PathBuf {
    home.join(".cursor").join("mcp.json")
}

pub(crate) fn config_path() -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        let appdata = dirs::config_dir()?;
        return Some(appdata.join("Cursor").join("mcp.json"));
    }
    let home = dirs::home_dir()?;
    Some(config_path_under(&home))
}

pub(crate) fn skills_dir_under(home: &Path) -> PathBuf {
    home.join(".cursor").join("skills")
}

pub(crate) fn skills_dir() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(skills_dir_under(&home))
}
