//! Shared `thiserror` error enums for the daemon's library modules.
//!
//! Each enum is typed (not erased to `anyhow`) so callers can match on specific
//! failures — `ConfigError`, `WatcherError`, `DbError`, `RuntimeError`, `SkillError`.
//! Module-local error types (e.g. `ApprovalError`, `ProxyError`, `IpcError`) live
//! beside their code and often wrap these via `#[from]`.

use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("reading {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("parsing config.toml: {0}")]
    Parse(#[from] toml::de::Error),
}

#[derive(Debug, Error)]
pub enum ConfigWriteError {
    #[error("invalid value for {key}: {reason}")]
    Invalid { key: String, reason: String },

    #[error("unknown setting key: {0}")]
    UnknownKey(String),

    #[error("config.toml is not valid TOML: {0}")]
    Toml(#[from] toml_edit::TomlError),

    #[error(transparent)]
    Config(#[from] ConfigError),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("config writer actor gone")]
    ActorGone,
}

#[derive(Debug, Error)]
pub enum WatcherError {
    #[error("notify: {0}")]
    Notify(#[from] notify::Error),
}

#[derive(Debug, Error)]
pub enum DbError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("db actor channel closed")]
    ActorGone,

    #[error("spawning db actor thread: {0}")]
    Spawn(#[from] std::io::Error),

    #[error("encoding row value: {0}")]
    Encoding(String),
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("spawning tokio worker thread: {0}")]
    SpawnThread(#[from] std::io::Error),

    #[error("tokio worker thread died before publishing its runtime handle")]
    WorkerDied,
}

#[derive(Debug, Error)]
pub enum SkillError {
    #[error("db: {0}")]
    Db(#[from] DbError),

    #[error("http: {0}")]
    Http(#[from] reqwest::Error),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid skill path segment: {0}")]
    InvalidPath(String),

    #[error("SKILL.md not found in {repo} at {dir}")]
    MissingSkillFile { repo: String, dir: String },

    #[error("github rate-limited (HTTP {0})")]
    RateLimited(u16),

    #[error("github API HTTP {0}")]
    HttpStatus(u16),

    #[error("repo branch resolution failed: {0}")]
    RepoTree(String),

    #[error("skill directory {name:?} is already managed by {owner}/{repo}")]
    DirectoryConflict {
        name: String,
        owner: String,
        repo: String,
    },

    #[error("unknown target ide: {0}")]
    UnknownTarget(String),
}
