#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test ergonomics"
)]

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn fresh_tmp(label: &str) -> std::path::PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("conduit-installer-{label}-{ns}"))
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use conduit::installer::systemd_linux::{install_into, render_service_unit, uninstall_from};
    use std::path::Path;

    #[test]
    fn render_service_unit_includes_exec_path() {
        let unit = render_service_unit(Path::new("/opt/conduit/bin/conduit"));
        assert!(unit.contains("ExecStart=\"/opt/conduit/bin/conduit\""));
        assert!(unit.contains("[Unit]"));
        assert!(unit.contains("[Service]"));
        assert!(unit.contains("[Install]"));
        assert!(unit.contains("Restart=on-failure"));
    }

    #[test]
    fn install_into_writes_service_file() {
        let dir = fresh_tmp("linux-install");
        let exe = Path::new("/usr/bin/conduit");
        let path = install_into(&dir, exe).expect("install");
        assert!(path.is_file());
        assert!(path.ends_with("conduit.service"));
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("ExecStart=\"/usr/bin/conduit\""));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn uninstall_from_missing_is_ok() {
        let dir = fresh_tmp("linux-uninstall-missing");
        uninstall_from(&dir).expect("idempotent");
    }

    #[test]
    fn install_then_uninstall_round_trip() {
        let dir = fresh_tmp("linux-rt");
        let exe = Path::new("/usr/bin/conduit");
        let path = install_into(&dir, exe).unwrap();
        assert!(path.is_file());
        uninstall_from(&dir).unwrap();
        assert!(!path.exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use conduit::installer::launchd_macos::{install_into, render_plist, uninstall_from};
    use std::path::Path;

    #[test]
    fn render_plist_includes_exec_and_keepalive() {
        let plist = render_plist(Path::new(
            "/Applications/conduit.app/Contents/MacOS/conduit",
        ));
        assert!(plist.contains("/Applications/conduit.app/Contents/MacOS/conduit"));
        assert!(plist.contains("<key>RunAtLoad</key><true/>"));
        assert!(plist.contains("<key>KeepAlive</key>"));
        assert!(plist.contains("<key>SuccessfulExit</key><false/>"));
        assert!(plist.contains("<key>Label</key><string>com.mcprouter</string>"));
    }

    #[test]
    fn install_into_writes_plist() {
        let dir = fresh_tmp("macos-install");
        let exe = Path::new("/usr/local/bin/conduit");
        let path = install_into(&dir, exe).expect("install");
        assert!(path.is_file());
        assert!(path.ends_with("com.mcprouter.plist"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn install_then_uninstall_round_trip() {
        let dir = fresh_tmp("macos-rt");
        let exe = Path::new("/usr/local/bin/conduit");
        let path = install_into(&dir, exe).unwrap();
        uninstall_from(&dir).unwrap();
        assert!(!path.exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use conduit::installer::scheduled_task_windows::render_task_xml;
    use std::path::Path;

    #[test]
    fn render_task_xml_includes_command_and_logon_trigger() {
        let xml = render_task_xml(Path::new(r"C:\Program Files\conduit\conduit.exe"));
        assert!(xml.contains("LogonTrigger"));
        assert!(xml.contains("RestartOnFailure"));
        assert!(xml.contains(r"C:\Program Files\conduit\conduit.exe"));
    }
}

#[test]
fn xml_escape_smoke() {
    // No public XML escape exists; this is just a smoke test of the public is_installed.
    // is_installed must not panic regardless of state.
    let _ = conduit::installer::is_installed();
}
