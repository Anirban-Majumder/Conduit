//! Repaints Slint list models and detail panes from the DB.
//!
//! `push_initial_models` seeds the window at startup; `spawn_event_bridge`
//! subscribes to the event bus and re-runs the relevant `refresh_*` on each
//! event. Every `refresh_*` is async (DB-actor calls) and writes UI state only
//! through `upgrade_in_event_loop`, so all are safe to call from worker tasks.

use std::sync::Arc;

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use tokio_util::sync::CancellationToken;

use conduit::daemon::client_config;
use conduit::daemon::config::toml::Settings;
use conduit::daemon::db::types::{AuditDecision, AuditFilter, TRANSPORT_STDIO};
use conduit::daemon::db::Db;
use conduit::daemon::events::Event;
use conduit::daemon::skills::SkillManager;

use crate::window::view::{
    ApprovalRow, AuditRow, DashboardStats, IdeRow, IdeTarget, MainWindow, PolicyRow, ScopeRow,
    ServerDetail, ServerRow, ToolPolicyRow as ToolPolicyUiRow,
};

use crate::window::skills::{installed_skills_to_rows, refresh_skills};
use crate::window::util::{
    clock_full, display_scope, fire_auth_failure_notification, lifecycle_label, read_ui_state,
    relative_time, run_ide_detection, settings_to_row, unix_now,
};

/// Decode an IDE's embedded brand SVG into a `slint::Image`. Falls back to an
/// empty image if the bytes fail to parse. Must run on the UI thread (`Image`
/// is not `Send`).
pub(crate) fn ide_icon(client_id: &str) -> slint::Image {
    client_config::builtin_by_id(client_id)
        .map(|d| slint::Image::load_from_svg_data(d.icon_svg).unwrap_or_default())
        .unwrap_or_default()
}

pub(crate) fn push_initial_models(
    window: &MainWindow,
    db: &Db,
    settings: &Settings,
    rt: tokio::runtime::Handle,
    skills: Arc<SkillManager>,
) {
    window.set_settings(settings_to_row(settings));

    let weak = window.as_weak();
    let db_for_refresh = db.clone();
    let weak_for_skills = weak.clone();
    let port = settings.daemon.port;
    rt.spawn(async move {
        skills.purge_tmp().await;
        run_ide_detection(&db_for_refresh).await;
        // Repair any DB↔on-disk config divergence from an interrupted cascade
        // before the first paint reads the (now-authoritative) DB state.
        if let Err(e) = client_config::project::reconcile_configs(&db_for_refresh, port).await {
            tracing::warn!(error = %e, "startup config reconcile failed");
        }
        refresh_all(&weak, &db_for_refresh).await;
        // Before refresh_skills — that runs slow network discovery and would
        // otherwise delay the dashboard's first paint.
        refresh_dashboard(&weak, &db_for_refresh, &skills).await;
        refresh_skills(&weak_for_skills, &skills).await;
        let auto = db_for_refresh
            .get_settings_kv("open_window_at_startup")
            .await
            .ok()
            .flatten()
            .as_deref()
            == Some("true");
        let default_policy = db_for_refresh
            .get_settings_kv("default_unknown_tool_policy")
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| "ask".to_string());
        let _ignored = weak.upgrade_in_event_loop(move |ui| {
            ui.set_open_window_at_startup(auto);
            ui.set_default_unknown_tool_policy(SharedString::from(default_policy));
        });
    });
}

pub(crate) fn spawn_event_bridge(
    window: &MainWindow,
    mut rx: tokio::sync::broadcast::Receiver<Event>,
    db: Db,
    rt: tokio::runtime::Handle,
    skills: Arc<SkillManager>,
    cancel: CancellationToken,
) {
    let weak = window.as_weak();
    rt.spawn(async move {
        // Dirty flag: set when AuditAppended arrives; cleared after the coalesced
        // refresh fires. At most one audit query + UI repaint per ~300ms burst.
        let mut audit_dirty = false;
        let mut coalesce = tokio::time::interval(std::time::Duration::from_millis(300));
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                event = rx.recv() => {
                    match event {
                        Ok(Event::ApprovalPending { .. }) => {
                            refresh_pending(&weak, &db).await;
                        }
                        Ok(Event::ApprovalResolved { .. }) => {
                            refresh_pending(&weak, &db).await;
                            refresh_audit(&weak, &db).await;
                            refresh_dashboard(&weak, &db, &skills).await;
                        }
                        Ok(Event::AuditAppended) => {
                            audit_dirty = true;
                        }
                        Ok(Event::ConfigReloaded { settings }) => {
                            let row = settings_to_row(&settings);
                            weak.upgrade_in_event_loop(move |ui| ui.set_settings(row))
                                .ok();
                        }
                        Ok(Event::ServerHealthChanged { name }) => {
                            tracing::debug!(server = %name, "health changed; consumer to refresh");
                            refresh_servers(&weak, &db).await;
                            refresh_dashboard(&weak, &db, &skills).await;
                        }
                        Ok(Event::FocusApproval { id }) => {
                            let id_for_ui = id.clone();
                            weak.upgrade_in_event_loop(move |ui| {
                                ui.window().show().ok();
                                ui.set_focused_approval(SharedString::from(id_for_ui));
                            })
                            .ok();
                        }
                        Ok(Event::SkillsChanged) => {
                            refresh_skills(&weak, &skills).await;
                            refresh_dashboard(&weak, &db, &skills).await;
                        }
                        Ok(Event::AuthFailure { server, message }) => {
                            tracing::warn!(server = %server, message = %message, "auth failure");
                            fire_auth_failure_notification(&server, &message);
                            refresh_servers(&weak, &db).await;
                            refresh_server_detail(&weak, &db, &server).await;
                            refresh_dashboard(&weak, &db, &skills).await;
                        }
                        Ok(Event::AuthCleared { server }) => {
                            tracing::info!(server = %server, "auth cleared");
                            refresh_servers(&weak, &db).await;
                            refresh_server_detail(&weak, &db, &server).await;
                            refresh_dashboard(&weak, &db, &skills).await;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(skipped = n, "event bridge lagged");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
                _ = coalesce.tick() => {
                    if audit_dirty {
                        audit_dirty = false;
                        refresh_audit(&weak, &db).await;
                        refresh_dashboard(&weak, &db, &skills).await;
                    }
                }
            }
        }
    });
}

pub(crate) async fn refresh_all(weak: &slint::Weak<MainWindow>, db: &Db) {
    refresh_pending(weak, db).await;
    refresh_servers(weak, db).await;
    refresh_ides(weak, db).await;
    refresh_policies(weak, db).await;
    refresh_audit(weak, db).await;
}

pub(crate) async fn refresh_pending(weak: &slint::Weak<MainWindow>, db: &Db) {
    let pending = db.list_pending_approvals().await.unwrap_or_default();
    let ide_names =
        lookup_ide_names_for_keys(db, pending.iter().map(|p| p.client_key.as_str())).await;
    let now = unix_now();
    let rows: Vec<ApprovalRow> = pending
        .into_iter()
        .map(|p| ApprovalRow {
            id: SharedString::from(p.id),
            ide_name: SharedString::from(
                ide_names
                    .get(&p.client_key)
                    .cloned()
                    .unwrap_or_else(|| "?".to_string()),
            ),
            server: SharedString::from(p.mcp_server),
            tool: SharedString::from(p.tool),
            created_at: SharedString::from(relative_time(now - p.created_at)),
            args_json: SharedString::from(p.args_json),
        })
        .collect();
    weak.upgrade_in_event_loop(move |ui| {
        let still_pending = rows.iter().any(|r| r.id == ui.get_selected_approval().id);
        ui.set_approvals(ModelRc::new(VecModel::from(rows)));
        // Close the detail modal if the approval it showed is no longer pending.
        if ui.get_show_approval_detail() && !still_pending {
            ui.set_show_approval_detail(false);
        }
    })
    .ok();
}

pub(crate) async fn refresh_servers(weak: &slint::Weak<MainWindow>, db: &Db) {
    let servers = db.list_servers().await.unwrap_or_default();
    let mut tool_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for t in db.list_all_tools().await.unwrap_or_default() {
        *tool_counts.entry(t.server).or_default() += 1;
    }
    let mut rows: Vec<ServerRow> = Vec::with_capacity(servers.len());
    for s in servers {
        let tool_count = tool_counts.get(&s.name).copied().unwrap_or(0);
        let count_label = if tool_count == 0 {
            "no tools".to_string()
        } else {
            format!("{tool_count} tools")
        };
        let auth_failure = s.last_auth_error_msg.is_some();
        rows.push(ServerRow {
            name: SharedString::from(s.name),
            transport: SharedString::from(s.transport),
            lifecycle_label: SharedString::from(lifecycle_label(s.lifecycle_seconds)),
            status: SharedString::from("idle"),
            tool_count_label: SharedString::from(count_label),
            auth_failure,
        });
    }
    weak.upgrade_in_event_loop(move |ui| {
        ui.set_servers(ModelRc::new(VecModel::from(rows)));
    })
    .ok();
}

pub(crate) async fn refresh_server_detail(weak: &slint::Weak<MainWindow>, db: &Db, name: &str) {
    let Ok(Some(server)) = db.get_server(name).await else {
        let n = name.to_string();
        let _ignored = weak.upgrade_in_event_loop(move |ui| {
            if ui.get_selected_server().as_str() == n {
                ui.set_selected_server(SharedString::from(""));
                ui.set_selected_server_tools(ModelRc::new(VecModel::from(
                    Vec::<ToolPolicyUiRow>::new(),
                )));
            }
        });
        return;
    };

    let tools = db.list_server_tools(name).await.unwrap_or_default();
    let mut tool_rows: Vec<ToolPolicyUiRow> = Vec::with_capacity(tools.len());
    for t in &tools {
        let decision = t
            .decision
            .map_or_else(|| "ask".to_string(), |d| d.as_db_str().to_string());
        let description = t.description.clone().unwrap_or_default();
        let summary = description.lines().next().unwrap_or_default().to_string();
        tool_rows.push(ToolPolicyUiRow {
            name: SharedString::from(t.name.clone()),
            description: SharedString::from(description),
            summary: SharedString::from(summary),
            decision: SharedString::from(decision),
        });
    }

    let now = unix_now();
    let probed_ts = server.last_probed_at;
    let probe_error = server.last_probe_error.clone().unwrap_or_default();
    let health_state = if !probe_error.is_empty() {
        "down"
    } else if probed_ts.is_some() {
        "live"
    } else if tools.is_empty() {
        "unprobed"
    } else {
        "idle"
    };
    let last_probe_label = probed_ts.map_or_else(|| "—".to_string(), |ts| relative_time(now - ts));
    let version_label = server.reported_version.clone().unwrap_or_default();
    let detail = ServerDetail {
        name: SharedString::from(server.name.clone()),
        version: SharedString::from(version_label),
        transport: SharedString::from(server.transport.clone()),
        command_or_url: SharedString::from(if server.transport == TRANSPORT_STDIO {
            let cmd = server.command.clone().unwrap_or_default();
            if server.args.is_empty() {
                cmd
            } else {
                format!("{cmd} {}", server.args.join(" "))
            }
        } else {
            server.url.clone().unwrap_or_default()
        }),
        lifecycle_label: SharedString::from(lifecycle_label(server.lifecycle_seconds)),
        health_state: SharedString::from(health_state),
        last_probe_label: SharedString::from(last_probe_label),
        last_error: SharedString::from(probe_error),
        tool_count_label: SharedString::from(format!("{} tools", tool_rows.len())),
        auth_failure_msg: SharedString::from(
            server.last_auth_error_msg.clone().unwrap_or_default(),
        ),
        latency_label: SharedString::from(String::new()),
    };

    let name_owned = name.to_string();
    let _ignored = weak.upgrade_in_event_loop(move |ui| {
        ui.set_selected_server(SharedString::from(name_owned));
        ui.set_selected_server_detail(detail);
        ui.set_selected_server_tools(ModelRc::new(VecModel::from(tool_rows)));
    });

    refresh_server_targets(weak, db, name).await;
}

pub(crate) async fn refresh_server_targets(
    weak: &slint::Weak<MainWindow>,
    db: &Db,
    server_name: &str,
) {
    let actives = db.list_tracked_clients().await.unwrap_or_default();

    // Clients that manage this server in ANY scope (global or a repo): the icon
    // lights up if the server is bound anywhere for that editor, not just global.
    let managing: std::collections::HashSet<String> = db
        .list_clients_managing_server(server_name)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|(client_id, _scope)| client_id)
        .collect();

    // Build `IdeTarget`s (incl. brand-icon `Image`s) on the UI thread — `Image`
    // is !Send.
    let _ignored = weak.upgrade_in_event_loop(move |ui| {
        let rows: Vec<IdeTarget> = actives
            .iter()
            .map(|client| IdeTarget {
                ide_kind: SharedString::from(client.id.clone()),
                label: SharedString::from(client.name.clone()),
                icon: ide_icon(&client.id),
                active: managing.contains(&client.id),
            })
            .collect();
        ui.set_selected_server_targets(ModelRc::new(VecModel::from(rows)));
    });
}

/// Populate the scope popover for `(item, kind, editor)` and open it. Called on
/// open and re-called after each mutation so the toggle states stay live.
pub(crate) async fn refresh_scope_popover(
    weak: &slint::Weak<MainWindow>,
    db: &Db,
    item: &str,
    kind: &str,
    editor: &str,
) {
    let editor_label = db
        .get_client(editor)
        .await
        .ok()
        .flatten()
        .map_or_else(|| editor.to_string(), |c| c.name);

    // Scopes `item` is currently in for `editor`.
    let member: std::collections::HashSet<String> = if kind == "skill" {
        db.list_skill_targets_scoped(item).await.unwrap_or_default()
    } else {
        db.list_clients_managing_server(item)
            .await
            .unwrap_or_default()
    }
    .into_iter()
    .filter(|(c, _)| c == editor)
    .map(|(_, s)| s)
    .collect();

    // For skills, only an editor with a repo skills dir can host repo scopes.
    let editor_supports_repo = kind != "skill"
        || client_config::resolve_client(db, editor)
            .await
            .ok()
            .flatten()
            .is_some_and(|c| c.project_skills_rel.is_some());

    let mut rows: Vec<ScopeRow> = vec![ScopeRow {
        label: SharedString::from("Global"),
        scope_key: SharedString::from("global"),
        active: member.contains("global"),
        removable: false,
    }];
    if editor_supports_repo {
        for repo in db.list_known_repos().await.unwrap_or_default() {
            rows.push(ScopeRow {
                label: SharedString::from(display_scope(&repo)),
                active: member.contains(&repo),
                scope_key: SharedString::from(repo),
                removable: true,
            });
        }
    }

    let (item, kind, editor) = (item.to_string(), kind.to_string(), editor.to_string());
    let _ignored = weak.upgrade_in_event_loop(move |ui| {
        ui.set_scope_popover_editor(SharedString::from(editor));
        ui.set_scope_popover_editor_label(SharedString::from(editor_label));
        ui.set_scope_popover_item(SharedString::from(item.clone()));
        ui.set_scope_popover_item_label(SharedString::from(item));
        ui.set_scope_popover_item_kind(SharedString::from(kind));
        ui.set_scope_popover_scopes(ModelRc::new(VecModel::from(rows)));
        ui.set_scope_popover_open(true);
    });
}

/// Add/remove a server from a client's managed set, then re-derive its config
/// file from the resulting set (the one renderer, no per-IDE branching).
pub(crate) async fn toggle_server_for_ide(
    db: &Db,
    port: u16,
    server_name: &str,
    client_id: &str,
    enable: bool,
) {
    let Some(client) = client_config::resolve_client(db, client_id)
        .await
        .ok()
        .flatten()
    else {
        tracing::warn!(client_id = %client_id, "no client row for id");
        return;
    };
    if !client.is_builtin {
        tracing::warn!(client_id = %client_id, "custom client config is user-authored; ignoring toggle");
        return;
    }

    let mut entries = db
        .list_managed_servers(client_id, "global")
        .await
        .unwrap_or_default();
    let present = entries.iter().any(|n| n == server_name);
    if enable && !present {
        entries.push(server_name.to_string());
    } else if !enable && present {
        entries.retain(|n| n != server_name);
    }

    if let Err(e) = db
        .set_managed_servers(client_id, "global", entries.clone())
        .await
    {
        tracing::warn!(error = %e, "set_managed_servers failed");
        return;
    }

    // render_mcp makes the file hold exactly `entries` (purge ours + render).
    let render =
        tokio::task::spawn_blocking(move || client_config::render_mcp(&client, &entries, port))
            .await;
    match render {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(error = %e, client_id, "render failed"),
        Err(e) => tracing::warn!(error = %e, client_id, "render join failed"),
    }
}

pub(crate) async fn refresh_ides(weak: &slint::Weak<MainWindow>, db: &Db) {
    let records = db.list_clients().await.unwrap_or_default();
    // Build rows (and decode their SVG icons) on the UI thread: `slint::Image`
    // is not `Send`, so it can't be constructed before `upgrade_in_event_loop`.
    weak.upgrade_in_event_loop(move |ui| {
        let rows: Vec<IdeRow> = records
            .into_iter()
            .map(|r| {
                let icon = ide_icon(&r.id);
                let status_label = if r.tracked { "active" } else { "not_tracked" };
                IdeRow {
                    icon,
                    kind: SharedString::from(r.id),
                    display_name: SharedString::from(r.name),
                    key: SharedString::from(r.mcp_key),
                    status: SharedString::from(status_label),
                    config_path: SharedString::from(r.mcp_config_path),
                }
            })
            .collect();
        ui.set_ides(ModelRc::new(VecModel::from(rows)));
    })
    .ok();
}

pub(crate) async fn refresh_policies(weak: &slint::Weak<MainWindow>, db: &Db) {
    // list_all_policies is removed; collect tool decisions in one batched query.
    let tools = db.list_all_tools().await.unwrap_or_default();
    let mut rows: Vec<PolicyRow> = Vec::new();
    for t in tools {
        if let Some(decision) = t.decision {
            rows.push(PolicyRow {
                server: SharedString::from(t.server.clone()),
                tool: SharedString::from(t.name),
                decision: SharedString::from(decision.as_db_str()),
                source: SharedString::from(""), // decision source not tracked per-tool anymore
            });
        }
    }
    weak.upgrade_in_event_loop(move |ui| {
        ui.set_policies(ModelRc::new(VecModel::from(rows)));
    })
    .ok();
}

pub(crate) async fn refresh_audit(weak: &slint::Weak<MainWindow>, db: &Db) {
    // Read the live filter state from the UI thread before querying.
    let (search, decision_str) = read_ui_state(weak, |ui| {
        (
            ui.get_audit_search_query().to_string(),
            ui.get_audit_decision_filter().to_string(),
        )
    })
    .await
    .unwrap_or_default();

    let decision = if decision_str.is_empty() {
        None
    } else {
        let d = AuditDecision::from_db_str(&decision_str);
        if d.is_none() {
            tracing::warn!(value = %decision_str, "unknown audit decision filter; showing all");
        }
        d
    };
    let text = if search.is_empty() {
        None
    } else {
        Some(search)
    };

    let filter = AuditFilter {
        limit: Some(25),
        decision,
        text,
        ..Default::default()
    };
    let entries = db.query_audit_log(filter).await.unwrap_or_default();
    let now = unix_now();
    let rows: Vec<AuditRow> = entries
        .into_iter()
        .map(|e| AuditRow {
            id: SharedString::from(e.id.map(|n| n.to_string()).unwrap_or_default()),
            ts_label: SharedString::from(relative_time(now - e.ts)),
            ts_full: SharedString::from(clock_full(e.ts)),
            ide_name: SharedString::from(e.client_name.unwrap_or_else(|| "?".to_string())),
            server: SharedString::from(e.mcp_server),
            tool: SharedString::from(e.tool.unwrap_or_default()),
            decision: SharedString::from(e.decision.as_db_str()),
            decision_source: SharedString::from(
                e.decision_source
                    .map(|s| s.as_db_str().to_string())
                    .unwrap_or_default(),
            ),
            latency_label: SharedString::from(
                e.latency_ms.map(|n| format!("{n}ms")).unwrap_or_default(),
            ),
            request_args: SharedString::from(e.request_args.unwrap_or_default()),
            response_status: SharedString::from(e.response_status.unwrap_or_default()),
            response_length: SharedString::from(
                e.response_length.map(|n| n.to_string()).unwrap_or_default(),
            ),
            error_code: SharedString::from(e.error_code.unwrap_or_default()),
        })
        .collect();
    weak.upgrade_in_event_loop(move |ui| {
        ui.set_audit(ModelRc::new(VecModel::from(rows)));
    })
    .ok();
}

pub(crate) async fn refresh_dashboard(
    weak: &slint::Weak<MainWindow>,
    db: &Db,
    skills: &SkillManager,
) {
    let servers = db.list_servers().await.unwrap_or_default();
    let server_count = servers.len();
    let server_auth_failures = servers
        .iter()
        .filter(|s| s.last_auth_error_msg.is_some())
        .count();
    let server_live = servers
        .iter()
        .filter(|s| {
            s.last_auth_error_msg.is_none()
                && s.last_probe_error.is_none()
                && s.last_probed_at.is_some()
        })
        .count();

    let editor_count = db.list_tracked_clients().await.unwrap_or_default().len();
    let installed_skills = skills.list_installed().await.unwrap_or_default();
    let skill_count = installed_skills.len();

    let since = unix_now() - 86_400;
    let recent = db
        .query_audit_log(AuditFilter {
            since_ts: Some(since),
            limit: Some(10_000),
            ..Default::default()
        })
        .await
        .unwrap_or_default();
    let calls_24h = recent.len();
    let denied_24h = recent
        .iter()
        .filter(|e| {
            matches!(
                e.decision,
                AuditDecision::Deny | AuditDecision::AskDenied | AuditDecision::AskTimeout
            )
        })
        .count();

    let to_i32 = |n: usize| i32::try_from(n).unwrap_or(i32::MAX);
    let stats = DashboardStats {
        server_count: to_i32(server_count),
        server_live: to_i32(server_live),
        server_auth_failures: to_i32(server_auth_failures),
        skill_count: to_i32(skill_count),
        editor_count: to_i32(editor_count),
        calls_24h: to_i32(calls_24h),
        denied_24h: to_i32(denied_24h),
    };
    weak.upgrade_in_event_loop(move |ui| {
        ui.set_dashboard_stats(stats);
        // Dashboard skill cards don't render IDE toggles, so no active kinds.
        let skill_rows = installed_skills_to_rows(&installed_skills, &[]);
        ui.set_dashboard_skills(ModelRc::new(VecModel::from(skill_rows)));
    })
    .ok();
}

pub(crate) async fn lookup_ide_names_for_keys<'a>(
    db: &Db,
    keys: impl Iterator<Item = &'a str>,
) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for key in keys {
        if !out.contains_key(key) {
            if let Ok(Some(client)) = db.lookup_client_key(key).await {
                out.insert(client.mcp_key, client.name);
            }
        }
    }
    out
}
