//! OpenClaude — static client definition.

// Repo:   https://github.com/Gitlawb/openclaude
// MCP:    ~/.openclaude.json — top-level `mcpServers`; schema `{type:"http", url}`.
// Skills: ~/.openclaude/skills/<name>/SKILL.md.

use super::paths::PathSpec;
use super::schema::{ConfigFormat, ConfigSpec};

pub static DEF: super::ClientDef = super::ClientDef {
    id: "OpenClaude",
    display_name: "OpenClaude",
    icon_svg: include_bytes!("../../../assets/client/openclaude.svg"),
    config: PathSpec::Home(".openclaude.json"),
    skills: Some(PathSpec::Home(".openclaude/skills")),
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
