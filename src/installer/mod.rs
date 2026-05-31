//! One-shot autostart-service install at first run.
//!
//! Per spec §"Autostart at login". Dispatches `install` / `uninstall` /
//! `is_installed` to the per-OS backend: a systemd user unit on Linux, a
//! `LaunchAgent` plist on macOS, a Task Scheduler entry on Windows. All
//! operations are idempotent; unsupported OSes return
//! `InstallError::UnsupportedOs`.

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::fs;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::path::Path;
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

/// Escape XML special characters for use in plist/Task-Scheduler XML bodies.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub(super) fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

/// Write `bytes` to `path` atomically via a temp-file rename.
///
/// `fallback_name` is used as the temp-file stem when `path` has no
/// filename component (practically unreachable but avoids an unwrap).
// reason: Windows installer (scheduled_task_windows.rs) writes a one-shot
// autostart.xml that schtasks reads immediately — a partial write just fails
// the install() call, not ongoing runtime. Not worth the ReplaceFileW wiring;
// see daemon/clients/io.rs for the runtime-critical Windows atomic write.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn atomic_write(
    path: &Path,
    fallback_name: &str,
    bytes: &[u8],
) -> Result<(), InstallError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(fallback_name);
    let tmp = parent.join(format!(".{file_name}.{}.tmp", std::process::id()));
    fs::write(&tmp, bytes)?;
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ignored = fs::remove_file(&tmp);
            Err(InstallError::Io(e))
        }
    }
}
