use std::path::{Path, PathBuf};

pub(crate) const IDE_NAME: &str = "ClaudeCode";

pub(crate) fn config_path_under(home: &Path) -> PathBuf {
    home.join(".claude").join("claude_desktop_config.json")
}

pub(crate) fn config_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let appdata = dirs::config_dir()?;
        return Some(appdata.join("Claude").join("claude_desktop_config.json"));
    }
    #[cfg(not(target_os = "windows"))]
    {
        let home = dirs::home_dir()?;
        Some(config_path_under(&home))
    }
}

pub(crate) fn skills_dir_under(home: &Path) -> PathBuf {
    home.join(".claude").join("skills")
}

pub(crate) fn skills_dir() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(skills_dir_under(&home))
}
