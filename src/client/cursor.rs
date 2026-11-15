//! Cursor — static client definition.

// Repo:   N/A (closed-source; vendor: Anysphere)
// MCP:    ~/.cursor/mcp.json — top-level `mcpServers`; remote servers use `{url}` only.
// Skills: ~/.cursor/skills/ (auto-discovered SKILL.md folders).

use crate::daemon::client_config::paths::PathSpec;
use crate::daemon::client_config::types::{ConfigFormat, ConfigSpec};
use crate::daemon::client_config::ClientDef;

pub static DEF: ClientDef = ClientDef {
    id: "Cursor",
    display_name: "Cursor",
    icon_svg: include_bytes!("../../assets/client/cursor.svg"),
    config: PathSpec::Home(".cursor/mcp.json"),
    skills: Some(PathSpec::Home(".cursor/skills")),
    project_config_rel: Some(".cursor/mcp.json"),
    project_skills_rel: Some(".cursor/skills"),
    config_spec: ConfigSpec {
        format: ConfigFormat::Json,
        key_path: &["mcpServers"],
        entry_template: r#"{"url":"${url}"}"#,
    },
    binaries: &["cursor"],
    #[cfg(target_os = "macos")]
    mac_app: Some("Cursor.app"),
    #[cfg(target_os = "windows")]
    win_paths: &["$LOCALAPPDATA\\Programs\\Cursor\\bin\\cursor.cmd"],
};
