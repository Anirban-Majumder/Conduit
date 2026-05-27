// Repo:   https://github.com/sst/opencode
// MCP:    ~/.config/opencode/opencode.json (XDG on Linux+macOS, NOT ~/Library) — top-level `mcp`; schema `{type:"remote", url}`. Source: https://raw.githubusercontent.com/sst/opencode/main/packages/opencode/src/config/mcp.ts
// Skills: ~/.config/opencode/skills/<name>/SKILL.md (one of several scan paths). Source: https://raw.githubusercontent.com/sst/opencode/main/packages/opencode/src/skill/index.ts

use std::path::{Path, PathBuf};

pub(crate) const IDE_NAME: &str = "OpenCode";

pub(crate) fn config_path_under(home: &Path) -> PathBuf {
    home.join(".config").join("opencode").join("opencode.json")
}

pub(crate) fn config_path() -> Option<PathBuf> {
    // Spec pins OpenCode to ~/.config on Linux AND macOS (not Library on mac).
    #[cfg(target_os = "windows")]
    {
        let appdata = dirs::config_dir()?;
        return Some(appdata.join("opencode").join("opencode.json"));
    }
    #[cfg(not(target_os = "windows"))]
    {
        let home = dirs::home_dir()?;
        Some(config_path_under(&home))
    }
}

pub(crate) fn skills_dir_under(home: &Path) -> PathBuf {
    home.join(".config").join("opencode").join("skills")
}

pub(crate) fn skills_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let appdata = dirs::config_dir()?;
        return Some(appdata.join("opencode").join("skills"));
    }
    #[cfg(not(target_os = "windows"))]
    {
        let home = dirs::home_dir()?;
        Some(skills_dir_under(&home))
    }
}
