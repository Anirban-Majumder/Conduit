//! Codex — static client definition (TOML config; honors `$CODEX_HOME`).

// Repo:   https://github.com/openai/codex
// MCP:    $CODEX_HOME/config.toml (default ~/.codex/config.toml) — `[mcp_servers.<name>]`.
// Skills: $CODEX_HOME/skills (default ~/.codex/skills).

use crate::daemon::client_config::paths::PathSpec;
use crate::daemon::client_config::types::{ConfigFormat, ConfigSpec};
use crate::daemon::client_config::ClientDef;

pub static DEF: ClientDef = ClientDef {
    id: "Codex",
    display_name: "Codex",
    icon_svg: include_bytes!("../../assets/client/codex.svg"),
    config: PathSpec::EnvOrHome {
        env: "CODEX_HOME",
        suffix: "config.toml",
        fallback: ".codex/config.toml",
    },
    skills: Some(PathSpec::EnvOrHome {
        env: "CODEX_HOME",
        suffix: "skills",
        fallback: ".codex/skills",
    }),
    project_config_rel: Some(".codex/config.toml"),
    project_skills_rel: Some(".codex/skills"),
    config_spec: ConfigSpec {
        format: ConfigFormat::Toml,
        key_path: &["mcp_servers"],
        entry_template: r#"{"url":"${url}"}"#,
    },
    binaries: &["codex"],
    #[cfg(target_os = "macos")]
    mac_app: None,
    #[cfg(target_os = "windows")]
    win_paths: &["$APPDATA\\npm\\codex.cmd"],
};
