//! Linux autostart via a systemd user unit at
//! `~/.config/systemd/user/conduit.service`. Spec §"Autostart at login".
//!
//! Writes the unit atomically (`Restart=on-failure`). Install/uninstall/
//! `is_installed` are idempotent. We deliberately don't run `systemctl` — headless,
//! WSL2, and non-systemd setups make invoking it from the binary unreliable.

use std::fs;
use std::path::{Path, PathBuf};

use super::InstallError;

const SERVICE_FILE_NAME: &str = "conduit.service";

pub fn install(exe: &Path) -> Result<PathBuf, InstallError> {
    let dir = service_dir()?;
    install_into(&dir, exe)
}

pub fn uninstall() -> Result<(), InstallError> {
    let dir = service_dir()?;
    uninstall_from(&dir)
}

pub fn is_installed() -> bool {
    service_dir()
        .map(|d| d.join(SERVICE_FILE_NAME).is_file())
        .unwrap_or(false)
}

// Post-install the user runs once:
//   systemctl --user daemon-reload && systemctl --user enable --now conduit.service
// We deliberately don't shell out — headless / WSL2 / non-systemd setups make
// invoking systemctl from inside the binary unreliable.
pub fn install_into(dir: &Path, exe: &Path) -> Result<PathBuf, InstallError> {
    fs::create_dir_all(dir)?;
    let path = dir.join(SERVICE_FILE_NAME);
    let body = render_service_unit(exe);
    super::atomic_write(&path, SERVICE_FILE_NAME, body.as_bytes())?;
    Ok(path)
}

pub fn uninstall_from(dir: &Path) -> Result<(), InstallError> {
    let path = dir.join(SERVICE_FILE_NAME);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(InstallError::Io(e)),
    }
}

pub fn render_service_unit(exe: &Path) -> String {
    format!(
        "[Unit]\n\
         Description=MCP Router \u{2014} MCP proxy + policy enforcer\n\
         After=network.target graphical-session.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={exec}\n\
         Restart=on-failure\n\
         RestartSec=2\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        exec = exe.display(),
    )
}

fn service_dir() -> Result<PathBuf, InstallError> {
    let home = dirs::home_dir().ok_or(InstallError::NoHomeDir)?;
    Ok(home.join(".config").join("systemd").join("user"))
}
