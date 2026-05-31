//! Editors-tab Slint callbacks: per-IDE config view/edit/save and detection.
//!
//! Wires the raw-config editor (read/validate/save/reload/reveal) plus
//! remove/retrack/refresh of detected IDEs. Handlers offload DB and filesystem
//! work to the tokio worker and repaint via `crate::glue::refresh`; UI mutation
//! only happens through `upgrade_in_event_loop`.

use slint::{ComponentHandle, SharedString};

use conduit::daemon::clients;

use crate::glue::refresh::{refresh_ides, refresh_server_targets};
use crate::glue::skills::refresh_skills;
use crate::glue::util::{
    current_selected_server, remove_ide, resolve_ide_config_path, reveal_in_file_manager,
    run_ide_detection,
};
use crate::glue::CallbackContext;
use crate::ui::MainWindow;

#[expect(clippy::too_many_lines, reason = "wires every editors-tab callback")]
pub(crate) fn install_ide_callbacks(window: &MainWindow, ctx: &CallbackContext) {
    let db = &ctx.db;
    let port = ctx.port;
    let rt = &ctx.rt;
    let skills = &ctx.skills;

    {
        let db = db.clone();
        let rt = rt.clone();
        let weak = window.as_weak();
        window.on_select_ide(move |kind_str| {
            let db = db.clone();
            let weak = weak.clone();
            let kind_str = kind_str.to_string();
            rt.spawn(async move {
                let record = db.get_ide_by_kind(&kind_str).await.ok().flatten();
                let path = resolve_ide_config_path(record.as_ref());
                let status = record.as_ref().map_or_else(
                    || "active".to_string(),
                    |r| r.status.as_db_str().to_string(),
                );
                let display = record
                    .as_ref()
                    .map_or_else(|| kind_str.clone(), |r| r.display_name.clone());
                let text = match path.as_deref().map(clients::read_raw) {
                    Some(Ok(Some(s))) => s,
                    Some(Ok(None)) | None => String::new(),
                    Some(Err(e)) => format!("// read error: {e}"),
                };
                let config_name = path
                    .as_deref()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .to_string();
                let _ignored = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_selected_editor(SharedString::from(kind_str));
                    ui.set_selected_editor_display(SharedString::from(display));
                    ui.set_selected_editor_status_label(SharedString::from(status));
                    ui.set_selected_editor_text(SharedString::from(text));
                    ui.set_selected_editor_config_name(SharedString::from(config_name));
                    ui.set_selected_editor_status(SharedString::default());
                    ui.set_selected_editor_status_is_error(false);
                    ui.set_selected_editor_dirty(false);
                });
            });
        });
    }

    {
        let weak = window.as_weak();
        window.on_edit_ide_config(move |text| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let validation = clients::validate_json(text.as_str());
            ui.set_selected_editor_text(text);
            ui.set_selected_editor_dirty(true);
            match validation {
                Ok(()) => {
                    ui.set_selected_editor_status(SharedString::from("valid JSON"));
                    ui.set_selected_editor_status_is_error(false);
                }
                Err(e) => {
                    ui.set_selected_editor_status(SharedString::from(e.to_string()));
                    ui.set_selected_editor_status_is_error(true);
                }
            }
        });
    }

    {
        let db = db.clone();
        let rt = rt.clone();
        let weak = window.as_weak();
        window.on_save_ide_config(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let kind_str = ui.get_selected_editor().to_string();
            let text = ui.get_selected_editor_text().to_string();
            if kind_str.is_empty() {
                return;
            }
            let db = db.clone();
            let weak = weak.clone();
            rt.spawn(async move {
                let record = db.get_ide_by_kind(&kind_str).await.ok().flatten();
                let Some(path) = resolve_ide_config_path(record.as_ref()) else {
                    let _ignored = weak.upgrade_in_event_loop(move |ui| {
                        ui.set_selected_editor_status(SharedString::from("no config path"));
                        ui.set_selected_editor_status_is_error(true);
                    });
                    return;
                };
                let result = clients::write_raw(&path, &text);
                let _ignored = weak.upgrade_in_event_loop(move |ui| match result {
                    Ok(()) => {
                        ui.set_selected_editor_status(SharedString::from("saved"));
                        ui.set_selected_editor_status_is_error(false);
                        ui.set_selected_editor_dirty(false);
                    }
                    Err(e) => {
                        ui.set_selected_editor_status(SharedString::from(e.to_string()));
                        ui.set_selected_editor_status_is_error(true);
                    }
                });
            });
        });
    }

    {
        let db = db.clone();
        let rt = rt.clone();
        let weak = window.as_weak();
        window.on_reload_ide_config(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let kind_str = ui.get_selected_editor().to_string();
            if kind_str.is_empty() {
                return;
            }
            let db = db.clone();
            let weak = weak.clone();
            rt.spawn(async move {
                let record = db.get_ide_by_kind(&kind_str).await.ok().flatten();
                let path = resolve_ide_config_path(record.as_ref());
                let text = match path.as_deref().map(clients::read_raw) {
                    Some(Ok(Some(s))) => s,
                    Some(Ok(None)) | None => String::new(),
                    Some(Err(e)) => format!("// read error: {e}"),
                };
                let _ignored = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_selected_editor_text(SharedString::from(text));
                    ui.set_selected_editor_status(SharedString::default());
                    ui.set_selected_editor_status_is_error(false);
                    ui.set_selected_editor_dirty(false);
                });
            });
        });
    }

    {
        let db = db.clone();
        let rt = rt.clone();
        window.on_reveal_ide_config(move |kind_str| {
            let db = db.clone();
            let kind_str = kind_str.to_string();
            rt.spawn(async move {
                let record = db.get_ide_by_kind(&kind_str).await.ok().flatten();
                if let Some(path) = resolve_ide_config_path(record.as_ref()) {
                    reveal_in_file_manager(&path);
                }
            });
        });
    }

    let db_for_remove_ide = db.clone();
    let rt_for_remove_ide = rt.clone();
    let weak_for_remove_ide = window.as_weak();
    let skills_for_remove_ide = skills.clone();
    let port_for_remove_ide = port;
    window.on_remove_ide_clicked(move |kind_str| {
        let db = db_for_remove_ide.clone();
        let weak = weak_for_remove_ide.clone();
        let skills = skills_for_remove_ide.clone();
        let kind_str = kind_str.to_string();
        rt_for_remove_ide.spawn(async move {
            remove_ide(&db, port_for_remove_ide, &kind_str).await;
            refresh_ides(&weak, &db).await;
            // Repaint skills: their per-skill toggle set tracks detected IDEs.
            refresh_skills(&weak, &skills).await;
            if let Some(server) = current_selected_server(&weak).await {
                refresh_server_targets(&weak, &db, &server).await;
            }
        });
    });

    let db_for_retrack = db.clone();
    let rt_for_retrack = rt.clone();
    let weak_for_retrack = window.as_weak();
    let skills_for_retrack = skills.clone();
    window.on_retrack_ide_clicked(move |kind_str| {
        let db = db_for_retrack.clone();
        let weak = weak_for_retrack.clone();
        let skills = skills_for_retrack.clone();
        let kind_str = kind_str.to_string();
        rt_for_retrack.spawn(async move {
            if let Err(e) = db.retrack_ide(&kind_str).await {
                tracing::warn!(error = %e, kind = %kind_str, "retrack failed");
                return;
            }
            refresh_ides(&weak, &db).await;
            refresh_skills(&weak, &skills).await;
            if let Some(server) = current_selected_server(&weak).await {
                refresh_server_targets(&weak, &db, &server).await;
            }
        });
    });

    let db_for_refresh_ides = db.clone();
    let rt_for_refresh_ides = rt.clone();
    let weak_for_refresh_ides = window.as_weak();
    let skills_for_refresh_ides = skills.clone();
    window.on_refresh_ides_clicked(move || {
        let db = db_for_refresh_ides.clone();
        let weak = weak_for_refresh_ides.clone();
        let skills = skills_for_refresh_ides.clone();
        weak.upgrade_in_event_loop(|ui| ui.set_ides_refreshing(true))
            .ok();
        rt_for_refresh_ides.spawn(async move {
            run_ide_detection(&db).await;
            refresh_ides(&weak, &db).await;
            refresh_skills(&weak, &skills).await;
            if let Some(server) = current_selected_server(&weak).await {
                refresh_server_targets(&weak, &db, &server).await;
            }
            weak.upgrade_in_event_loop(|ui| ui.set_ides_refreshing(false))
                .ok();
        });
    });
}
