use std::path::{Path, PathBuf};

pub(crate) const IDE_NAME: &str = "Cursor";

pub(crate) fn config_path_under(home: &Path) -> PathBuf {
    home.join(".cursor").join("mcp.json")
}

pub(crate) fn config_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let appdata = dirs::config_dir()?;
        return Some(appdata.join("Cursor").join("mcp.json"));
    }
    #[cfg(not(target_os = "windows"))]
    {
        let home = dirs::home_dir()?;
        Some(config_path_under(&home))
    }
}
