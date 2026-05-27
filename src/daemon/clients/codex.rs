// Repo:   https://github.com/openai/codex
// MCP:    $CODEX_HOME/config.toml (default ~/.codex/config.toml) — top-level `[mcp_servers.<name>]`; remote uses `url` only (transport inferred, no `type`). Source: https://raw.githubusercontent.com/openai/codex/main/codex-rs/config/src/mcp_types.rs
// Skills: ~/.codex/skills (deprecated; current is ~/.agents/skills, plus repo-local .agents/skills). Source: https://raw.githubusercontent.com/openai/codex/main/codex-rs/core-skills/src/loader.rs

use std::path::{Path, PathBuf};

pub(crate) const IDE_NAME: &str = "Codex";

pub(crate) fn config_path_under(home: &Path) -> PathBuf {
    home.join(".codex").join("config.toml")
}

pub(crate) fn config_path() -> Option<PathBuf> {
    if let Some(custom) = std::env::var_os("CODEX_HOME") {
        let p = PathBuf::from(custom);
        if !p.as_os_str().is_empty() {
            return Some(p.join("config.toml"));
        }
    }
    let home = dirs::home_dir()?;
    Some(config_path_under(&home))
}

// Uses $CODEX_HOME/skills (default ~/.codex/skills). This is the legacy
// per-tool skills path — still scanned by Codex for compat. The newer
// cross-agent standard is ~/.agents/skills, but writing there would affect
// every other agent on the system, not just Codex; per-IDE isolation wins.
pub(crate) fn skills_dir_under(home: &Path) -> PathBuf {
    home.join(".codex").join("skills")
}

pub(crate) fn skills_dir() -> Option<PathBuf> {
    if let Some(custom) = std::env::var_os("CODEX_HOME") {
        let p = PathBuf::from(custom);
        if !p.as_os_str().is_empty() {
            return Some(p.join("skills"));
        }
    }
    let home = dirs::home_dir()?;
    Some(skills_dir_under(&home))
}
