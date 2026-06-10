//! Settings-tab Slint callbacks: typed setting commits + open-config-folder.
//!
//! Every edit funnels through `ConfigWriter::set` (validate → write →
//! `ArcSwap` store). On success the tab repaints from the fresh shared
//! settings; on
//! rejection it repaints from last-good (revert) and flags the field invalid
//! so the UI shows a red border.

use slint::{ComponentHandle, SharedString};

use crate::window::util::{reveal_in_file_manager, settings_to_row};
use crate::window::view::MainWindow;
use crate::window::CallbackContext;

pub(crate) fn install_settings_callbacks(window: &MainWindow, ctx: &CallbackContext) {
    let weak = window.as_weak();
    let config = ctx.config.clone();
    let settings = ctx.settings.clone();
    let rt = ctx.rt.clone();
    let commit = move |key: SharedString, value: String| {
        let weak = weak.clone();
        let config = config.clone();
        let settings = settings.clone();
        let key = key.to_string();
        rt.spawn(async move {
            // Skip no-op commits (e.g. blur without a change) so an idle focus
            // change never rewrites the file.
            if current_value(&settings.load(), &key).is_some_and(|cur| cur == value) {
                let key_for_ui = key;
                weak.upgrade_in_event_loop(move |ui| {
                    if ui.get_settings_invalid_key().as_str() == key_for_ui {
                        ui.set_settings_invalid_key(SharedString::default());
                    }
                })
                .ok();
                return;
            }
            let result = config.set(&key, &value).await;
            // Success or rejection, repaint from the shared settings: success
            // shows the new value, rejection reverts the field to last-good.
            let row_settings = settings.load_full();
            weak.upgrade_in_event_loop(move |ui| {
                ui.set_settings(settings_to_row(&row_settings));
                // Revision bump forces text inputs back to the row value —
                // that's the revert on rejection.
                ui.set_settings_revision(ui.get_settings_revision().wrapping_add(1));
                ui.set_settings_invalid_key(match result {
                    Ok(()) => SharedString::default(),
                    Err(_) => SharedString::from(key),
                });
            })
            .ok();
        });
    };

    let commit_bool = commit.clone();
    window.on_set_bool_setting(move |key, v| {
        commit_bool(key, if v { "true" } else { "false" }.to_string());
    });
    let commit_number = commit.clone();
    window.on_set_number_setting(move |key, v| commit_number(key, v.trim().to_string()));
    let commit_enum = commit.clone();
    window.on_set_enum_setting(move |key, v| commit_enum(key, v.to_string()));
    window.on_set_text_setting(move |key, v| commit(key, v.trim().to_string()));

    let config_toml_path = ctx.config_toml_path.clone();
    window.on_open_config_folder(move || {
        reveal_in_file_manager(&config_toml_path);
    });
}

/// The current stringified value of a settings key, mirroring the key set
/// accepted by `ConfigEdit::parse`. `None` for unknown keys — the writer will
/// reject those on commit.
fn current_value(s: &conduit::daemon::config::toml::Settings, key: &str) -> Option<String> {
    let v = match key {
        "daemon.port" => s.daemon.port.to_string(),
        "daemon.approval_timeout_seconds" => s.daemon.approval_timeout_seconds.to_string(),
        "daemon.default_lifecycle_seconds" => s.daemon.default_lifecycle_seconds.to_string(),
        "daemon.audit_retention_days" => s.daemon.audit_retention_days.to_string(),
        "daemon.log_level" => s.daemon.log_level.clone(),
        "daemon.autostart_at_login" => s.daemon.autostart_at_login.to_string(),
        "daemon.default_unknown_tool_policy" => s.daemon.default_unknown_tool_policy.clone(),
        "gui.theme" => s.gui.theme.clone(),
        "gui.focus_hotkey" => s.gui.focus_hotkey.clone(),
        "gui.update_channel" => s.gui.update_channel.clone(),
        "gui.update_check_interval_hours" => s.gui.update_check_interval_hours.to_string(),
        "gui.show_rss_in_servers_tab" => s.gui.show_rss_in_servers_tab.to_string(),
        "gui.open_window_at_startup" => s.gui.open_window_at_startup.to_string(),
        "gui.close_to_tray" => s.gui.close_to_tray.to_string(),
        "notifications.enabled" => s.notifications.enabled.to_string(),
        "notifications.sound" => s.notifications.sound.to_string(),
        "skills.sync_mode" => match s.skills.sync_mode {
            conduit::daemon::config::toml::SyncMode::Symlink => "symlink".to_string(),
            conduit::daemon::config::toml::SyncMode::Copy => "copy".to_string(),
        },
        _ => return None,
    };
    Some(v)
}
