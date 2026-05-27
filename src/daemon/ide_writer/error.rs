use thiserror::Error;

#[derive(Debug, Error)]
pub enum IdeWriterError {
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
}
