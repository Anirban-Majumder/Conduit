//! macOS autostart via a LaunchAgent at `~/Library/LaunchAgents/com.mcprouter.plist`.
//! Spec §"Autostart at login".
//!
//! Writes the plist atomically (`RunAtLoad=true`, `KeepAlive={SuccessfulExit:false}`).
//! Install/uninstall/is_installed are idempotent. We deliberately don't shell out
//! to `launchctl`; the user loads it once or the OS picks it up at next login.

use std::fs;
use std::path::{Path, PathBuf};

use super::InstallError;

const PLIST_FILE_NAME: &str = "com.mcprouter.plist";

pub fn install(exe: &Path) -> Result<PathBuf, InstallError> {
    let dir = agent_dir()?;
    install_into(&dir, exe)
}

pub fn uninstall() -> Result<(), InstallError> {
    let dir = agent_dir()?;
    uninstall_from(&dir)
}

pub fn is_installed() -> bool {
    agent_dir()
        .map(|d| d.join(PLIST_FILE_NAME).is_file())
        .unwrap_or(false)
}

// Post-install the user runs once:
//   launchctl load ~/Library/LaunchAgents/com.mcprouter.plist
// or the OS auto-loads at next login.
pub fn install_into(dir: &Path, exe: &Path) -> Result<PathBuf, InstallError> {
    fs::create_dir_all(dir)?;
    let path = dir.join(PLIST_FILE_NAME);
    let body = render_plist(exe);
    atomic_write(&path, body.as_bytes())?;
    Ok(path)
}

pub fn uninstall_from(dir: &Path) -> Result<(), InstallError> {
    let path = dir.join(PLIST_FILE_NAME);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(InstallError::Io(e)),
    }
}

// KeepAlive={SuccessfulExit: false} = respawn on crash (exit != 0), stay dead
// on graceful Quit (exit 0). Tray > Quit is truly final until next login.
pub fn render_plist(exe: &Path) -> String {
    let exec = xml_escape(&exe.display().to_string());
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \x20 <key>Label</key><string>com.mcprouter</string>\n\
         \x20 <key>ProgramArguments</key>\n\
         \x20 <array>\n\
         \x20   <string>{exec}</string>\n\
         \x20 </array>\n\
         \x20 <key>RunAtLoad</key><true/>\n\
         \x20 <key>KeepAlive</key>\n\
         \x20 <dict>\n\
         \x20   <key>SuccessfulExit</key><false/>\n\
         \x20 </dict>\n\
         \x20 <key>ProcessType</key><string>Interactive</string>\n\
         </dict>\n\
         </plist>\n",
    )
}

fn agent_dir() -> Result<PathBuf, InstallError> {
    let home = dirs::home_dir().ok_or(InstallError::NoHomeDir)?;
    Ok(home.join("Library").join("LaunchAgents"))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), InstallError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(PLIST_FILE_NAME);
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

fn xml_escape(s: &str) -> String {
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
