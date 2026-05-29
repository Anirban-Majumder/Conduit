//! Miscellaneous Slint callbacks: settings toggles, policy edits, paste parsing.
//!
//! Covers the settings-tab `settings_kv` toggles, global policy changes, and the
//! add-server JSON paste parser. Like the other callback modules, DB work runs
//! on the tokio worker and UI writes go through the Slint event loop.

use slint::{ComponentHandle, SharedString};

use conduit::daemon::db::types::{Decision, DecisionSource};
use conduit::daemon::db::Db;
use conduit::daemon::paste_parser;

use crate::ui::MainWindow;

pub(crate) fn install_misc_callbacks(window: &MainWindow, db: &Db, rt: &tokio::runtime::Handle) {
    let db_for_open_toggle = db.clone();
    let rt_for_open_toggle = rt.clone();
    window.on_toggle_open_window_at_startup(move |v| {
        let db = db_for_open_toggle.clone();
        rt_for_open_toggle.spawn(async move {
            let value = if v { "true" } else { "false" };
            if let Err(e) = db.set_settings_kv("open_window_at_startup", value).await {
                tracing::warn!(error = %e, "set open_window_at_startup failed");
            }
        });
    });

    let db_for_default_policy = db.clone();
    let rt_for_default_policy = rt.clone();
    window.on_set_default_unknown_tool_policy(move |p| {
        let db = db_for_default_policy.clone();
        let p = p.to_string();
        rt_for_default_policy.spawn(async move {
            if let Err(e) = db.set_settings_kv("default_unknown_tool_policy", &p).await {
                tracing::warn!(error = %e, "set default_unknown_tool_policy failed");
            }
        });
    });

    let db_for_policy = db.clone();
    let rt_for_policy = rt.clone();
    window.on_change_policy(move |server, tool, decision_str| {
        let db = db_for_policy.clone();
        let Some(decision) = Decision::from_db_str(decision_str.as_str()) else {
            tracing::warn!(decision = %decision_str.as_str(), "unknown decision");
            return;
        };
        rt_for_policy.spawn(async move {
            if let Err(e) = db
                .set_policy(
                    server.as_str(),
                    tool.as_str(),
                    decision,
                    DecisionSource::User,
                )
                .await
            {
                tracing::warn!(error = %e, "set policy failed");
            }
        });
    });

    let weak_for_paste = window.as_weak();
    window.on_parse_paste(move || {
        let Some(ui) = weak_for_paste.upgrade() else {
            return;
        };
        let raw = ui.get_add_server_paste().to_string();
        if raw.trim().is_empty() {
            return;
        }
        let Ok(parsed) = paste_parser::parse(&raw) else {
            ui.set_add_server_status(SharedString::from("Not a valid MCP JSON paste."));
            ui.set_add_server_status_is_error(true);
            return;
        };
        let transport = parsed.inferred_transport();
        ui.set_add_server_transport(SharedString::from(transport));
        if let Some(n) = parsed.name {
            ui.set_add_server_name(SharedString::from(n));
        }
        if let Some(c) = parsed.command {
            ui.set_add_server_command(SharedString::from(c));
        }
        if !parsed.args.is_empty() {
            ui.set_add_server_args(SharedString::from(parsed.args.join(" ")));
        }
        if let Some(u) = parsed.url {
            ui.set_add_server_url(SharedString::from(u));
        }
        if !parsed.headers.is_empty() {
            let joined = parsed
                .headers
                .iter()
                .map(|h| format!("{}={}", h.name, h.value))
                .collect::<Vec<_>>()
                .join(", ");
            ui.set_add_server_headers(SharedString::from(joined));
        }
        ui.set_add_server_paste(SharedString::from(""));
        if ui.get_add_server_status_is_error() {
            ui.set_add_server_status(SharedString::from(""));
            ui.set_add_server_status_is_error(false);
        }
    });
}
