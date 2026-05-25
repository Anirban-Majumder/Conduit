use std::path::{Path, PathBuf};

pub(crate) const IDE_NAME: &str = "OpenClaude";

pub(crate) fn config_path_under(home: &Path) -> PathBuf {
    home.join(".openclaude.json")
}

pub(crate) fn config_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(config_path_under(&home))
}
