//! Scope popover Slint callbacks (§8a) — shared by Server + Skill detail.
//!
//! Opening populates the model (`refresh_scope_popover`); each mutation applies
//! the change (`managed_mcp` + `render_scope`, or `SkillManager::set_skill_scope`),
//! then re-populates the popover and repaints the underlying detail. Path
//! validation for "add repo" runs on the worker via `spawn_blocking`.

use slint::{ComponentHandle, SharedString};

use conduit::daemon::client_config::{self, render_scope, resolve_client, Scope};
use conduit::daemon::db::Db;
use conduit::daemon::skills::SkillManager;

use crate::window::refresh::{refresh_scope_popover, refresh_server_detail};
use crate::window::skills::refresh_skills;
use crate::window::util::read_ui_state;
use crate::window::view::MainWindow;
use crate::window::CallbackContext;

async fn read_item_kind(weak: &slint::Weak<MainWindow>) -> String {
    read_ui_state(weak, |ui| ui.get_scope_popover_item_kind().to_string())
        .await
        .unwrap_or_default()
}

/// Re-derive one server scope's config file from its surviving managed set.
async fn rerender_server_scope(db: &Db, port: u16, editor: &str, scope_key: &str) {
    let Some(client) = resolve_client(db, editor).await.ok().flatten() else {
        return;
    };
    if !client.is_builtin {
        return;
    }
    let remaining = db
        .list_managed_servers(editor, scope_key)
        .await
        .unwrap_or_default();
    let scope = Scope::from_db(scope_key);
    match tokio::task::spawn_blocking(move || render_scope(&client, &scope, &remaining, port)).await
    {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(error = %e, "render_scope failed"),
        Err(e) => tracing::warn!(error = %e, "render_scope join failed"),
    }
}

/// Apply one `(item, editor, scope)` toggle for either kind.
async fn apply_scope(
    db: &Db,
    skills: &SkillManager,
    port: u16,
    kind: &str,
    item: &str,
    editor: &str,
    scope_key: &str,
    on: bool,
) {
    if kind == "skill" {
        if let Err(e) = skills.set_skill_scope(item, editor, scope_key, on).await {
            tracing::warn!(error = %e, "set_skill_scope failed");
        }
        return;
    }
    let res = if on {
        db.add_managed_server(editor, scope_key, item).await
    } else {
        db.remove_managed_server(editor, scope_key, item).await
    };
    if let Err(e) = res {
        tracing::warn!(error = %e, "managed_mcp scope mutation failed");
        return;
    }
    rerender_server_scope(db, port, editor, scope_key).await;
}

/// Scopes `item` is currently in for `editor` (snapshot for "Remove from all").
async fn member_scopes(db: &Db, kind: &str, item: &str, editor: &str) -> Vec<String> {
    let rows = if kind == "skill" {
        db.list_skill_targets_scoped(item).await.unwrap_or_default()
    } else {
        db.list_clients_managing_server(item)
            .await
            .unwrap_or_default()
    };
    rows.into_iter()
        .filter(|(c, _)| c == editor)
        .map(|(_, s)| s)
        .collect()
}

async fn repaint_detail(
    weak: &slint::Weak<MainWindow>,
    db: &Db,
    skills: &SkillManager,
    kind: &str,
    item: &str,
) {
    if kind == "skill" {
        refresh_skills(weak, skills).await;
    } else {
        refresh_server_detail(weak, db, item).await;
    }
}

#[expect(clippy::too_many_lines, reason = "wires every scope-popover callback")]
pub(crate) fn install_scope_callbacks(window: &MainWindow, ctx: &CallbackContext) {
    let db = &ctx.db;
    let rt = &ctx.rt;
    let port = ctx.port;
    let skills = &ctx.skills;

    {
        let db = db.clone();
        let rt = rt.clone();
        let weak = window.as_weak();
        window.on_open_scope_popover(move |item, kind, editor| {
            let db = db.clone();
            let weak = weak.clone();
            let (item, kind, editor) = (item.to_string(), kind.to_string(), editor.to_string());
            let _ignored = weak.upgrade_in_event_loop(|ui| {
                ui.set_scope_popover_add_path(SharedString::default());
                ui.set_scope_popover_add_error(SharedString::default());
            });
            rt.spawn(async move {
                refresh_scope_popover(&weak, &db, &item, &kind, &editor, true).await;
            });
        });
    }

    {
        let weak = window.as_weak();
        window.on_close_scope_popover(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_scope_popover_open(false);
                ui.set_scope_popover_add_path(SharedString::default());
                ui.set_scope_popover_add_error(SharedString::default());
            }
        });
    }

    {
        let weak = window.as_weak();
        window.on_edit_scope_add_path(move |text| {
            if let Some(ui) = weak.upgrade() {
                ui.set_scope_popover_add_path(text);
                ui.set_scope_popover_add_error(SharedString::default());
            }
        });
    }

    {
        let db = db.clone();
        let rt = rt.clone();
        let skills = skills.clone();
        let weak = window.as_weak();
        window.on_toggle_scope(move |item, editor, scope_key, on| {
            let db = db.clone();
            let skills = skills.clone();
            let weak = weak.clone();
            let (item, editor, scope_key) =
                (item.to_string(), editor.to_string(), scope_key.to_string());
            rt.spawn(async move {
                let kind = read_item_kind(&weak).await;
                apply_scope(&db, &skills, port, &kind, &item, &editor, &scope_key, on).await;
                refresh_scope_popover(&weak, &db, &item, &kind, &editor, false).await;
                repaint_detail(&weak, &db, &skills, &kind, &item).await;
            });
        });
    }

    {
        let db = db.clone();
        let rt = rt.clone();
        let skills = skills.clone();
        let weak = window.as_weak();
        window.on_remove_scope(move |item, editor, scope_key| {
            let db = db.clone();
            let skills = skills.clone();
            let weak = weak.clone();
            let (item, editor, scope_key) =
                (item.to_string(), editor.to_string(), scope_key.to_string());
            rt.spawn(async move {
                let kind = read_item_kind(&weak).await;
                apply_scope(&db, &skills, port, &kind, &item, &editor, &scope_key, false).await;
                refresh_scope_popover(&weak, &db, &item, &kind, &editor, false).await;
                repaint_detail(&weak, &db, &skills, &kind, &item).await;
            });
        });
    }

    {
        let db = db.clone();
        let rt = rt.clone();
        let skills = skills.clone();
        let weak = window.as_weak();
        window.on_remove_all_scopes(move |item, editor| {
            let db = db.clone();
            let skills = skills.clone();
            let weak = weak.clone();
            let (item, editor) = (item.to_string(), editor.to_string());
            rt.spawn(async move {
                let kind = read_item_kind(&weak).await;
                for scope_key in member_scopes(&db, &kind, &item, &editor).await {
                    apply_scope(&db, &skills, port, &kind, &item, &editor, &scope_key, false).await;
                }
                // The popover is dismissed by the click handler; only repaint the
                // underlying detail (re-populating here would re-open it).
                repaint_detail(&weak, &db, &skills, &kind, &item).await;
            });
        });
    }

    {
        let db = db.clone();
        let rt = rt.clone();
        let skills = skills.clone();
        let weak = window.as_weak();
        window.on_add_repo_scope(move |item, editor, path| {
            let db = db.clone();
            let skills = skills.clone();
            let weak = weak.clone();
            let (item, editor, raw) = (
                item.to_string(),
                editor.to_string(),
                path.trim().to_string(),
            );
            if raw.is_empty() {
                return;
            }
            rt.spawn(async move {
                let kind = read_item_kind(&weak).await;
                let validated =
                    tokio::task::spawn_blocking(move || client_config::normalize_scope_path(&raw))
                        .await;
                let scope_key = match validated {
                    Ok(Ok(s)) => s,
                    Ok(Err(e)) => {
                        let msg = e.to_string();
                        let _ignored = weak.upgrade_in_event_loop(move |ui| {
                            ui.set_scope_popover_add_error(SharedString::from(msg));
                        });
                        return;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "normalize_scope_path join failed");
                        return;
                    }
                };
                apply_scope(&db, &skills, port, &kind, &item, &editor, &scope_key, true).await;
                let _ignored = weak.upgrade_in_event_loop(|ui| {
                    ui.set_scope_popover_add_path(SharedString::default());
                    ui.set_scope_popover_add_error(SharedString::default());
                });
                refresh_scope_popover(&weak, &db, &item, &kind, &editor, false).await;
                repaint_detail(&weak, &db, &skills, &kind, &item).await;
            });
        });
    }
}
