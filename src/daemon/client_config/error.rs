//! Error type for the IDE config writers.
//!
//! `ClientError` covers the failure modes of reading/parsing/writing an IDE's
//! MCP config (JSON or TOML), missing per-OS path mappings, and non-UTF-8
//! paths. Returned by every public function in this `clients` module.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("parsing existing IDE config as JSON: {0}")]
    Json(#[from] serde_json::Error),

    #[error("parsing existing IDE config as TOML: {0}")]
    TomlDe(#[from] toml::de::Error),

    #[error("serializing IDE config as TOML: {0}")]
    TomlSer(#[from] toml::ser::Error),

    #[error("no config path mapping for this (ide, os) combination")]
    NoPathForOs,

    #[error("config path is not valid UTF-8")]
    NonUtf8Path,

    #[error("config schema has an empty key_path — refusing to write at the file root")]
    EmptyKeyPath,

    /// JSON parse failure with the file path attached for log context.
    #[error("parsing IDE config at {path}: {source}")]
    ParseJson {
        path: std::path::PathBuf,
        source: serde_json::Error,
    },

    /// TOML parse failure with the file path attached for log context.
    #[error("parsing IDE config TOML at {path}: {source}")]
    ParseToml {
        path: std::path::PathBuf,
        source: toml::de::Error,
    },
}
