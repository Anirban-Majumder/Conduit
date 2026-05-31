//! VS Code Copilot config-path resolution (no global skills directory).

// Repo:   https://github.com/microsoft/vscode-copilot-chat
// MCP:    <Code/User>/mcp.json — top-level `servers`; schema `{type:"http", url}`. Source: https://code.visualstudio.com/docs/copilot/reference/mcp-configuration
// Skills: not supported (prompt files are workspace-scoped only: .github/prompts/*.prompt.md). Source: https://code.visualstudio.com/docs/copilot/customization/prompt-files

use std::path::{Path, PathBuf};

pub(crate) const IDE_NAME: &str = "VsCodeCopilot";
pub(crate) const ICON_SVG: &[u8] = include_bytes!("../../../assets/client/copilot.svg");

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
            top_level_key: "servers",
            server_type: Some("http"),
        })
    }
    fn config_path(&self) -> Option<PathBuf> {
        config_path()
    }
    fn config_path_under(&self, home: &Path) -> PathBuf {
        config_path_under(home)
    }
    // VS Code Copilot has no global skills directory (workspace-scoped only).
    fn skills_dir(&self) -> Option<PathBuf> {
        None
    }
    fn skills_dir_under(&self, _home: &Path) -> Option<PathBuf> {
        None
    }
}
