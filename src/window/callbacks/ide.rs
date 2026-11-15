//! Editors-tab Slint callbacks: per-IDE config view/edit/save and detection.
//!
//! Wires the raw-config editor (read/validate/save/reload/reveal) plus
//! remove/retrack/refresh of detected IDEs. Handlers offload DB and filesystem
//! work to the tokio worker and repaint via `crate::window::refresh`; UI mutation
//! only happens through `upgrade_in_event_loop`.

use std::path::PathBuf;

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use conduit::daemon::client_config::{self, ConfigFormat, Scope};
use conduit::daemon::db::Db;

use crate::window::refresh::{refresh_ides, refresh_server_targets};
use crate::window::skills::refresh_skills;
use crate::window::util::{
    current_selected_server, display_scope, remove_ide, resolve_ide_config_path,
    reveal_in_file_manager, run_ide_detection,
};
use crate::window::view::{MainWindow, ScopeOption as ScopeOptionUi};
use crate::window::CallbackContext;

const fn format_str(format: ConfigFormat) -> &'static str {
    match format {
        ConfigFormat::Toml => "toml",
        ConfigFormat::Json | ConfigFormat::Jsonc => "json",
    }
}

fn format_from_str(s: &str) -> ConfigFormat {
    if s == "toml" {
        ConfigFormat::Toml
    } else {
        ConfigFormat::Json
    }
}

/// Resolve the absolute config file + format for one editor at one scope.
/// `None` = no config path for that (editor, scope).
async fn editor_scope_target(db: &Db, kind: &str, scope: &str) -> Option<(PathBuf, ConfigFormat)> {
    let client = client_config::resolve_client(db, kind)
        .await
        .ok()
        .flatten()?;
    let path = client.config_target(&Scope::from_db(scope))?;
    Some((path, client.schema.format))
}

/// Repaint the right-hand detail pane for `kind_str` at `scope` from the DB +
/// disk. Shared by selection, scope-switch, retrack, and remove so the pane
/// never goes stale when an action mutates the selected IDE under it.
async fn refresh_selected_editor(
    weak: &slint::Weak<MainWindow>,
    db: &Db,
    kind_str: String,
    scope: String,
) {
    let record = db.get_client(&kind_str).await.ok().flatten();
    let status = record.as_ref().map_or_else(
        || "active".to_string(),
        |r| {
            if r.tracked {
                "active".to_string()
            } else {
                "not_tracked".to_string()
            }
        },
    );
    let display = record
        .as_ref()
        .map_or_else(|| kind_str.clone(), |r| r.name.clone());

    // Resolve the file for the chosen scope (global → config_path; repo → join).
    let (path, format) = match editor_scope_target(db, &kind_str, &scope).await {
        Some((p, f)) => (Some(p), f),
        None => (resolve_ide_config_path(record.as_ref()), ConfigFormat::Json),
    };
    let text = match path.as_deref().map(client_config::read_raw) {
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

    // Scope options: Global always, plus every repo this client manages anything in.
    let mut scope_opts: Vec<ScopeOptionUi> = vec![ScopeOptionUi {
        label: SharedString::from("Global"),
        scope_key: SharedString::from("global"),
    }];
    for s in db
        .list_scopes_for_client(&kind_str)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s != "global")
    {
        scope_opts.push(ScopeOptionUi {
            label: SharedString::from(display_scope(&s)),
            scope_key: SharedString::from(s),
        });
    }
    let format_label = format_str(format).to_string();

    let _ignored = weak.upgrade_in_event_loop(move |ui| {
        ui.set_selected_editor(SharedString::from(kind_str));
        ui.set_selected_editor_display(SharedString::from(display));
        ui.set_selected_editor_status_label(SharedString::from(status));
        ui.set_selected_editor_text(SharedString::from(text));
        ui.set_selected_editor_config_name(SharedString::from(config_name));
        ui.set_selected_editor_scopes(ModelRc::new(VecModel::from(scope_opts)));
        ui.set_selected_editor_scope(SharedString::from(scope));
        ui.set_selected_editor_format(SharedString::from(format_label));
        ui.set_selected_editor_status(SharedString::default());
        ui.set_selected_editor_status_is_error(false);
        ui.set_selected_editor_dirty(false);
    });
}

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
                refresh_selected_editor(&weak, &db, kind_str, "global".to_string()).await;
            });
        });
    }

    {
        let db = db.clone();
        let rt = rt.clone();
        let weak = window.as_weak();
        window.on_select_editor_scope(move |scope| {
            let db = db.clone();
            let weak = weak.clone();
            let scope = scope.to_string();
            rt.spawn(async move {
                let kind = crate::window::util::read_ui_state(&weak, |ui| {
                    ui.get_selected_editor().to_string()
                })
                .await
                .unwrap_or_default();
                if kind.is_empty() {
                    return;
                }
                refresh_selected_editor(&weak, &db, kind, scope).await;
            });
        });
    }

    {
        let weak = window.as_weak();
        window.on_edit_ide_config(move |text| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let format = format_from_str(ui.get_selected_editor_format().as_str());
            let validation = client_config::validate_config(text.as_str(), format);
            ui.set_selected_editor_text(text);
            ui.set_selected_editor_dirty(true);
            match validation {
                Ok(()) => {
                    let ok = if format == ConfigFormat::Toml {
                        "valid TOML"
                    } else {
                        "valid JSON"
                    };
                    ui.set_selected_editor_status(SharedString::from(ok));
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
            let scope = ui.get_selected_editor_scope().to_string();
            let text = ui.get_selected_editor_text().to_string();
            if kind_str.is_empty() {
                return;
            }
            let db = db.clone();
            let weak = weak.clone();
            rt.spawn(async move {
                let Some((path, format)) = editor_scope_target(&db, &kind_str, &scope).await else {
                    let _ignored = weak.upgrade_in_event_loop(move |ui| {
                        ui.set_selected_editor_status(SharedString::from("no config path"));
                        ui.set_selected_editor_status_is_error(true);
                    });
                    return;
                };
                let result = client_config::write_raw_format(&path, &text, format);
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
            let scope = ui.get_selected_editor_scope().to_string();
            if kind_str.is_empty() {
                return;
            }
            let db = db.clone();
            let weak = weak.clone();
            rt.spawn(async move {
                let path = editor_scope_target(&db, &kind_str, &scope)
                    .await
                    .map(|(p, _)| p);
                let text = match path.as_deref().map(client_config::read_raw) {
                    Some(Ok(Some(s))) => s,
                    Some(Ok(None)) | None => String::new(),
                    Some(Err(e)) => format!("// read error: {e}"),
                };
                let _ignored = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_selected_editor_reload_nonce(
                        ui.get_selected_editor_reload_nonce().wrapping_add(1),
                    );
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
                let record = db.get_client(&kind_str).await.ok().flatten();
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
            refresh_selected_editor(&weak, &db, kind_str, "global".to_string()).await;
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
            if let Err(e) = db.set_client_tracked(&kind_str, true).await {
                tracing::warn!(error = %e, kind = %kind_str, "retrack failed");
                return;
            }
            refresh_ides(&weak, &db).await;
            refresh_selected_editor(&weak, &db, kind_str, "global".to_string()).await;
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
