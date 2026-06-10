//! Small shared helpers for the glue layer.
//!
//! Time formatting, IDE config-path resolution and detection, OS notification
//! firing, file-manager reveal, and settings→Slint-row conversion — used across
//! the callback and refresh modules.

use slint::SharedString;

use conduit::daemon::client_config;
use conduit::daemon::config::toml::Settings;
use conduit::daemon::db::Db;

use crate::window::view::{MainWindow, SettingsRow};

pub(crate) fn relative_time(delta_secs: i64) -> String {
    if delta_secs < 0 {
        return "just now".to_string();
    }
    if delta_secs < 60 {
        return format!("{delta_secs}s ago");
    }
    let mins = delta_secs / 60;
    if mins < 60 {
        return format!("{mins}m ago");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    format!("{}d ago", hours / 24)
}

/// Local full timestamp `YYYY-MM-DD HH:MM:SS` for the detail pane; empty on overflow.
pub(crate) fn clock_full(ts: i64) -> String {
    use chrono::{Local, TimeZone};
    Local
        .timestamp_opt(ts, 0)
        .single()
        .map_or_else(String::new, |dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
}

pub(crate) fn unix_now() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let s = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    i64::try_from(s).unwrap_or(i64::MAX)
}

pub(crate) fn fire_auth_failure_notification(server: &str, message: &str) {
    let summary = format!("Conduit · {server} auth failed");
    let body = format!("{message}\nOpen the Servers tab to edit credentials.");
    if let Err(e) = notify_rust::Notification::new()
        .summary(&summary)
        .body(&body)
        .show()
    {
        tracing::warn!(error = %e, "auth-failure notification failed");
    }
}

pub(crate) fn fire_background_hint() {
    let summary = "Conduit is running in the background";
    let body = "Click the tray icon, press Ctrl+Alt+Shift+M, or run `conduit` to reopen.";
    if let Err(e) = notify_rust::Notification::new()
        .summary(summary)
        .body(body)
        .show()
    {
        tracing::warn!(error = %e, "background-hint notification failed");
    }
}

/// The config path is the projected `client.mcp_config_path` — already
/// OS-resolved at detect. Empty means we have no mapping for this OS.
pub(crate) fn resolve_ide_config_path(
    record: Option<&conduit::daemon::db::types::ClientRow>,
) -> Option<std::path::PathBuf> {
    record
        .filter(|rec| !rec.mcp_config_path.is_empty())
        .map(|rec| std::path::PathBuf::from(&rec.mcp_config_path))
}

/// Read a value from the UI thread into an async context.
///
/// `upgrade_in_event_loop` is fire-and-forget; this helper bridges the gap with
/// a oneshot channel so the caller can await the value the UI thread produces.
pub(crate) async fn read_ui_state<T>(
    weak: &slint::Weak<MainWindow>,
    f: impl FnOnce(MainWindow) -> T + Send + 'static,
) -> Option<T>
where
    T: Send + 'static,
{
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ignored = weak.upgrade_in_event_loop(move |ui| {
        tx.send(f(ui)).ok();
    });
    rx.await.ok()
}

pub(crate) async fn current_selected_server(weak: &slint::Weak<MainWindow>) -> Option<String> {
    read_ui_state(weak, |ui| ui.get_selected_server().to_string())
        .await
        .filter(|s: &String| !s.is_empty())
}

/// Untrack a client and purge our entries from its config file.
pub(crate) async fn remove_ide(db: &Db, port: u16, client_id: &str) {
    let resolved = match client_config::resolve_client(db, client_id).await {
        Ok(Some(c)) => Some(c),
        Ok(None) => None,
        Err(e) => {
            tracing::warn!(error = %e, client_id, "resolve_client failed");
            None
        }
    };
    if let Some(client) = resolved {
        let schema = client.schema.clone();
        let path = client.config_path.clone();
        let key = client.mcp_key.clone();
        let purge = tokio::task::spawn_blocking(move || {
            client_config::purge_mcp(&schema, &path, &key, port)
        })
        .await;
        match purge {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(error = %e, client_id, "purge_mcp failed"),
            Err(e) => tracing::warn!(error = %e, client_id, "purge_mcp join failed"),
        }
    }
    if let Err(e) = db.set_client_tracked(client_id, false).await {
        tracing::warn!(error = %e, client_id, "set_client_tracked(false) failed");
    }
}

/// Detect installed built-ins and project their identity into the `client`
/// table (version-gated, §1a). The single source of truth thereafter is the DB.
pub(crate) async fn run_ide_detection(db: &Db) {
    let detected = tokio::task::spawn_blocking(client_config::detect::detect_installed)
        .await
        .unwrap_or_default();
    if let Err(e) = client_config::project::project_builtins(db, &detected).await {
        tracing::warn!(error = %e, "project_builtins failed");
    }
}

/// Home-relative display label for a scope string: "Global" or `~`-collapsed path.
pub(crate) fn display_scope(scope: &str) -> String {
    if scope == "global" {
        return "Global".to_string();
    }
    // Scope keys are `std::fs::canonicalize` output, which on Windows is a
    // verbatim `\\?\C:\...` path; `home_dir()` is non-verbatim, so `strip_prefix`
    // would never match and we'd show the raw `\\?\` string. De-verbatim first.
    let scope = strip_verbatim(scope);
    if let Some(home) = dirs::home_dir() {
        if let Ok(rel) = std::path::Path::new(scope.as_ref()).strip_prefix(&home) {
            return if rel.as_os_str().is_empty() {
                "~".to_string()
            } else {
                format!("~/{}", rel.display())
            };
        }
    }
    scope.into_owned()
}

#[cfg(windows)]
fn strip_verbatim(s: &str) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        Cow::Owned(format!(r"\\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        Cow::Borrowed(rest)
    } else {
        Cow::Borrowed(s)
    }
}

#[cfg(not(windows))]
const fn strip_verbatim(s: &str) -> std::borrow::Cow<'_, str> {
    std::borrow::Cow::Borrowed(s)
}

pub(crate) fn lifecycle_label(secs: i32) -> String {
    match secs {
        -1 => "persistent".to_string(),
        0 => "ephemeral".to_string(),
        n => format!("{n}s idle"),
    }
}

pub(crate) fn settings_to_row(s: &Settings) -> SettingsRow {
    SettingsRow {
        port: SharedString::from(s.daemon.port.to_string()),
        approval_timeout: SharedString::from(s.daemon.approval_timeout_seconds.to_string()),
        default_lifecycle: SharedString::from(s.daemon.default_lifecycle_seconds.to_string()),
        audit_retention: SharedString::from(s.daemon.audit_retention_days.to_string()),
        log_level: SharedString::from(s.daemon.log_level.clone()),
        autostart_at_login: s.daemon.autostart_at_login,
        default_unknown_tool_policy: SharedString::from(
            s.daemon.default_unknown_tool_policy.clone(),
        ),
        theme: SharedString::from(s.gui.theme.clone()),
        focus_hotkey: SharedString::from(s.gui.focus_hotkey.clone()),
        update_channel: SharedString::from(s.gui.update_channel.clone()),
        update_check_interval_hours: SharedString::from(
            s.gui.update_check_interval_hours.to_string(),
        ),
        show_rss_in_servers_tab: s.gui.show_rss_in_servers_tab,
        open_window_at_startup: s.gui.open_window_at_startup,
        close_to_tray: s.gui.close_to_tray,
        notifications_enabled: s.notifications.enabled,
        notifications_sound: s.notifications.sound,
        skills_sync_mode: SharedString::from(match s.skills.sync_mode {
            conduit::daemon::config::toml::SyncMode::Symlink => "symlink",
            conduit::daemon::config::toml::SyncMode::Copy => "copy",
        }),
    }
}

pub(crate) fn copy_to_clipboard(text: &str) {
    match arboard::Clipboard::new() {
        Ok(mut cb) => {
            if let Err(e) = cb.set_text(text.to_string()) {
                tracing::warn!(error = %e, "clipboard set_text failed");
            }
        }
        Err(e) => tracing::warn!(error = %e, "clipboard open failed"),
    }
}

pub(crate) fn reveal_in_file_manager(path: &std::path::Path) {
    let result: std::io::Result<std::process::Child> = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn()
    } else if cfg!(target_os = "windows") {
        let mut arg = std::ffi::OsString::from("/select,");
        arg.push(path);
        std::process::Command::new("explorer").arg(arg).spawn()
    } else {
        let dir = path.parent().unwrap_or(path);
        std::process::Command::new("xdg-open").arg(dir).spawn()
    };
    if let Err(e) = result {
        tracing::warn!(path = %path.display(), error = %e, "reveal in file manager failed");
    }
}
