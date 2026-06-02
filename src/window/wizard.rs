//! Add/edit-server wizard: probe an upstream, then save it with policies.
//!
//! Drives the multi-step Slint dialog: step 1 collects the form, `on_start_probe`
//! connects/initializes the upstream (streaming OAuth/phase progress to the UI),
//! and `on_wizard_save` writes any in-memory OAuth credentials to the keyring
//! under the server name and persists. `PendingWizard` (behind a
//! `std::sync::Mutex`) stages the probed server — including freshly-minted OAuth
//! credentials — between steps. The `*_field` / `*_picker` helpers convert UI
//! strings to domain types.

use std::rc::Rc;
use std::sync::Arc;

use rmcp::transport::auth::StoredCredentials;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

use conduit::daemon::db::types::{
    Decision, DecisionSource, EnvEntry, EnvValue, McpServer, McpTool as DbMcpTool, TRANSPORT_HTTP,
    TRANSPORT_STDIO,
};
use conduit::daemon::events::Event;
use conduit::daemon::onboarding::mark_first_run_completed;
use conduit::daemon::upstream::probe::{
    self, DraftEnvEntry, DraftServerConfig, DraftTransport, HttpAuth,
};
use conduit::daemon::wizard_input;

use crate::window::refresh::refresh_servers;
use crate::window::util::unix_now;
use crate::window::view::{EnvVarRow, MainWindow, ToolPolicyRow as ToolPolicyUiRow};
use crate::window::CallbackContext;

#[derive(Default, Clone)]
struct PendingWizard {
    server: Option<McpServer>,
    tools: Vec<DbMcpTool>,
    descriptions: std::collections::HashMap<String, String>,
    oauth_credentials: Option<StoredCredentials>,
}

#[expect(
    clippy::too_many_lines,
    reason = "wizard wires 4 Slint callbacks in one place"
)]
pub(crate) fn install_wizard_callbacks(
    window: &MainWindow,
    ctx: &CallbackContext,
    env_rows: &Rc<VecModel<EnvVarRow>>,
) {
    let db = ctx.db.clone();
    let bus = ctx.bus.clone();
    let rt = ctx.rt.clone();

    let pending: Arc<std::sync::Mutex<PendingWizard>> =
        Arc::new(std::sync::Mutex::new(PendingWizard::default()));

    // Env-var/header row editing — all mutate the shared model in place.
    let rows = env_rows.clone();
    window.on_reset_add_server_env(move || {
        rows.set_vec(vec![EnvVarRow::default()]);
    });
    let rows = env_rows.clone();
    window.on_env_add(move || {
        rows.push(EnvVarRow::default());
    });
    let rows = env_rows.clone();
    window.on_env_remove(move |i| {
        let i = usize::try_from(i).unwrap_or(0);
        if i < rows.row_count() {
            rows.remove(i);
        }
    });
    let rows = env_rows.clone();
    window.on_env_set_key(move |i, key| {
        let i = usize::try_from(i).unwrap_or(0);
        if let Some(mut row) = rows.row_data(i) {
            row.key = key;
            rows.set_row_data(i, row);
        }
    });
    let rows = env_rows.clone();
    window.on_env_set_value(move |i, value| {
        let i = usize::try_from(i).unwrap_or(0);
        if let Some(mut row) = rows.row_data(i) {
            row.value = value;
            rows.set_row_data(i, row);
        }
    });
    // Populate the rows from a "k=v, k2=v2" string (used by the edit path, which
    // runs on a worker thread and can't touch the !Send model directly).
    let rows = env_rows.clone();
    window.on_populate_env_from_string(move |raw| {
        let pairs = parse_headers_field(raw.as_str());
        if pairs.is_empty() {
            rows.set_vec(vec![EnvVarRow::default()]);
        } else {
            set_env_rows(&rows, &pairs);
        }
    });

    let weak = window.as_weak();
    let db_for_probe = db.clone();
    let rt_for_probe = rt.clone();
    let pending_for_probe = pending.clone();
    let env_for_probe = env_rows.clone();
    window.on_start_probe(move || {
        let Some(ui) = weak.upgrade() else { return };
        let name = ui.get_add_server_name().to_string();
        if !wizard_input::is_valid_server_name(&name) {
            ui.set_add_server_status(SharedString::from("Name must match [a-z0-9_-]{1,64}."));
            ui.set_add_server_status_is_error(true);
            return;
        }
        let transport = ui.get_add_server_transport().to_string();
        let command = ui.get_add_server_command().to_string();
        let args_raw = ui.get_add_server_args().to_string();
        let url = ui.get_add_server_url().to_string();
        let env_pairs = env_pairs_from_model(&env_for_probe);
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
            env_pairs.clone(),
            lifecycle_seconds,
        );
        let env: Vec<EnvEntry> = if transport == TRANSPORT_HTTP {
            bearer_from_headers(&env_pairs)
                .map(|token| {
                    vec![EnvEntry {
                        name: conduit::daemon::upstream::http::BEARER_TOKEN_ENV.to_string(),
                        value: EnvValue::Plain(token),
                    }]
                })
                .unwrap_or_default()
        } else {
            env_pairs
                .iter()
                .map(|(k, v)| EnvEntry {
                    name: k.clone(),
                    value: EnvValue::Plain(v.clone()),
                })
                .collect()
        };
        let server_row = McpServer {
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
            reported_name: None,
            reported_version: None,
            last_probed_at: None,
            last_probe_error: None,
            last_auth_error_msg: None,
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
                    let tools: Vec<DbMcpTool> = probed
                        .tools
                        .into_iter()
                        .map(|t| {
                            if let Some(d) = &t.description {
                                descriptions.insert(t.name.clone(), d.clone());
                            }
                            DbMcpTool {
                                server: name.clone(),
                                name: t.name,
                                description: t.description,
                                input_schema_json: t.input_schema.to_string(),
                                discovered_at: now,
                                decision: None,
                            }
                        })
                        .collect();
                    // Pre-fill each tool's decision with the current GLOBAL default
                    // (settings_kv.default_unknown_tool_policy, fallback Ask) so the
                    // wizard explicitly pins new tools to it on save — e.g. a global
                    // default of `deny` yields denied tools.
                    let default_decision = db
                        .get_settings_kv("default_unknown_tool_policy")
                        .await
                        .ok()
                        .flatten()
                        .and_then(|s| Decision::from_db_str(&s))
                        .unwrap_or(Decision::Ask);
                    let tool_rows: Vec<ToolPolicyUiRow> = tools
                        .iter()
                        .map(|t| {
                            let description = t.description.clone().unwrap_or_default();
                            let summary =
                                description.lines().next().unwrap_or_default().to_string();
                            ToolPolicyUiRow {
                                name: SharedString::from(t.name.clone()),
                                description: SharedString::from(description),
                                summary: SharedString::from(summary),
                                decision: SharedString::from(default_decision.as_db_str()),
                            }
                        })
                        .collect();
                    let mut updated_server = server_row;
                    updated_server.reported_name = Some(probed.server_name.clone());
                    updated_server.reported_version = Some(probed.server_version.clone());
                    updated_server.last_probed_at = Some(now);
                    updated_server.last_probe_error = None;
                    if let Ok(mut guard) = pending.lock() {
                        guard.server = Some(updated_server);
                        guard.tools = tools;
                        guard.descriptions = descriptions;
                        guard.oauth_credentials = probed.oauth_credentials;
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
        let oauth_credentials = staged.oauth_credentials.clone();
        let server_to_save = server_row;
        rt_for_save.spawn(async move {
            // Persist the wizard's in-memory OAuth credentials to the keyring
            // under the canonical server name — present only if this server ran
            // OAuth during Test connection.
            if let Some(cred) = oauth_credentials {
                let server = name.clone();
                match tokio::task::spawn_blocking(move || {
                    conduit::daemon::secrets::store_oauth_cred(&server, &cred)
                })
                .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(secrets_err)) => tracing::error!(
                        server = %name,
                        error = %secrets_err,
                        "oauth credential save failed — server will save without working auth",
                    ),
                    Err(join_err) => tracing::warn!(
                        error = %join_err,
                        "oauth credential save join failed",
                    ),
                }
            }
            let result = db.upsert_server_with_tools(server_to_save, tools).await;
            let outcome: Result<usize, String> = match result {
                Ok(()) => {
                    for (tool, decision) in &policy_overrides {
                        if let Err(e) = db
                            .set_tool_decision(&name, tool, *decision, DecisionSource::User)
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
    env_pairs: Vec<(String, String)>,
    lifecycle_seconds: i32,
) -> DraftServerConfig {
    let transport = if transport == TRANSPORT_HTTP {
        DraftTransport::Http {
            url: url.to_string(),
            auth: HttpAuth::None,
            headers: env_pairs,
        }
    } else {
        let env = env_pairs
            .into_iter()
            .map(|(name, value)| DraftEnvEntry {
                name,
                value: Zeroizing::new(value),
                is_secret: false,
            })
            .collect();
        DraftTransport::Stdio {
            command: command.to_string(),
            args,
            env,
        }
    };
    DraftServerConfig {
        name: name.to_string(),
        transport,
        lifecycle_seconds,
    }
}

/// Non-empty (key, value) pairs from the wizard's env-var/header row model.
pub(crate) fn env_pairs_from_model(rows: &Rc<VecModel<EnvVarRow>>) -> Vec<(String, String)> {
    rows.iter()
        .filter_map(|r| {
            let key = r.key.trim().to_string();
            (!key.is_empty()).then(|| (key, r.value.to_string()))
        })
        .collect()
}

/// Replace the env-var row model contents in place (preserves model identity).
pub(crate) fn set_env_rows(rows: &Rc<VecModel<EnvVarRow>>, pairs: &[(String, String)]) {
    let v: Vec<EnvVarRow> = pairs
        .iter()
        .map(|(k, val)| EnvVarRow {
            key: SharedString::from(k.as_str()),
            value: SharedString::from(val.as_str()),
        })
        .collect();
    rows.set_vec(v);
}

pub(crate) fn lifecycle_from_picker(mode: &str, custom: &str) -> i32 {
    match mode {
        "ephemeral" => 0,
        "custom" => custom.trim().parse::<i32>().unwrap_or(300).max(-1),
        // "persistent" and any unknown mode → persistent (the default).
        _ => -1,
    }
}

pub(crate) fn picker_from_lifecycle(secs: i32) -> (&'static str, String) {
    match secs {
        0 => ("ephemeral", String::new()),
        -1 => ("persistent", String::new()),
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

/// Serialize a saved server's env/headers into the "k=v, k2=v2" string the
/// wizard's env-row editor is populated from.
pub(crate) fn env_field_from_server(server: &McpServer) -> String {
    if server.transport == TRANSPORT_HTTP {
        return headers_field_from_env(&server.env);
    }
    server
        .env
        .iter()
        .filter_map(|e| match &e.value {
            EnvValue::Plain(v) => Some(format!("{}={}", e.name, v)),
            EnvValue::Secret(_) => None,
        })
        .collect::<Vec<_>>()
        .join(", ")
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
