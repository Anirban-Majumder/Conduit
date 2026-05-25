// One-shot service install at first run. Spec §"Autostart at login".
// Linux systemd user unit / macOS LaunchAgent plist / Windows Task Scheduler.

use std::path::PathBuf;

use thiserror::Error;

#[cfg(target_os = "linux")]
pub mod systemd_linux;

#[cfg(target_os = "macos")]
pub mod launchd_macos;

#[cfg(target_os = "windows")]
pub mod scheduled_task_windows;

#[derive(Debug, Error)]
pub enum InstallError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not resolve user home directory")]
    NoHomeDir,
    #[error("unsupported operating system")]
    UnsupportedOs,
    #[error("command failed: {stderr}")]
    CommandFailed { stderr: String },
}

/// Install the autostart service for the current OS. Idempotent: re-installing
/// over an existing entry just overwrites. Returns the path of the written
/// service file (for logging / display).
#[cfg(target_os = "linux")]
pub fn install() -> Result<PathBuf, InstallError> {
    let exe = std::env::current_exe()?;
    systemd_linux::install(&exe)
}

#[cfg(target_os = "macos")]
pub fn install() -> Result<PathBuf, InstallError> {
    let exe = std::env::current_exe()?;
    launchd_macos::install(&exe)
}

#[cfg(target_os = "windows")]
pub fn install() -> Result<PathBuf, InstallError> {
    let exe = std::env::current_exe()?;
    scheduled_task_windows::install(&exe)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub fn install() -> Result<PathBuf, InstallError> {
    Err(InstallError::UnsupportedOs)
}

/// Remove the autostart service. Idempotent: missing service file is Ok.
#[cfg(target_os = "linux")]
pub fn uninstall() -> Result<(), InstallError> {
    systemd_linux::uninstall()
}

#[cfg(target_os = "macos")]
pub fn uninstall() -> Result<(), InstallError> {
    launchd_macos::uninstall()
}

#[cfg(target_os = "windows")]
pub fn uninstall() -> Result<(), InstallError> {
    scheduled_task_windows::uninstall()
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub fn uninstall() -> Result<(), InstallError> {
    Err(InstallError::UnsupportedOs)
}

/// Whether autostart is currently installed (service file present).
#[cfg(target_os = "linux")]
pub fn is_installed() -> bool {
    systemd_linux::is_installed()
}

#[cfg(target_os = "macos")]
pub fn is_installed() -> bool {
    launchd_macos::is_installed()
}

#[cfg(target_os = "windows")]
pub fn is_installed() -> bool {
    scheduled_task_windows::is_installed()
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub fn is_installed() -> bool {
    false
}
