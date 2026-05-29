//! Add/edit-server wizard: probe an upstream, then save it with policies.
//!
//! Drives the multi-step Slint dialog: step 1 collects the form, `on_start_probe`
//! connects/initializes the upstream (streaming OAuth/phase progress to the UI),
//! and `on_wizard_save` promotes draft OAuth tokens to the canonical server name
//! and persists. `PendingWizard` (behind a `std::sync::Mutex`) stages the probed
//! server between steps. The `*_field` / `*_picker` helpers convert UI strings to
//! domain types.

use std::sync::Arc;

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use tokio_util::sync::CancellationToken;

use conduit::daemon::db::types::{
    Decision, DecisionSource, EnvEntry, EnvValue, Server, ToolMeta as DbToolMeta, TRANSPORT_HTTP,
    TRANSPORT_STDIO,
};
use conduit::daemon::db::Db;
use conduit::daemon::events::{Bus, Event};
use conduit::daemon::onboarding::mark_first_run_completed;
use conduit::daemon::server_validators;
use conduit::daemon::upstream::probe::{self, DraftServerConfig, DraftTransport, HttpAuth};

use crate::ui::{MainWindow, ToolPolicyRow as ToolPolicyUiRow};

use crate::glue::refresh::refresh_servers;
use crate::glue::util::unix_now;

#[derive(Default, Clone)]
struct PendingWizard {
    server: Option<Server>,
    tools: Vec<DbToolMeta>,
    descriptions: std::collections::HashMap<String, String>,
    oauth_session: Option<String>,
}

#[expect(
    clippy::too_many_lines,
    reason = "wizard wires 4 Slint callbacks in one place"
)]
pub(crate) fn install_wizard_callbacks(
    window: &MainWindow,
    db: Db,
    bus: Bus,
    rt: tokio::runtime::Handle,
) {
    let pending: Arc<std::sync::Mutex<PendingWizard>> =
        Arc::new(std::sync::Mutex::new(PendingWizard::default()));

    let weak = window.as_weak();
    let db_for_probe = db.clone();
    let rt_for_probe = rt.clone();
    let pending_for_probe = pending.clone();
    window.on_start_probe(move || {
        let Some(ui) = weak.upgrade() else { return };
        let name = ui.get_add_server_name().to_string();
        if !server_validators::is_valid_server_name(&name) {
            ui.set_add_server_status(SharedString::from("Name must match [a-z0-9_-]{1,64}."));
            ui.set_add_server_status_is_error(true);
            return;
        }
        let transport = ui.get_add_server_transport().to_string();
        let command = ui.get_add_server_command().to_string();
        let args_raw = ui.get_add_server_args().to_string();
        let url = ui.get_add_server_url().to_string();
        let headers_raw = ui.get_add_server_headers().to_string();
        let lifecycle_mode = ui.get_add_server_lifecycle_mode().to_string();
        let lifecycle_custom = ui.get_add_server_lifecycle_custom().to_string();
        let lifecycle_seconds = lifecycle_from_picker(&lifecycle_mode, &lifecycle_custom);
        let args: Vec<String> = if args_raw.is_empty() {
            Vec::new()
        } else {
            args_raw.split_whitespace().map(str::to_string).collect()
        };

        ui.set_wizard_step(2);
        ui.set_wizard_phase_status(SharedString::from("Connecting + initializing..."));
        ui.set_wizard_oauth_url(SharedString::from(""));
        ui.set_add_server_status(SharedString::from(""));
        ui.set_add_server_status_is_error(false);
        ui.set_add_server_busy(true);

        let draft = build_draft(
            &name,
            &transport,
            &command,
            args.clone(),
            &url,
            &headers_raw,
            lifecycle_seconds,
        );
        let env: Vec<EnvEntry> = if transport == TRANSPORT_HTTP {
            bearer_from_headers(&parse_headers_field(&headers_raw))
                .map(|token| {
                    vec![EnvEntry {
                        name: conduit::daemon::upstream::http::BEARER_TOKEN_ENV.to_string(),
                        value: EnvValue::Plain(token),
                    }]
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let server_row = Server {
            name: name.clone(),
            transport: transport.clone(),
            command: if transport == TRANSPORT_STDIO && !command.is_empty() {
                Some(command)
            } else {
                None
            },
            args,
            env,
            url: if transport == TRANSPORT_HTTP && !url.is_empty() {
                Some(url)
            } else {
                None
            },
            lifecycle_seconds,
            last_init_duration_ms: None,
            package_version: None,
            mcp_self_version: None,
            created_at: 0,
            server_name: None,
            server_version: None,
            last_probed_at: None,
            last_probe_error: None,
            last_auth_error_msg: None,
            last_auth_error_at: None,
        };

        let weak = weak.clone();
        let pending = pending_for_probe.clone();
        let db = db_for_probe.clone();
        rt_for_probe.spawn(async move {
            let cancel = CancellationToken::new();
            let weak_for_progress = weak.clone();
            let progress: probe::ProgressSink = Arc::new(move |evt| match evt {
                probe::ProbeEvent::Phase(msg) => {
                    let weak = weak_for_progress.clone();
                    let _ignored = weak.upgrade_in_event_loop(move |ui| {
                        ui.set_wizard_phase_status(SharedString::from(msg));
                    });
                }
                probe::ProbeEvent::AuthorizeUrl(url) => {
                    let weak = weak_for_progress.clone();
                    let _ignored = weak.upgrade_in_event_loop(move |ui| {
                        ui.set_wizard_oauth_url(SharedString::from(url));
                        ui.set_wizard_phase_status(SharedString::from(
                            "Waiting for sign-in in your browser...",
                        ));
                    });
                }
            });
            let result = probe::probe_with_progress(&draft, cancel, Some(progress)).await;
            match result {
                Ok(probed) => {
                    let now = unix_now();
                    let mut descriptions = std::collections::HashMap::new();
                    let tools: Vec<DbToolMeta> = probed
                        .tools
                        .into_iter()
                        .map(|t| {
                            if let Some(d) = &t.description {
                                descriptions.insert(t.name.clone(), d.clone());
                            }
                            DbToolMeta {
                                name: t.name,
                                description: t.description,
                                input_schema_json: t.input_schema.to_string(),
                                discovered_at: now,
                            }
                        })
                        .collect();
                    let tool_rows: Vec<ToolPolicyUiRow> = tools
                        .iter()
                        .map(|t| ToolPolicyUiRow {
                            name: SharedString::from(t.name.clone()),
                            description: SharedString::from(
                                t.description.clone().unwrap_or_default(),
                            ),
                            decision: SharedString::from("ask"),
                        })
                        .collect();
                    let mut updated_server = server_row;
                    updated_server.server_name = Some(probed.server_name.clone());
                    updated_server.server_version = Some(probed.server_version.clone());
                    updated_server.last_probed_at = Some(now);
                    updated_server.last_probe_error = None;
                    let draft_for_tracking = probed.oauth_session.clone();
                    if let Ok(mut guard) = pending.lock() {
                        guard.server = Some(updated_server);
                        guard.tools = tools;
                        guard.descriptions = descriptions;
                        guard.oauth_session = probed.oauth_session;
                    }
                    // Track the draft in DB so startup can purge abandoned drafts.
                    if let Some(session) = draft_for_tracking {
                        if let Err(e) = db.insert_oauth_draft(session).await {
                            tracing::debug!(error = %e, "track oauth draft failed");
                        }
                    }
                    let _ignored = weak.upgrade_in_event_loop(move |ui| {
                        ui.set_add_server_busy(false);
                        ui.set_wizard_step(3);
                        ui.set_wizard_phase_status(SharedString::from(""));
                        ui.set_wizard_oauth_url(SharedString::from(""));
                        ui.set_wizard_tools(ModelRc::new(VecModel::from(tool_rows)));
                    });
                    let _ = (name, db);
                }
                Err(e) => {
                    let _ignored = weak.upgrade_in_event_loop(move |ui| {
                        ui.set_add_server_busy(false);
                        ui.set_wizard_step(1);
                        ui.set_wizard_phase_status(SharedString::from(""));
                        ui.set_wizard_oauth_url(SharedString::from(""));
                        ui.set_add_server_status(SharedString::from(format!("Probe failed: {e}")));
                        ui.set_add_server_status_is_error(true);
                    });
                }
            }
        });
    });

    let weak = window.as_weak();
    let db_for_save = db;
    let bus_for_save = bus;
    let rt_for_save = rt;
    let pending_for_save = pending;
    window.on_wizard_save(move || {
        let Some(ui) = weak.upgrade() else { return };
        let staged = match pending_for_save.lock() {
            Ok(g) => g.clone(),
            Err(_) => return,
        };
        let Some(server_row) = staged.server else {
            ui.set_add_server_status(SharedString::from(
                "No probed server staged. Click Test connection first.",
            ));
            ui.set_add_server_status_is_error(true);
            return;
        };

        let tool_rows_model = ui.get_wizard_tools();
        let mut policy_overrides: std::collections::HashMap<String, Decision> =
            std::collections::HashMap::new();
        for i in 0..tool_rows_model.row_count() {
            if let Some(row) = tool_rows_model.row_data(i) {
                if let Some(d) = Decision::from_db_str(row.decision.as_str()) {
                    policy_overrides.insert(row.name.to_string(), d);
                }
            }
        }

        let weak = weak.clone();
        let db = db_for_save.clone();
        let bus = bus_for_save.clone();
        let pending = pending_for_save.clone();
        let tools = staged.tools.clone();
        let name = server_row.name.clone();
        let oauth_session = staged.oauth_session.clone();
        let server_to_save = server_row;
        rt_for_save.spawn(async move {
            // Promote OAuth tokens from draft session → canonical server name.
            tracing::info!(
                server = %name,
                has_oauth_session = oauth_session.is_some(),
                "wizard save: starting OAuth token promotion",
            );
            if let Some(draft) = oauth_session.as_deref() {
                let draft = draft.to_string();
                let server = name.clone();
                match tokio::task::spawn_blocking(move || {
                    conduit::daemon::secrets::promote_oauth_tokens(&draft, &server)
                })
                .await
                {
                    Ok(Ok(())) => {
                        if let Some(ref session) = oauth_session {
                            if let Err(e) = db.delete_oauth_draft(session.clone()).await {
                                tracing::debug!(error = %e, "delete oauth draft after save failed");
                            }
                        }
                    }
                    Ok(Err(secrets_err)) => tracing::error!(
                        server = %name,
                        error = %secrets_err,
                        "oauth token promotion failed — server will save without working auth",
                    ),
                    Err(join_err) => tracing::warn!(
                        error = %join_err,
                        "oauth token promotion join failed",
                    ),
                }
            }
            let result = db.upsert_server_with_tools(server_to_save, tools).await;
            let outcome: Result<usize, String> = match result {
                Ok(()) => {
                    for (tool, decision) in &policy_overrides {
                        if let Err(e) = db
                            .set_policy(&name, tool, *decision, DecisionSource::User)
                            .await
                        {
                            tracing::warn!(error = %e, tool = %tool, "policy override failed");
                        }
                    }
                    if let Err(e) = mark_first_run_completed(&db).await {
                        tracing::warn!(error = %e, "mark_first_run_completed failed");
                    }
                    match db.clear_server_auth_error(&name).await {
                        Ok(true) => bus.publish(Event::AuthCleared {
                            server: name.clone(),
                        }),
                        Ok(false) => {}
                        Err(e) => {
                            tracing::warn!(error = %e, "clear_server_auth_error after save failed");
                        }
                    }
                    Ok(policy_overrides.len())
                }
                Err(e) => Err(format!("DB write failed: {e}")),
            };
            let _ignored = weak.upgrade_in_event_loop(move |ui| match outcome {
                Ok(n) => {
                    ui.set_show_add_server_dialog(false);
                    ui.set_wizard_step(1);
                    ui.set_add_server_status(SharedString::from(""));
                    if let Ok(mut g) = pending.lock() {
                        *g = PendingWizard::default();
                    }
                    tracing::info!(policies = n, "server saved");
                }
                Err(msg) => {
                    ui.set_add_server_status(SharedString::from(msg));
                    ui.set_add_server_status_is_error(true);
                }
            });
            refresh_servers(&weak, &db).await;
        });
    });

    let weak = window.as_weak();
    window.on_wizard_back_to_form(move || {
        let Some(ui) = weak.upgrade() else { return };
        ui.set_wizard_step(1);
        ui.set_wizard_phase_status(SharedString::from(""));
        ui.set_add_server_status(SharedString::from(""));
        ui.set_add_server_status_is_error(false);
    });

    let weak = window.as_weak();
    window.on_wizard_change_tool_policy(move |tool, decision| {
        let Some(ui) = weak.upgrade() else { return };
        let model = ui.get_wizard_tools();
        let tool = tool.to_string();
        let decision = decision.to_string();
        let mut updated: Vec<ToolPolicyUiRow> = Vec::with_capacity(model.row_count());
        for i in 0..model.row_count() {
            if let Some(mut row) = model.row_data(i) {
                if tool == "*" || row.name.as_str() == tool {
                    row.decision = SharedString::from(decision.clone());
                }
                updated.push(row);
            }
        }
        ui.set_wizard_tools(ModelRc::new(VecModel::from(updated)));
    });
}

pub(crate) fn build_draft(
    name: &str,
    transport: &str,
    command: &str,
    args: Vec<String>,
    url: &str,
    headers_raw: &str,
    lifecycle_seconds: i32,
) -> DraftServerConfig {
    let transport = if transport == TRANSPORT_HTTP {
        let headers = parse_headers_field(headers_raw);
        DraftTransport::Http {
            url: url.to_string(),
            auth: HttpAuth::None,
            headers,
        }
    } else {
        DraftTransport::Stdio {
            command: command.to_string(),
            args,
            env: vec![],
        }
    };
    DraftServerConfig {
        name: name.to_string(),
        transport,
        lifecycle_seconds,
        draft_session: uuid::Uuid::new_v4().to_string(),
    }
}

pub(crate) fn lifecycle_from_picker(mode: &str, custom: &str) -> i32 {
    match mode {
        "ephemeral" => 0,
        "persistent" => -1,
        "custom" => custom.trim().parse::<i32>().unwrap_or(300).max(-1),
        _ => 300,
    }
}

pub(crate) fn picker_from_lifecycle(secs: i32) -> (&'static str, String) {
    match secs {
        0 => ("ephemeral", String::new()),
        -1 => ("persistent", String::new()),
        300 => ("auto", String::new()),
        other => ("custom", other.to_string()),
    }
}

pub(crate) fn parse_headers_field(raw: &str) -> Vec<(String, String)> {
    raw.split(',')
        .filter_map(|chunk| {
            let chunk = chunk.trim();
            if chunk.is_empty() {
                return None;
            }
            let (k, v) = chunk.split_once('=')?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

pub(crate) fn bearer_from_headers(headers: &[(String, String)]) -> Option<String> {
    headers.iter().find_map(|(k, v)| {
        if k.eq_ignore_ascii_case("authorization") {
            v.strip_prefix("Bearer ").map(str::to_string)
        } else {
            None
        }
    })
}

pub(crate) fn headers_field_from_env(env: &[EnvEntry]) -> String {
    env.iter()
        .find(|e| e.name == conduit::daemon::upstream::http::BEARER_TOKEN_ENV)
        .and_then(|e| match &e.value {
            EnvValue::Plain(v) => Some(format!("Authorization=Bearer {v}")),
            EnvValue::Secret(_) => None,
        })
        .unwrap_or_default()
}
