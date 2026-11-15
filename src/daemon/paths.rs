//! Resolves all on-disk locations under the Conduit config root.
//!
//! `from_user_dirs` anchors at the OS config dir (`dirs::config_dir`) so paths
//! stay correct across Linux/macOS/Windows; `under` is for tests. Every other
//! method derives a child path (state.db, config.toml, installs, skills, logs).

use std::path::{Path, PathBuf};

use thiserror::Error;

const ROOT_DIRNAME: &str = "conduit";

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

    pub fn skills_root(&self) -> PathBuf {
        self.root.join("skills")
    }

    pub fn skills_managed_dir(&self) -> PathBuf {
        self.skills_root().join("managed")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    pub fn ensure_created(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        std::fs::create_dir_all(self.installs_dir())?;
        std::fs::create_dir_all(self.logs_dir())?;
        std::fs::create_dir_all(self.skills_managed_dir())?;
        Ok(())
    }
}
