//! `OpenClaude` — static client definition.

// Repo:   https://github.com/Gitlawb/openclaude
// MCP:    ~/.openclaude.json — top-level `mcpServers`; schema `{type:"http", url}`.
// Skills: ~/.openclaude/skills/<name>/SKILL.md.

use crate::daemon::client_config::paths::PathSpec;
use crate::daemon::client_config::types::{ConfigFormat, ConfigSpec};
use crate::daemon::client_config::ClientDef;

pub static DEF: ClientDef = ClientDef {
    id: "OpenClaude",
    display_name: "OpenClaude",
    icon_svg: include_bytes!("../../assets/client/openclaude.svg"),
    config: PathSpec::Home(".openclaude.json"),
    skills: Some(PathSpec::Home(".openclaude/skills")),
    project_config_rel: Some(".mcp.json"),
    project_skills_rel: Some(".openclaude/skills"),
    config_spec: ConfigSpec {
        format: ConfigFormat::Json,
        key_path: &["mcpServers"],
        entry_template: r#"{"type":"http","url":"${url}"}"#,
    },
    binaries: &["openclaude"],
    #[cfg(target_os = "macos")]
    mac_app: None,
    #[cfg(target_os = "windows")]
    win_paths: &[],
};
