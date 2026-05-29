//! Servers-tab Slint callbacks: approvals, server CRUD, retest, policies, targets.
//!
//! Wires approve/deny, server removal (via `cascade`), selection, re-probe,
//! per-tool policy edits, IDE-target toggles, and the edit-server entry into the
//! wizard. Handlers offload to the tokio worker and repaint through
//! `crate::glue::refresh`; UI mutation only via `upgrade_in_event_loop`.

use std::sync::Arc;

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use tokio_util::sync::CancellationToken;

use conduit::daemon::approval::ApprovalCenter;
use conduit::daemon::cascade;
use conduit::daemon::db::types::{
    ApprovalStatus, Decision, DecisionSource, ToolMeta as DbToolMeta,
};
use conduit::daemon::db::Db;
use conduit::daemon::events::{Bus, Event};
use conduit::daemon::upstream::probe::{self};

use crate::ui::{MainWindow, ToolPolicyRow as ToolPolicyUiRow};

use crate::glue::refresh::{refresh_server_detail, refresh_servers, toggle_server_for_ide};
use crate::glue::util::unix_now;
use crate::glue::wizard::{build_draft, headers_field_from_env, picker_from_lifecycle};

#[expect(clippy::too_many_lines, reason = "wires every server-tab callback")]
pub(crate) fn install_server_callbacks(
    window: &MainWindow,
    db: &Db,
    approvals: &Arc<ApprovalCenter>,
    bus: &Bus,
    port: u16,
    rt: &tokio::runtime::Handle,
) {
    let approvals_for_approve = approvals.clone();
    let rt_for_approve = rt.clone();
    window.on_approve_clicked(move |id| {
        let approvals = approvals_for_approve.clone();
        rt_for_approve.spawn(async move {
            if let Err(e) = approvals
                .resolve(id.as_str(), ApprovalStatus::Approved)
                .await
            {
                tracing::warn!(error = %e, "approve failed");
            }
        });
    });

    let approvals_for_deny = approvals.clone();
    let rt_for_deny = rt.clone();
    window.on_deny_clicked(move |id| {
        let approvals = approvals_for_deny.clone();
        rt_for_deny.spawn(async move {
            if let Err(e) = approvals.resolve(id.as_str(), ApprovalStatus::Denied).await {
                tracing::warn!(error = %e, "deny failed");
            }
        });
    });

    let db_for_remove_server = db.clone();
    let bus_for_remove_server = bus.clone();
    let rt_for_remove_server = rt.clone();
    let weak_for_remove_server = window.as_weak();
    let port_for_remove = port;
    window.on_remove_server_clicked(move |name| {
        let db = db_for_remove_server.clone();
        let bus = bus_for_remove_server.clone();
        let weak = weak_for_remove_server.clone();
        let name = name.to_string();
        rt_for_remove_server.spawn(async move {
            match cascade::delete_server_with_cascade(&db, &bus, &name, port_for_remove, false)
                .await
            {
                Ok(report) => {
                    tracing::info!(
                        server = %name,
                        ides_trimmed = report.ides_trimmed.len(),
                        ides_skipped = report.ides_skipped.len(),
                        cancelled = report.cancelled_approvals.len(),
                        "cascade delete complete",
                    );
                }
                Err(e) => tracing::warn!(error = %e, server = %name, "cascade delete failed"),
            }
            let _ignored = weak.upgrade_in_event_loop(move |ui| {
                if ui.get_selected_server().as_str() == name {
                    ui.set_selected_server(SharedString::from(""));
                    ui.set_selected_server_tools(ModelRc::new(VecModel::from(Vec::<
                        ToolPolicyUiRow,
                    >::new(
                    ))));
                }
            });
            refresh_servers(&weak, &db).await;
        });
    });

    let db_for_select = db.clone();
    let rt_for_select = rt.clone();
    let weak_for_select = window.as_weak();
    window.on_select_server(move |name| {
        let db = db_for_select.clone();
        let weak = weak_for_select.clone();
        let name = name.to_string();
        rt_for_select.spawn(async move {
            refresh_server_detail(&weak, &db, &name).await;
        });
    });

    let db_for_retest = db.clone();
    let bus_for_retest = bus.clone();
    let rt_for_retest = rt.clone();
    let weak_for_retest = window.as_weak();
    window.on_retest_clicked(move |name| {
        let db = db_for_retest.clone();
        let bus = bus_for_retest.clone();
        let weak = weak_for_retest.clone();
        let name = name.to_string();
        rt_for_retest.spawn(async move {
            let Ok(Some(server)) = db.get_server(&name).await else {
                return;
            };
            let draft = build_draft(
                &server.name,
                &server.transport,
                server.command.as_deref().unwrap_or(""),
                server.args.clone(),
                server.url.as_deref().unwrap_or(""),
                "",
                server.lifecycle_seconds,
            );
            let cancel = CancellationToken::new();
            match probe::probe(&draft, cancel).await {
                Ok(result) => {
                    let now = unix_now();
                    let tools: Vec<DbToolMeta> = result
                        .tools
                        .into_iter()
                        .map(|t| DbToolMeta {
                            name: t.name,
                            description: t.description,
                            input_schema_json: t.input_schema.to_string(),
                            discovered_at: now,
                        })
                        .collect();
                    if let Err(e) = db.upsert_server_with_tools(server, tools).await {
                        tracing::warn!(error = %e, "re-test upsert failed");
                    }
                    match db.clear_server_auth_error(&name).await {
                        Ok(true) => bus.publish(Event::AuthCleared {
                            server: name.clone(),
                        }),
                        Ok(false) => {}
                        Err(e) => tracing::warn!(error = %e, "clear_server_auth_error after retest failed"),
                    }
                    refresh_server_detail(&weak, &db, &name).await;
                    refresh_servers(&weak, &db).await;
                }
                Err(e) => tracing::warn!(error = %e, server = %name, "re-test probe failed"),
            }
        });
    });

    let db_for_tool_policy = db.clone();
    let rt_for_tool_policy = rt.clone();
    let weak_for_tool_policy = window.as_weak();
    window.on_change_tool_policy(move |server, tool, decision_str| {
        let db = db_for_tool_policy.clone();
        let weak = weak_for_tool_policy.clone();
        let Some(decision) = Decision::from_db_str(decision_str.as_str()) else {
            return;
        };
        let server = server.to_string();
        let tool = tool.to_string();
        rt_for_tool_policy.spawn(async move {
            if tool == "*" {
                if let Ok(tools) = db.list_server_tools(&server).await {
                    for t in tools {
                        if let Err(e) = db
                            .set_policy(&server, &t.name, decision, DecisionSource::User)
                            .await
                        {
                            tracing::warn!(error = %e, "set policy failed");
                        }
                    }
                }
            } else if let Err(e) = db
                .set_policy(&server, &tool, decision, DecisionSource::User)
                .await
            {
                tracing::warn!(error = %e, "set policy failed");
            }
            refresh_server_detail(&weak, &db, &server).await;
        });
    });

    let db_for_toggle_target = db.clone();
    let rt_for_toggle_target = rt.clone();
    let weak_for_toggle_target = window.as_weak();
    let port_for_toggle_target = port;
    window.on_toggle_server_target(move |server, ide_kind, enable| {
        let db = db_for_toggle_target.clone();
        let weak = weak_for_toggle_target.clone();
        let server = server.to_string();
        let ide_kind = ide_kind.to_string();
        rt_for_toggle_target.spawn(async move {
            toggle_server_for_ide(&db, port_for_toggle_target, &server, &ide_kind, enable).await;
            refresh_server_detail(&weak, &db, &server).await;
        });
    });

    window.on_copy_error_clicked(move |text| {
        if text.is_empty() {
            return;
        }
        match arboard::Clipboard::new() {
            Ok(mut cb) => {
                if let Err(e) = cb.set_text(text.to_string()) {
                    tracing::warn!(error = %e, "clipboard set_text failed");
                }
            }
            Err(e) => tracing::warn!(error = %e, "clipboard open failed"),
        }
    });

    let db_for_edit = db.clone();
    let rt_for_edit = rt.clone();
    let weak_for_edit = window.as_weak();
    window.on_edit_server_clicked(move |name| {
        let db = db_for_edit.clone();
        let weak = weak_for_edit.clone();
        let name = name.to_string();
        rt_for_edit.spawn(async move {
            let Ok(Some(server)) = db.get_server(&name).await else {
                return;
            };
            let (mode, custom) = picker_from_lifecycle(server.lifecycle_seconds);
            let headers_field = headers_field_from_env(&server.env);
            let _ignored = weak.upgrade_in_event_loop(move |ui| {
                ui.set_wizard_edit_mode(true);
                ui.set_wizard_step(1);
                ui.set_show_add_server_dialog(true);
                ui.set_add_server_status(SharedString::default());
                ui.set_add_server_status_is_error(false);
                ui.set_add_server_busy(false);
                ui.set_add_server_name(SharedString::from(server.name.clone()));
                ui.set_add_server_transport(SharedString::from(server.transport.clone()));
                ui.set_add_server_command(SharedString::from(
                    server.command.clone().unwrap_or_default(),
                ));
                ui.set_add_server_args(SharedString::from(server.args.join(" ")));
                ui.set_add_server_url(SharedString::from(server.url.clone().unwrap_or_default()));
                ui.set_add_server_headers(SharedString::from(headers_field));
                ui.set_add_server_paste(SharedString::default());
                ui.set_add_server_lifecycle_mode(SharedString::from(mode));
                if !custom.is_empty() {
                    ui.set_add_server_lifecycle_custom(SharedString::from(custom));
                }
                ui.set_wizard_phase_status(SharedString::default());
                ui.set_wizard_oauth_url(SharedString::default());
            });
        });
    });
}
