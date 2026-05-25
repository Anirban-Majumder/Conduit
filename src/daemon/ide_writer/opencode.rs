// TODO: refine per-IDE shape — OpenCode may use a non-standard key; ship the
// uniform `mcpServers` shape for v0 and revisit during beta.

use std::path::{Path, PathBuf};

pub(crate) const IDE_NAME: &str = "OpenCode";

pub(crate) fn config_path_under(home: &Path) -> PathBuf {
    home.join(".config").join("opencode").join("opencode.json")
}

pub(crate) fn config_path() -> Option<PathBuf> {
    // Spec pins OpenCode to ~/.config on Linux AND macOS (not Library on mac).
    #[cfg(target_os = "windows")]
    {
        let appdata = dirs::config_dir()?;
        return Some(appdata.join("opencode").join("opencode.json"));
    }
    #[cfg(not(target_os = "windows"))]
    {
        let home = dirs::home_dir()?;
        Some(config_path_under(&home))
    }
}
