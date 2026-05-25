use std::path::{Path, PathBuf};

use thiserror::Error;

const ROOT_DIRNAME: &str = "mcp-router";

#[derive(Debug, Error)]
pub enum PathsError {
    #[error("could not resolve user config directory")]
    NoConfigDir,
}

#[derive(Debug, Clone)]
pub struct Paths {
    root: PathBuf,
}

impl Paths {
    pub fn from_user_dirs() -> Result<Self, PathsError> {
        let root = dirs::config_dir()
            .ok_or(PathsError::NoConfigDir)?
            .join(ROOT_DIRNAME);
        Ok(Self { root })
    }

    pub fn under(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn state_db(&self) -> PathBuf {
        self.root.join("state.db")
    }

    pub fn config_toml(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    pub fn installs_dir(&self) -> PathBuf {
        self.root.join("installs")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    pub fn daemon_log(&self) -> PathBuf {
        self.logs_dir().join("daemon.log")
    }

    pub fn server_log(&self, server_name: &str) -> PathBuf {
        self.logs_dir().join(format!("server-{server_name}.log"))
    }

    pub fn ensure_created(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        std::fs::create_dir_all(self.installs_dir())?;
        std::fs::create_dir_all(self.logs_dir())?;
        Ok(())
    }
}
