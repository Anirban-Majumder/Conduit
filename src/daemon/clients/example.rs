//! Copy-this template for adding a new IDE/agent client (never compiled).
//!
//! Repo:   <upstream repo URL, or N/A if closed-source>
//! MCP:    <config file path>; top-level `<key>`; schema `<one-line>`. Source: <docs/repo URL>
//! Skills: <skills dir path, or "not supported">. Source: <docs/repo URL, or N/A>
//!
//! Steps to wire in a new IDE:
//!
//!   1. Add a variant to `IdeKind` in `crate::daemon::ide_detect`.
//!   2. `pub mod <name>;` in `clients/mod.rs`.
//!   3. Copy this file to `<name>.rs`; implement `ClientSpec` on `Spec`
//!      (fill in `json_schema`, `config_path`, `skills_dir`, etc.).
//!   4. Drop a brand SVG at `assets/client/<name>.svg`; point `ICON_SVG` below
//!      at it. The GUI auto-renders it via `slint::Image::load_from_svg_data`.
//!   5. Add one arm to `spec_for` in `clients/mod.rs`:
//!        `IdeKind::ExampleClient => &example::SPEC,`
//!   6. If skills are auto-loaded, add the variant to `SKILL_TARGET_IDES`.
//!
//! Drive every path through `dirs::home_dir()` / `dirs::config_dir()` — never
//! hardcode `~/.config`, `~/Library`, or `%APPDATA%`. The `config_path_under`
//! variant exists so tests can drive a fake home directory.

#![cfg(any())] // template only; never compiled

use std::path::{Path, PathBuf};

pub(crate) const IDE_NAME: &str = "ExampleClient";
pub(crate) const ICON_SVG: &[u8] = include_bytes!("../../../assets/client/example.svg");

pub(crate) fn config_path_under(home: &Path) -> PathBuf {
    home.join(".example").join("config.json")
}

pub(crate) fn config_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(config_path_under(&home))
}

// Delete these two if the client has no skills directory.
pub(crate) fn skills_dir_under(home: &Path) -> PathBuf {
    home.join(".example").join("skills")
}

pub(crate) fn skills_dir() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(skills_dir_under(&home))
}

pub(super) static SPEC: Spec = Spec;

pub(super) struct Spec;

impl super::ClientSpec for Spec {
    fn ide_name(&self) -> &'static str {
        IDE_NAME
    }
    fn icon_svg(&self) -> &'static [u8] {
        ICON_SVG
    }
    fn json_schema(&self) -> Option<super::JsonSchema> {
        Some(super::JsonSchema {
            top_level_key: "mcpServers",
            server_type: Some("http"),
        })
    }
    fn config_path(&self) -> Option<PathBuf> {
        config_path()
    }
    fn config_path_under(&self, home: &Path) -> PathBuf {
        config_path_under(home)
    }
    fn skills_dir(&self) -> Option<PathBuf> {
        skills_dir()
    }
    // Return None here (and delete skills_dir_under above) if unsupported.
    fn skills_dir_under(&self, home: &Path) -> Option<PathBuf> {
        Some(skills_dir_under(home))
    }
}
