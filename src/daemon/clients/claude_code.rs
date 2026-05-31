//! Claude Code — static client definition.

// Repo:   N/A (closed-source CLI; SDK at https://github.com/anthropics/claude-code-sdk-ts)
// MCP:    ~/.claude.json — top-level `mcpServers`; schema `{type:"http", url}`.
// Skills: ~/.claude/skills/<name>/SKILL.md — auto-loaded at startup.

use super::paths::PathSpec;
use super::schema::{ConfigFormat, ConfigSpec};

pub static DEF: super::ClientDef = super::ClientDef {
    id: "ClaudeCode",
    display_name: "Claude Code",
    icon_svg: include_bytes!("../../../assets/client/claude-code.svg"),
    config: PathSpec::Home(".claude.json"),
    skills: Some(PathSpec::Home(".claude/skills")),
    config_spec: ConfigSpec {
        format: ConfigFormat::Json,
        key_path: &["mcpServers"],
        entry_template: r#"{"type":"http","url":"${url}"}"#,
    },
    binaries: &["claude"],
    #[cfg(target_os = "macos")]
    mac_app: Some("Claude.app"),
    #[cfg(target_os = "windows")]
    win_paths: &["$APPDATA\\npm\\claude.cmd"],
};
