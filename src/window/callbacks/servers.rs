//! Servers-tab Slint callbacks: approvals, server CRUD, retest, policies, targets.
//!
//! Wires approve/deny, server removal (via `cascade`), selection, re-probe,
//! per-tool policy edits, IDE-target toggles, and the edit-server entry into the
//! wizard. Handlers offload to the tokio worker and repaint through
//! `crate::window::refresh`; UI mutation only via `upgrade_in_event_loop`.

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use tokio_util::sync::CancellationToken;

use conduit::daemon::cascade;
use conduit::daemon::db::types::{
    ApprovalStatus, Decision, DecisionSource, EnvValue, McpTool as DbMcpTool, TRANSPORT_HTTP,
    TRANSPORT_STDIO,
};
use conduit::daemon::events::Event;
use conduit::daemon::upstream::http;
use conduit::daemon::upstream::probe::{
    self, DraftEnvEntry, DraftServerConfig, DraftTransport, HttpAuth,
};
use zeroize::Zeroizing;

use crate::window::refresh::{refresh_server_detail, refresh_servers, toggle_server_for_ide};
use crate::window::util::unix_now;
use crate::window::view::{MainWindow, ToolPolicyRow as ToolPolicyUiRow};
use crate::window::wizard::{bearer_from_headers, env_field_from_server, picker_from_lifecycle};
use crate::window::CallbackContext;

#[expect(clippy::too_many_lines, reason = "wires every server-tab callback")]
pub(crate) fn install_server_callbacks(window: &MainWindow, ctx: &CallbackContext) {
    let db = &ctx.db;
    let approvals = &ctx.approvals;
    let bus = &ctx.bus;
    let port = ctx.port;
    let rt = &ctx.rt;

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

    let approvals_for_always = approvals.clone();
    let db_for_always = db.clone();
    let rt_for_always = rt.clone();
    window.on_approve_always_clicked(move |id, server, tool| {
        let approvals = approvals_for_always.clone();
        let db = db_for_always.clone();
        let (id, server, tool) = (id.to_string(), server.to_string(), tool.to_string());
        rt_for_always.spawn(async move {
            if let Err(e) = approvals.resolve(&id, ApprovalStatus::Approved).await {
                tracing::warn!(error = %e, "approve always resolve failed");
                return;
            }
            if let Err(e) = db
                .set_tool_decision(&server, &tool, Decision::Allow, DecisionSource::User)
                .await
            {
                tracing::warn!(error = %e, server = %server, tool = %tool, "approve always policy upgrade failed");
            }
        });
    });

    let db_for_remove_server = db.clone();
    let bus_for_remove_server = bus.clone();
    let upstream_for_remove = ctx.upstream.clone();
    let rt_for_remove_server = rt.clone();
    let weak_for_remove_server = window.as_weak();
    let port_for_remove = port;
    window.on_remove_server_clicked(move |name| {
        let db = db_for_remove_server.clone();
        let bus = bus_for_remove_server.clone();
        let upstream = upstream_for_remove.clone();
        let weak = weak_for_remove_server.clone();
        let name = name.to_string();
        rt_for_remove_server.spawn(async move {
            match cascade::delete_server_with_cascade(
                &db,
                &bus,
                &upstream,
                &name,
                port_for_remove,
                false,
            )
            .await
            {
                Ok(report) => {
                    tracing::info!(
                        server = %name,
                        rewritten_clients = report.rewritten_clients.len(),
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

        // Runs on the UI thread: flip the open detail's badge to "probing" before
        // the async probe starts. `refresh_server_detail` clears it on completion.
        if let Some(ui) = weak.upgrade() {
            let mut detail = ui.get_selected_server_detail();
            detail.health_state = "probing".into();
            ui.set_selected_server_detail(detail);
        }

        rt_for_retest.spawn(async move {
            let Ok(Some(mut server)) = db.get_server(&name).await else {
                refresh_server_detail(&weak, &db, &name).await;
                return;
            };

            // Build the probe draft using the stored env/headers, mirroring how
            // the edit path reconstructs them — so stdio servers probe with their
            // real API-key env vars and HTTP servers probe with their bearer token.
            let cancel = CancellationToken::new();
            let now = unix_now();

            // For HTTP OAuth servers (env holds no plain bearer token), connect
            // via the keyring-backed store instead of starting a new OAuth login.
            // This avoids opening the browser and discarding fresh tokens.
            let is_oauth_http = server.transport == TRANSPORT_HTTP && {
                // If there is no plain bearer value, treat as OAuth.
                let has_bearer = server.env.iter().any(|e| {
                    e.name == http::BEARER_TOKEN_ENV
                        && matches!(e.value, EnvValue::Plain(_))
                });
                !has_bearer
            };

            if is_oauth_http {
                let url = server.url.clone().unwrap_or_default();
                let probe_result = http::connect_oauth(&url, &name, cancel.clone()).await;
                match probe_result {
                    Ok(Some(_client)) => {
                        // OAuth connection succeeded; record a successful probe
                        // timestamp (no tools re-enumerated on retest via OAuth to
                        // keep this path simple — the routing path enumerates tools
                        // on connect anyway).
                        server.last_probed_at = Some(now);
                        server.last_probe_error = None;
                        if let Err(e) = db.upsert_server(server).await {
                            tracing::warn!(error = %e, "re-test oauth upsert failed");
                        }
                        match db.clear_server_auth_error(&name).await {
                            Ok(true) => bus.publish(Event::AuthCleared { server: name.clone() }),
                            Ok(false) => {}
                            Err(e) => tracing::warn!(error = %e, "clear_server_auth_error after oauth retest failed"),
                        }
                    }
                    Ok(None) => {
                        // No stored credentials — mark as needing auth.
                        server.last_probed_at = Some(now);
                        server.last_probe_error = Some("No OAuth credentials stored; re-add the server to sign in.".to_string());
                        if let Err(e) = db.upsert_server(server).await {
                            tracing::warn!(error = %e, "re-test no-oauth upsert failed");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, server = %name, "re-test oauth probe failed");
                        server.last_probed_at = Some(now);
                        server.last_probe_error = Some(e.to_string());
                        if let Err(e) = db.upsert_server(server).await {
                            tracing::warn!(error = %e, "re-test oauth error upsert failed");
                        }
                    }
                }
                refresh_server_detail(&weak, &db, &name).await;
                refresh_servers(&weak, &db).await;
                return;
            }

            // Non-OAuth path: reconstruct the env/headers from the stored server row.
            let env_field = env_field_from_server(&server);
            let env_pairs: Vec<(String, String)> = env_field
                .split(',')
                .filter_map(|chunk| {
                    let chunk = chunk.trim();
                    if chunk.is_empty() { return None; }
                    let (k, v) = chunk.split_once('=')?;
                    Some((k.trim().to_string(), v.trim().to_string()))
                })
                .collect();

            // Also resolve any Secret-ref env entries (blocking keyring call on
            // the worker thread — never on the UI thread).
            let resolved_env: Vec<DraftEnvEntry> = if server.transport == TRANSPORT_STDIO {
                let mut entries = Vec::new();
                for env_entry in &server.env {
                    let env_key = env_entry.name.clone();
                    let value = match &env_entry.value {
                        EnvValue::Plain(v) => Zeroizing::new(v.clone()),
                        EnvValue::Secret(uuid) => {
                            let server_name = name.clone();
                            let uuid_owned = uuid.clone();
                            match tokio::task::spawn_blocking(move || {
                                conduit::daemon::secrets::fetch(&server_name, &uuid_owned)
                            })
                            .await
                            {
                                Ok(Ok(v)) => Zeroizing::new(v),
                                Ok(Err(e)) => {
                                    tracing::warn!(error = %e, key = %env_key, "secret fetch failed for retest; skipping");
                                    continue;
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, key = %env_key, "secret fetch join failed for retest; skipping");
                                    continue;
                                }
                            }
                        }
                    };
                    entries.push(DraftEnvEntry {
                        name: env_entry.name.clone(),
                        value,
                        is_secret: matches!(env_entry.value, EnvValue::Secret(_)),
                    });
                }
                entries
            } else {
                Vec::new()
            };

            let draft = if server.transport == TRANSPORT_HTTP {
                let bearer = bearer_from_headers(&env_pairs);
                DraftServerConfig {
                    name: name.clone(),
                    transport: DraftTransport::Http {
                        url: server.url.clone().unwrap_or_default(),
                        auth: bearer
                            .map_or(HttpAuth::None, |t| HttpAuth::Bearer(Zeroizing::new(t))),
                        headers: env_pairs,
                    },
                    lifecycle_seconds: server.lifecycle_seconds,
                }
            } else {
                DraftServerConfig {
                    name: name.clone(),
                    transport: DraftTransport::Stdio {
                        command: server.command.clone().unwrap_or_default(),
                        args: server.args.clone(),
                        env: resolved_env,
                    },
                    lifecycle_seconds: server.lifecycle_seconds,
                }
            };

            match probe::probe(&draft, cancel).await {
                Ok(result) => {
                    let tools: Vec<DbMcpTool> = result
                        .tools
                        .into_iter()
                        .map(|t| DbMcpTool {
                            server: server.name.clone(),
                            name: t.name,
                            description: t.description,
                            input_schema_json: t.input_schema.to_string(),
                            discovered_at: now,
                            decision: None,
                        })
                        .collect();
                    server.last_probed_at = Some(now);
                    server.last_probe_error = None;
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
                Err(e) => {
                    tracing::warn!(error = %e, server = %name, "re-test probe failed");
                    server.last_probed_at = Some(now);
                    server.last_probe_error = Some(e.to_string());
                    if let Err(e) = db.upsert_server(server).await {
                        tracing::warn!(error = %e, "re-test error upsert failed");
                    }
                    refresh_server_detail(&weak, &db, &name).await;
                    refresh_servers(&weak, &db).await;
                }
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
                            .set_tool_decision(&server, &t.name, decision, DecisionSource::User)
                            .await
                        {
                            tracing::warn!(error = %e, "set tool decision failed");
                        }
                    }
                }
            } else if let Err(e) = db
                .set_tool_decision(&server, &tool, decision, DecisionSource::User)
                .await
            {
                tracing::warn!(error = %e, "set tool decision failed");
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
        if !text.is_empty() {
            crate::window::util::copy_to_clipboard(&text);
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
            let env_field = env_field_from_server(&server);
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
                ui.invoke_populate_env_from_string(SharedString::from(env_field));
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
