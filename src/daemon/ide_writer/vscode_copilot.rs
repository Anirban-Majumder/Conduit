// TODO: refine per-IDE shape — VS Code Copilot's stable schema uses `servers`
// rather than `mcpServers`. v0 ships the uniform shape; revisit during beta.

use std::path::{Path, PathBuf};

pub(crate) const IDE_NAME: &str = "VsCodeCopilot";

pub(crate) fn config_path_under(home: &Path) -> PathBuf {
    // Linux-style fallback used by tests. Real per-OS resolution goes through
    // dirs::config_dir() in config_path(), which picks Library/Application
    // Support on macOS and %APPDATA% on Windows automatically.
    home.join(".config")
        .join("Code")
        .join("User")
        .join("mcp.json")
}

pub(crate) fn config_path() -> Option<PathBuf> {
    let cfg = dirs::config_dir()?;
    Some(cfg.join("Code").join("User").join("mcp.json"))
}
