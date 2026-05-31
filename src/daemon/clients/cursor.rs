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

pub(super) static SPEC: Spec = Spec;

pub(super) struct Spec;

impl super::ClientSpec for Spec {
    fn ide_name(&self) -> &'static str {
        IDE_NAME
    }
    fn icon_svg(&self) -> &'static [u8] {
        ICON_SVG
    }
    fn json_schema(&self) -> Option<super::JsonSchema> {
        Some(super::JsonSchema {
            top_level_key: "mcpServers",
            server_type: None,
        })
    }
    fn config_path(&self) -> Option<PathBuf> {
        config_path()
    }
    fn config_path_under(&self, home: &Path) -> PathBuf {
        config_path_under(home)
    }
    fn skills_dir(&self) -> Option<PathBuf> {
        skills_dir()
    }
    fn skills_dir_under(&self, home: &Path) -> Option<PathBuf> {
        Some(skills_dir_under(home))
    }
}
