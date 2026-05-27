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
