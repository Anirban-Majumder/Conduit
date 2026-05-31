//! Cursor — static client definition.

// Repo:   N/A (closed-source; vendor: Anysphere)
// MCP:    ~/.cursor/mcp.json — top-level `mcpServers`; remote servers use `{url}` only.
// Skills: ~/.cursor/skills/ (auto-discovered SKILL.md folders).

use super::paths::PathSpec;
use super::schema::{ConfigFormat, ConfigSpec};

pub static DEF: super::ClientDef = super::ClientDef {
    id: "Cursor",
    display_name: "Cursor",
    icon_svg: include_bytes!("../../../assets/client/cursor.svg"),
    config: PathSpec::PlatformSplit {
        unix: ".cursor/mcp.json",
        windows: "Cursor/mcp.json",
    },
    skills: Some(PathSpec::Home(".cursor/skills")),
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
