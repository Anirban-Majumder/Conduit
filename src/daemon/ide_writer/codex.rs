// Codex CLI (OpenAI). Config lives at `$CODEX_HOME/config.toml`, default
// `~/.codex/config.toml` on every OS. Format is TOML — not JSON like the
// other five — and the section key is `mcp_servers` (snake_case), with
// HTTP servers expressed as `[mcp_servers.<name>]` tables holding a `url`.

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
