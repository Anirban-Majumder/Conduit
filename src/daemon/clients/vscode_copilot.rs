//! VS Code Copilot config-path resolution (no global skills directory).

// Repo:   https://github.com/microsoft/vscode-copilot-chat
// MCP:    <Code/User>/mcp.json — top-level `servers`; schema `{type:"http", url}`. Source: https://code.visualstudio.com/docs/copilot/reference/mcp-configuration
// Skills: not supported (prompt files are workspace-scoped only: .github/prompts/*.prompt.md). Source: https://code.visualstudio.com/docs/copilot/customization/prompt-files

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
