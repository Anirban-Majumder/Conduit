//! Client definitions and config writing utilities.
//!
//! Each per-IDE submodule exports a single `static DEF: ClientDef`. The
//! `BUILTINS` slice collects them; `builtin_by_id()` looks up by stable id.
//! After detect→project at startup, all downstream code reads `ResolvedClient`
//! from the DB — no consumer imports an IDE module directly.

use std::path::Path;

use serde_json::Value;

pub mod claude_code;
pub mod codex;
pub mod cursor;
pub mod openclaude;
pub mod opencode;
pub mod vscode_copilot;

// Template for adding a new client. Gated with `#![cfg(any())]` so it never
// compiles — copy this file to `<name>.rs` and wire it into BUILTINS.
pub mod example;

pub mod error;
pub use error::ClientError;

pub mod paths;
pub mod schema;

mod io;
mod merger;

// Re-export the merge helpers that integration tests drive directly.
pub use merger::{purge_uniform_mcp_servers, write_uniform_mcp_servers};

pub use schema::{ConfigFormat, ConfigSchema, ConfigSpec};

/// The full static definition of a built-in client.
///
/// Replaces `ClientSpec` trait + per-IDE `Spec` structs + `IdeKind` enum methods.
/// Each built-in is a single `static DEF: ClientDef` — zero functions, zero trait
/// impls. Paths are declarative via `PathSpec`; detection data is co-located.
pub struct ClientDef {
    pub id: &'static str,
    pub display_name: &'static str,
    pub icon_svg: &'static [u8],
    // Paths (declarative — resolved by paths::resolve).
    pub config: paths::PathSpec,
    /// `None` = no skills support (VS Code Copilot).
    pub skills: Option<paths::PathSpec>,
    // MCP config writing spec.
    pub config_spec: ConfigSpec,
    // Detection.
    pub binaries: &'static [&'static str],
    #[cfg(target_os = "macos")]
    pub mac_app: Option<&'static str>,
    #[cfg(target_os = "windows")]
    pub win_paths: &'static [&'static str],
}

/// All built-in client definitions. Adding an IDE = one new file + one line here.
pub static BUILTINS: &[&ClientDef] = &[
    &claude_code::DEF,
    &codex::DEF,
    &cursor::DEF,
    &openclaude::DEF,
    &opencode::DEF,
    &vscode_copilot::DEF,
];

/// Look up a built-in client definition by its stable id.
pub fn builtin_by_id(id: &str) -> Option<&'static ClientDef> {
    BUILTINS.iter().copied().find(|b| b.id == id)
}

/// Read the raw file contents (UTF-8 string) for the Editors-tab inline JSON
/// viewer. Returns `None` if the file doesn't exist yet.
pub fn read_raw(path: &Path) -> Result<Option<String>, ClientError> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ClientError::Io(e)),
    }
}

/// Atomic write of the inline-editor textarea. Validates JSON first; refuses
/// to clobber the file on parse error.
pub fn write_raw(path: &Path, text: &str) -> Result<(), ClientError> {
    let value: Value = serde_json::from_str(text)?;
    let Value::Object(_) = &value else {
        return Err(ClientError::Json(serde::de::Error::custom(
            "expected top-level JSON object",
        )));
    };
    io::atomic_write_json(path, &value)
}

/// Returns Ok(()) if `text` parses as a valid top-level JSON object. Used by
/// the Editors-tab live-validate (without writing).
pub fn validate_json(text: &str) -> Result<(), ClientError> {
    let value: Value = serde_json::from_str(text)?;
    let Value::Object(_) = &value else {
        return Err(ClientError::Json(serde::de::Error::custom(
            "expected top-level JSON object",
        )));
    };
    Ok(())
}
