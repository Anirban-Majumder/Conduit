use std::path::{Path, PathBuf};

pub(crate) const IDE_NAME: &str = "OpenClaude";

pub(crate) fn config_path_under(home: &Path) -> PathBuf {
    home.join(".openclaude.json")
}

pub(crate) fn config_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(config_path_under(&home))
}

pub(crate) fn skills_dir_under(home: &Path) -> PathBuf {
    home.join(".openclaude").join("skills")
}

pub(crate) fn skills_dir() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(skills_dir_under(&home))
}
