//! Small shared helpers for the glue layer.
//!
//! Time formatting, IDE config-path resolution and detection, OS notification
//! firing, file-manager reveal, and settings→Slint-row conversion — used across
//! the callback and refresh modules.

use slint::SharedString;

use conduit::daemon::clients;
use conduit::daemon::config_toml::Settings;
use conduit::daemon::db::Db;
use conduit::daemon::ide_detect::IdeKind;

use crate::ui::{MainWindow, SettingsRow};

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

pub(crate) fn resolve_ide_config_path(
    record: Option<&conduit::daemon::db::types::IdeRecord>,
) -> Option<std::path::PathBuf> {
    if let Some(rec) = record {
        if let Some(p) = rec.config_path.clone() {
            return Some(p);
        }
        if let Some(kind) = IdeKind::from_db_kind(&rec.ide_kind) {
            return clients::config_path_for(kind);
        }
    }
    None
}

pub(crate) async fn current_selected_server(weak: &slint::Weak<MainWindow>) -> Option<String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ignored = weak.upgrade_in_event_loop(move |ui| {
        let s = ui.get_selected_server().to_string();
        tx.send(s).ok();
    });
    rx.await.ok().filter(|s| !s.is_empty())
}

pub(crate) async fn remove_ide(db: &Db, port: u16, ide_kind: &str) {
    let Some(record) = db.get_ide_by_kind(ide_kind).await.ok().flatten() else {
        return;
    };
    let Some(kind) = IdeKind::from_db_kind(&record.ide_kind) else {
        return;
    };
    if let Err(e) = clients::purge(kind, &record.ide_key, port) {
        tracing::warn!(error = %e, kind = %record.ide_kind, "ide purge failed");
    }
    if let Err(e) = db.mark_ide_not_tracked(&record.ide_kind).await {
        tracing::warn!(error = %e, kind = %record.ide_kind, "mark_ide_not_tracked failed");
    }
}

pub(crate) async fn run_ide_detection(db: &Db) {
    let detected = tokio::task::spawn_blocking(conduit::daemon::ide_detect::detect_installed)
        .await
        .unwrap_or_default();
    let mut seen_kinds: Vec<String> = Vec::with_capacity(detected.len());
    for d in &detected {
        let kind_str = d.kind.as_db_kind().to_string();
        seen_kinds.push(kind_str.clone());
        let source_str = d.source.as_db_str();
        if let Err(e) = db
            .upsert_detected_ide(&kind_str, d.kind.display_name(), Some(source_str))
            .await
        {
            tracing::warn!(error = %e, kind = %kind_str, "upsert_detected_ide failed");
        }
    }
    if let Err(e) = db.mark_ides_stale(seen_kinds).await {
        tracing::warn!(error = %e, "mark_ides_stale failed");
    }
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
        theme: SharedString::from(s.gui.theme.clone()),
        autostart_at_login: SharedString::from(s.daemon.autostart_at_login.to_string()),
        notifications_enabled: SharedString::from(s.notifications.enabled.to_string()),
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
