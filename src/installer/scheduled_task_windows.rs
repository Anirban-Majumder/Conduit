//! Windows autostart via a Task Scheduler logon task with restart-on-failure.
//! Spec §"Autostart at login".
//!
//! Renders a task XML and registers it through `schtasks /Create /XML`.
//! `Install/uninstall/is_installed` shell out to `schtasks` and are idempotent —
//! a missing task on delete/query is treated as success.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::InstallError;

const TASK_NAME: &str = "MCP Router";
const XML_FILE_NAME: &str = "autostart.xml";

pub fn install(exe: &Path) -> Result<PathBuf, InstallError> {
    let dir = task_xml_dir()?;
    install_into(&dir, exe)
}

pub fn uninstall() -> Result<(), InstallError> {
    // Query first: exit 0 means the task exists; non-zero means absent.
    // This avoids matching localised error text from /Delete.
    let present = Command::new("schtasks")
        .args(["/Query", "/TN", TASK_NAME])
        .output()
        .is_ok_and(|o| o.status.success());

    if present {
        let out = Command::new("schtasks")
            .args(["/Delete", "/TN", TASK_NAME, "/F"])
            .output()?;
        if !out.status.success() {
            return Err(InstallError::CommandFailed {
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
    }

    // Best-effort: remove the rendered XML regardless of locale / task presence.
    if let Ok(dir) = task_xml_dir() {
        let _ = std::fs::remove_file(dir.join(XML_FILE_NAME));
    }

    Ok(())
}

pub fn is_installed() -> bool {
    Command::new("schtasks")
        .args(["/Query", "/TN", TASK_NAME])
        .output()
        .is_ok_and(|o| o.status.success())
}

pub fn install_into(dir: &Path, exe: &Path) -> Result<PathBuf, InstallError> {
    std::fs::create_dir_all(dir)?;
    let xml_path = dir.join(XML_FILE_NAME);
    let body = render_task_xml(exe);
    super::atomic_write(&xml_path, XML_FILE_NAME, body.as_bytes())?;

    let output = Command::new("schtasks")
        .args(["/Create", "/TN", TASK_NAME, "/XML"])
        .arg(&xml_path)
        .arg("/F")
        .output()?;
    if !output.status.success() {
        return Err(InstallError::CommandFailed {
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(xml_path)
}

pub fn render_task_xml(exe: &Path) -> String {
    let exec = super::xml_escape(&exe.display().to_string());
    let user = current_user_display();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <Task version=\"1.4\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n\
         \x20 <Triggers>\n\
         \x20   <LogonTrigger>\n\
         \x20     <Enabled>true</Enabled>\n\
         \x20     <UserId>{user}</UserId>\n\
         \x20   </LogonTrigger>\n\
         \x20 </Triggers>\n\
         \x20 <Principals>\n\
         \x20   <Principal id=\"Author\">\n\
         \x20     <UserId>{user}</UserId>\n\
         \x20     <LogonType>InteractiveToken</LogonType>\n\
         \x20     <RunLevel>LeastPrivilege</RunLevel>\n\
         \x20   </Principal>\n\
         \x20 </Principals>\n\
         \x20 <Settings>\n\
         \x20   <RestartOnFailure>\n\
         \x20     <Interval>PT1M</Interval>\n\
         \x20     <Count>5</Count>\n\
         \x20   </RestartOnFailure>\n\
         \x20   <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>\n\
         \x20   <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>\n\
         \x20   <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>\n\
         \x20   <Enabled>true</Enabled>\n\
         \x20 </Settings>\n\
         \x20 <Actions Context=\"Author\">\n\
         \x20   <Exec>\n\
         \x20     <Command>{exec}</Command>\n\
         \x20   </Exec>\n\
         \x20 </Actions>\n\
         </Task>\n",
    )
}

fn task_xml_dir() -> Result<PathBuf, InstallError> {
    let appdata = dirs::config_dir().ok_or(InstallError::NoHomeDir)?;
    Ok(appdata.join("conduit"))
}

fn current_user_display() -> String {
    let username = std::env::var("USERNAME").unwrap_or_default();
    let domain = std::env::var("USERDOMAIN").unwrap_or_default();
    if domain.is_empty() {
        super::xml_escape(&username)
    } else {
        super::xml_escape(&format!("{domain}\\{username}"))
    }
}
