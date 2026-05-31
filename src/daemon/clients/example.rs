//! Copy-this template for adding a new IDE/agent client (never compiled).
//!
//! Steps to wire in a new client:
//!
//!   1. Copy this file to `<name>.rs`; fill in the `ClientDef` fields.
//!   2. `pub mod <name>;` in `clients/mod.rs`.
//!   3. Add `&<name>::DEF` to the `BUILTINS` slice in `clients/mod.rs`.
//!   4. Drop a brand SVG at `assets/client/<name>.svg`; point `icon_svg`
//!      at it via `include_bytes!`. The GUI auto-renders it.

#![cfg(any())] // template only; never compiled

use super::paths::PathSpec;
use super::schema::{ConfigFormat, ConfigSpec};

pub static DEF: super::ClientDef = super::ClientDef {
    id: "ExampleClient",
    display_name: "Example Client",
    icon_svg: include_bytes!("../../../assets/client/example.svg"),
    config: PathSpec::Home(".example/config.json"),
    skills: Some(PathSpec::Home(".example/skills")),
    config_spec: ConfigSpec {
        format: ConfigFormat::Json,
        key_path: &["mcpServers"],
        entry_template: r#"{"type":"http","url":"${url}"}"#,
    },
    binaries: &["example"],
    #[cfg(target_os = "macos")]
    mac_app: Some("Example.app"),
    #[cfg(target_os = "windows")]
    win_paths: &["$APPDATA\\npm\\example.cmd"],
};
