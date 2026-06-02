//! `OpenCode` — static client definition (XDG on Linux+macOS, not `~/Library`).

// Repo:   https://github.com/sst/opencode
// MCP:    ~/.config/opencode/opencode.json — top-level `mcp`; schema `{type:"remote", url}`.
// Skills: ~/.config/opencode/skills/<name>/SKILL.md.

use crate::daemon::client_config::paths::PathSpec;
use crate::daemon::client_config::types::{ConfigFormat, ConfigSpec};
use crate::daemon::client_config::ClientDef;

pub static DEF: ClientDef = ClientDef {
    id: "OpenCode",
    display_name: "OpenCode",
    icon_svg: include_bytes!("../../assets/client/opencode.svg"),
    config: PathSpec::PlatformSplit {
        unix: ".config/opencode/opencode.json",
        windows: "opencode/opencode.json",
    },
    skills: Some(PathSpec::PlatformSplit {
        unix: ".config/opencode/skills",
        windows: "opencode/skills",
    }),
    project_config_rel: Some(".opencode/opencode.json"),
    project_skills_rel: Some(".opencode/skills"),
    config_spec: ConfigSpec {
        format: ConfigFormat::Json,
        key_path: &["mcp"],
        entry_template: r#"{"type":"remote","url":"${url}"}"#,
    },
    binaries: &["opencode"],
    #[cfg(target_os = "macos")]
    mac_app: Some("OpenCode.app"),
    #[cfg(target_os = "windows")]
    win_paths: &["$LOCALAPPDATA\\Programs\\OpenCode\\bin\\opencode.cmd"],
};
