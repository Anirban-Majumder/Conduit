//! VS Code Copilot — static client definition (no global skills directory).

// Repo:   https://github.com/microsoft/vscode-copilot-chat
// MCP:    <Code/User>/mcp.json — top-level `servers`; schema `{type:"http", url}`.
// Skills: not supported (prompt files are workspace-scoped only).

use crate::daemon::client_config::paths::PathSpec;
use crate::daemon::client_config::types::{ConfigFormat, ConfigSpec};
use crate::daemon::client_config::ClientDef;

pub static DEF: ClientDef = ClientDef {
    id: "VsCodeCopilot",
    display_name: "VS Code Copilot",
    icon_svg: include_bytes!("../../assets/client/copilot.svg"),
    config: PathSpec::ConfigDir("Code/User/mcp.json"),
    skills: None,
    config_spec: ConfigSpec {
        format: ConfigFormat::Jsonc,
        key_path: &["servers"],
        entry_template: r#"{"type":"http","url":"${url}"}"#,
    },
    binaries: &["code"],
    #[cfg(target_os = "macos")]
    mac_app: Some("Visual Studio Code.app"),
    #[cfg(target_os = "windows")]
    win_paths: &[
        "$LOCALAPPDATA\\Programs\\Microsoft VS Code\\bin\\code.cmd",
        "C:\\Program Files\\Microsoft VS Code\\bin\\code.cmd",
    ],
};
