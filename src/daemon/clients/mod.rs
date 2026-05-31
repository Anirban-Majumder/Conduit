//! IDE config writers and the shared dispatch layer.
//!
//! Each per-IDE submodule implements `ClientSpec` and is wired into `spec_for`.
//! Public functions (`config_path_for`, `write`, `purge`, etc.) route through
//! that single dispatch point — adding IDE N is one new module + one
//! `spec_for` arm, zero edits to the other functions.
//! Merge/read logic lives in `merger`; atomic I/O lives in `io`.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::daemon::db::types::{IdeKey, ManagedIdeConfig};
pub use crate::daemon::ide_detect::IdeKind;
use crate::daemon::time_util::now_unix_secs;

pub mod claude_code;
pub mod codex;
pub mod cursor;
pub mod openclaude;
pub mod opencode;
pub mod vscode_copilot;

// Template for adding a new client. Gated with `#![cfg(any())]` so it never
// compiles — copy this file to `<name>.rs` and wire it into the dispatchers.
pub mod example;

pub mod error;
pub use error::ClientError;

mod io;
mod merger;

// Re-export the merge helpers that integration tests drive directly.
pub use merger::{purge_uniform_mcp_servers, write_uniform_mcp_servers};

/// Per-IDE schema for the JSON-flavored MCP config (5 of 6 IDEs).
/// Codex is TOML; its `ClientSpec::json_schema` returns `None`.
#[derive(Debug, Clone, Copy)]
pub struct JsonSchema {
    /// Top-level key holding the server map (e.g. `mcpServers`, `servers`, `mcp`).
    pub top_level_key: &'static str,
    /// Required `type` discriminator on each entry, or None if the IDE
    /// auto-negotiates transport from the URL.
    pub server_type: Option<&'static str>,
}

/// Per-IDE metadata and OS-resolved path logic.
///
/// Implement this trait on a zero-sized sentinel type in each IDE module, then
/// wire one `&'static dyn ClientSpec` into `spec_for`. Adding an IDE requires
/// one new module + one `spec_for` arm; all other dispatch functions below are
/// untouched.
pub(crate) trait ClientSpec: Send + Sync {
    fn ide_name(&self) -> &'static str;
    fn icon_svg(&self) -> &'static [u8];
    /// JSON schema; `None` for TOML-format IDEs (Codex).
    fn json_schema(&self) -> Option<JsonSchema>;
    /// OS-resolved config path. Each impl does its own `cfg!` / `dirs::` logic.
    fn config_path(&self) -> Option<PathBuf>;
    /// Test seam: config path relative to `home` (Unix-style layout).
    fn config_path_under(&self, home: &Path) -> PathBuf;
    /// OS-resolved skills directory; `None` if the IDE has no global skills dir.
    fn skills_dir(&self) -> Option<PathBuf>;
    /// Test seam: skills dir relative to `home`; `None` if unsupported.
    fn skills_dir_under(&self, home: &Path) -> Option<PathBuf>;
}

/// Single dispatch point — maps each `IdeKind` to its `ClientSpec` impl.
/// Adding an IDE = one new arm here + one new module; nothing else changes.
fn spec_for(ide: IdeKind) -> &'static dyn ClientSpec {
    match ide {
        IdeKind::ClaudeCode => &claude_code::SPEC,
        IdeKind::Cursor => &cursor::SPEC,
        IdeKind::VsCodeCopilot => &vscode_copilot::SPEC,
        IdeKind::OpenCode => &opencode::SPEC,
        IdeKind::OpenClaude => &openclaude::SPEC,
        IdeKind::Codex => &codex::SPEC,
    }
}

/// IDE targets that natively load `<dir>/skills/*` at startup. VS Code Copilot
/// has no global skills directory (prompt files are workspace-scoped only).
pub const SKILL_TARGET_IDES: &[IdeKind] = &[
    IdeKind::ClaudeCode,
    IdeKind::Cursor,
    IdeKind::OpenClaude,
    IdeKind::OpenCode,
    IdeKind::Codex,
];

/// Resolve the IDE's config file path on the CURRENT operating system.
/// Returns None if we have no path mapping for this (ide, os) combination.
pub fn config_path_for(ide: IdeKind) -> Option<PathBuf> {
    spec_for(ide).config_path()
}

/// Test-only resolver: returns the path each IDE would write under `home`,
/// using the Unix-style layout from the spec. Useful for driving tests with
/// a fake home directory.
pub fn config_path_under(ide: IdeKind, home: &Path) -> PathBuf {
    spec_for(ide).config_path_under(home)
}

pub fn skills_dir_for(ide: IdeKind) -> Option<PathBuf> {
    spec_for(ide).skills_dir()
}

pub fn skills_dir_under(ide: IdeKind, home: &Path) -> Option<PathBuf> {
    spec_for(ide).skills_dir_under(home)
}

pub const fn ide_kind_from_db_name(s: &str) -> Option<IdeKind> {
    match s.as_bytes() {
        b"ClaudeCode" => Some(IdeKind::ClaudeCode),
        b"OpenClaude" => Some(IdeKind::OpenClaude),
        b"OpenCode" => Some(IdeKind::OpenCode),
        b"Cursor" => Some(IdeKind::Cursor),
        b"VsCodeCopilot" => Some(IdeKind::VsCodeCopilot),
        b"Codex" => Some(IdeKind::Codex),
        _ => None,
    }
}

pub fn ide_db_name(ide: IdeKind) -> &'static str {
    spec_for(ide).ide_name()
}

/// Compile-time SVG bytes for the IDE's brand icon.
///
/// Each client module declares its own `ICON_SVG` (via `include_bytes!`), so
/// adding an IDE needs no UI edit: the GUI renders this through
/// `slint::Image::load_from_svg_data`.
pub fn icon_svg(ide: IdeKind) -> &'static [u8] {
    spec_for(ide).icon_svg()
}

/// Inject our entries into the IDE's MCP config.
///
/// Returns a `ManagedIdeConfig` describing what was written. Caller persists
/// via `Db::record_ide_config_write`. Atomic-write: tempfile in same dir +
/// rename. Preserves user's other entries.
pub fn write(
    ide: IdeKind,
    ide_key: &IdeKey,
    server_names: &[String],
    port: u16,
) -> Result<ManagedIdeConfig, ClientError> {
    let path = config_path_for(ide).ok_or(ClientError::NoPathForOs)?;
    let spec = spec_for(ide);
    if let Some(schema) = spec.json_schema() {
        write_uniform_mcp_servers(&path, &schema, &ide_key.key, server_names, port)?;
    } else {
        merger::write_toml_mcp_servers(&path, &ide_key.key, server_names, port)?;
    }
    Ok(ManagedIdeConfig {
        ide_name: spec.ide_name().to_string(),
        ide_key: ide_key.key.clone(),
        config_path: path,
        managed_entries: server_names.to_vec(),
        written_at: now_unix_secs(),
    })
}

/// Remove our entries from the IDE's MCP config.
///
/// Identifies our entries by URL match: `http://127.0.0.1:<port>/<ide_key>/<server>/mcp`.
/// Preserves every other entry the user added. Idempotent (missing file is Ok).
pub fn purge(ide: IdeKind, ide_key: &str, port: u16) -> Result<(), ClientError> {
    let path = config_path_for(ide).ok_or(ClientError::NoPathForOs)?;
    let Some(schema) = spec_for(ide).json_schema() else {
        return merger::purge_toml_mcp_servers(&path, ide_key, port);
    };
    purge_uniform_mcp_servers(&path, &schema, ide_key, port)
}

/// Drop one server's entry from the IDE's MCP config by exact URL match.
///
/// Targets `http://127.0.0.1:<port>/<ide_key>/<server_name>/mcp`. Used by
/// §3.1 cascade delete and the per-server "Remove from this IDE" button.
/// Other entries (user-authored, other servers, other top-level keys) are
/// preserved. `ide` selects the per-IDE schema (top-level key + format).
pub fn purge_one_server_for_ide(
    ide: IdeKind,
    path: &Path,
    port: u16,
    ide_key: &str,
    server_name: &str,
) -> Result<(), ClientError> {
    let Some(schema) = spec_for(ide).json_schema() else {
        return merger::purge_one_server_for_ide_toml(path, port, ide_key, server_name);
    };
    merger::purge_one_server_for_ide_json(path, &schema, port, ide_key, server_name)
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
