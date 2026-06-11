//! Claude Desktop — static client definition.

// App:    https://claude.ai/download
// MCP:    claude_desktop_config.json — top-level `mcpServers`; STDIO entries via mcp-remote shim.
// Config: macOS  ~/Library/Application Support/Claude/claude_desktop_config.json
//         Windows %APPDATA%\Claude\claude_desktop_config.json
//         Linux   ~/.config/Claude/claude_desktop_config.json (unofficial)

use crate::daemon::client_config::paths::PathSpec;
use crate::daemon::client_config::types::{ConfigFormat, ConfigSpec};
use crate::daemon::client_config::ClientDef;

pub static DEF: ClientDef = ClientDef {
    id: "ClaudeDesktop",
    display_name: "Claude Desktop",
    icon_svg: include_bytes!("../../assets/client/claude-desktop.svg"),
    // ConfigDir resolves to the OS app-config directory on all three platforms:
    //   macOS   → ~/Library/Application Support/Claude/claude_desktop_config.json
    //   Windows → %APPDATA%\Claude\claude_desktop_config.json
    //   Linux   → ~/.config/Claude/claude_desktop_config.json
    config: PathSpec::ConfigDir("Claude/claude_desktop_config.json"),
    skills: None,
    project_config_rel: None,
    project_skills_rel: None,
    config_spec: ConfigSpec {
        format: ConfigFormat::Json,
        key_path: &["mcpServers"],
        // Claude Desktop accepts STDIO entries. mcp-remote bridges HTTP→STDIO.
        // Version-pinned so npx resolution is stable; this is Claude Desktop's
        // own spawn, not Conduit's (the no-npx rule covers Conduit upstreams).
        entry_template: r#"{"command":"npx","args":["-y","mcp-remote@0.1","${url}"]}"#,
    },
    binaries: &[],
    #[cfg(target_os = "macos")]
    mac_app: Some("Claude.app"),
    #[cfg(target_os = "windows")]
    win_paths: &[],
};
