//! Copy-this template for adding a new IDE/agent client (never compiled).
//!
//! Repo:   <upstream repo URL, or N/A if closed-source>
//! MCP:    <config file path>; top-level `<key>`; schema `<one-line>`. Source: <docs/repo URL>
//! Skills: <skills dir path, or "not supported">. Source: <docs/repo URL, or N/A>
//!
//! Copy this file to `<name>.rs`, fill in the three header lines from
//! authoritative sources (never invent URLs), then wire it into `mod.rs`:
//!
//!   1. Add a variant to `IdeKind` in `crate::daemon::ide_detect`.
//!   2. `pub mod <name>;` in this directory's `mod.rs`.
//!   3. Map the variant in `json_schema_for` (top-level key + optional `type`)
//!      OR add a TOML branch like Codex.
//!   4. Map the variant in `config_path_for` / `config_path_under` /
//!      `ide_kind_from_db_name` / `ide_name`.
//!   5. If skills are auto-loaded, map it in `skills_dir_for` /
//!      `skills_dir_under` and add to `SKILL_TARGET_IDES`.
//!
//! Drive every path through `dirs::home_dir()` / `dirs::config_dir()` — never
//! hardcode `~/.config`, `~/Library`, or `%APPDATA%`. The `_under(home)`
//! variant exists so tests can drive a fake home directory.

#![cfg(any())] // template only; never compiled

use std::path::{Path, PathBuf};

pub(crate) const IDE_NAME: &str = "ExampleClient";

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
