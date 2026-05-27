#![warn(rustdoc::broken_intra_doc_links)]
#![warn(rustdoc::private_intra_doc_links)]

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

use mcp_router::daemon::approval::{spawn_timeout_sweeper, ApprovalCenter};
use mcp_router::daemon::cascade;
use mcp_router::daemon::config_toml::Settings;
use mcp_router::daemon::config_watcher;
use mcp_router::daemon::db::types::{
    ApprovalStatus, Decision, DecisionSource, EnvEntry, EnvValue, ManagedIdeConfig, Server,
    ToolMeta as DbToolMeta, TRANSPORT_HTTP, TRANSPORT_STDIO,
};
use mcp_router::daemon::db::Db;
use mcp_router::daemon::events::{Bus, Event};
use mcp_router::daemon::http_server::{self, AppState};
use mcp_router::daemon::ide_detect::IdeKind;
use mcp_router::daemon::ide_writer;
use mcp_router::daemon::ipc;
use mcp_router::daemon::onboarding::{
    check_background_hint, mark_first_run_completed, should_show_window_at_startup,
    BackgroundHintAction,
};
use mcp_router::daemon::paste_parser;
use mcp_router::daemon::server_validators;
use mcp_router::daemon::skills::{SkillCandidate, SkillManager};
use mcp_router::daemon::db::types::SkillRepo;
use mcp_router::daemon::upstream::probe::{self, DraftServerConfig, DraftTransport, HttpAuth};
use mcp_router::daemon::upstream::manager::UpstreamManager;
use mcp_router::gui::{
    hotkey, notification_router as gui_notification_router, notifications as gui_notifications,
    tray as gui_tray,
};
use mcp_router::paths::Paths;
use mcp_router::runtime::Worker;

mod ui;
use ui::{
    ApprovalRow, AuditRow, IdeRow, MainWindow, PolicyRow, ServerDetail, ServerRow, SettingsRow,
    SkillRepoRow, SkillRow, ToolPolicyRow as ToolPolicyUiRow,
};

#[expect(clippy::too_many_lines, reason = "single-binary entrypoint wires every subsystem")]
fn main() -> Result<()> {
    init_tracing();
    tracing::info!("mcp-router starting");

    let paths = Paths::from_user_dirs().context("resolve mcp-router config dir")?;
    paths.ensure_created().context("create state dirs")?;
    tracing::info!(root = %paths.root().display(), "state root ready");

    let settings = Settings::load_or_default(&paths.config_toml());
    tracing::info!(port = settings.daemon.port, "settings loaded");

    let cancel = CancellationToken::new();
    let worker = Worker::spawn(cancel.clone()).context("spawn tokio worker")?;

    // Single-instance IPC: bind early so re-launches fail fast and forward Show.
    let socket = ipc::resolve_socket_path();
    let listener = match worker.handle().block_on(ipc::try_bind(&socket)) {
        Ok(l) => l,
        Err(ipc::BindError::InUse) => {
            let send_result = worker
                .handle()
                .block_on(ipc::send_command(&socket, &ipc::Command::Show));
            #[expect(
                clippy::print_stderr,
                reason = "CLI UX: user reinvoked the binary, deserves a one-line ack"
            )]
            {
                eprintln!("mcp-router: already running — opened existing window.");
            }
            if let Err(e) = send_result {
                tracing::warn!(error = %e, "ipc forward show failed");
            }
            return Ok(());
        }
        Err(e) => return Err(anyhow::Error::new(e).context("ipc bind")),
    };

    let db = Db::spawn_at(&paths.state_db()).context("spawn db actor")?;
    let bus = Bus::new();

    let _watcher = {
        let _guard = worker.handle().enter();
        config_watcher::spawn(&paths.config_toml(), bus.clone(), cancel.clone())
            .context("spawn config watcher")?
    };

    let approval_timeout = Duration::from_secs(u64::from(settings.daemon.approval_timeout_seconds));
    let approvals = Arc::new(ApprovalCenter::new(
        db.clone(),
        bus.clone(),
        approval_timeout,
    ));
    // UpstreamManager::new spawns a reaper task via tokio::spawn — must run
    // inside the worker runtime context, not the bare main thread.
    let upstream = {
        let _guard = worker.handle().enter();
        Arc::new(UpstreamManager::new(paths.clone(), cancel.clone()))
    };
    let skill_manager = {
        let _guard = worker.handle().enter();
        Arc::new(
            SkillManager::new(paths, db.clone(), bus.clone())
                .context("build skill manager")?,
        )
    };

    let app_state = AppState {
        db: db.clone(),
        approvals: approvals.clone(),
        upstream,
        port: settings.daemon.port,
    };

    {
        let _guard = worker.handle().enter();
        let _sweeper = spawn_timeout_sweeper(
            db.clone(),
            bus.clone(),
            approval_timeout,
            cancel.clone(),
        );
        let http_cancel = cancel.clone();
        tokio::spawn(async move {
            if let Err(e) = http_server::serve(app_state, http_cancel).await {
                tracing::error!(error = %e, "axum server exited with error");
            }
        });
    }

    let window = MainWindow::new().context("build slint main window")?;
    install_callbacks(
        &window,
        &db,
        &approvals,
        &bus,
        settings.daemon.port,
        worker.handle().clone(),
        skill_manager.clone(),
    );
    push_initial_models(
        &window,
        &db,
        &settings,
        worker.handle().clone(),
        skill_manager.clone(),
    );
    let settings_arc = Arc::new(settings);
    spawn_event_bridge(
        &window,
        bus.subscribe(),
        db.clone(),
        worker.handle().clone(),
        skill_manager,
    );

    // Notifications bridge: emits OS toasts AND owns the click→dispatch loop.
    {
        let _guard = worker.handle().enter();
        let (action_tx, mut action_rx) =
            tokio::sync::mpsc::unbounded_channel::<gui_notification_router::NotificationAction>();
        let _notif = gui_notifications::spawn(
            bus.subscribe(),
            db.clone(),
            settings_arc,
            cancel.clone(),
            action_tx,
        );
        let approvals_for_router = Arc::clone(&approvals);
        let bus_for_router = bus.clone();
        let db_for_router = db.clone();
        let cancel_for_router = cancel.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = cancel_for_router.cancelled() => break,
                    msg = action_rx.recv() => {
                        let Some(action) = msg else { break };
                        gui_notification_router::dispatch_action(
                            action,
                            &db_for_router,
                            &approvals_for_router,
                            &bus_for_router,
                        )
                        .await;
                    }
                }
            }
        });
    }

    // IPC accept loop: needs MainWindow to be built for the show-handler.
    {
        let _guard = worker.handle().enter();
        let weak = window.as_weak();
        let on_show: ipc::ShowHandler = Arc::new(move || {
            let weak = weak.clone();
            slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak.upgrade() {
                    ui.window().show().ok();
                }
            })
            .ok();
        });
        tokio::spawn(ipc::accept_loop(listener, cancel.clone(), on_show));
    }

    // Tray icon on a dedicated thread; nil if SNI host or libxdo absent.
    let _tray = install_tray(&db, &bus, &cancel, &window, worker.handle().clone());

    let _hotkey = install_hotkey(&window);

    // First-run override: force-show on first run, otherwise honor setting.
    match worker.handle().block_on(should_show_window_at_startup(&db)) {
        Ok(true) => {
            window.show().context("show main window")?;
        }
        Ok(false) => {
            tracing::info!("open_window_at_startup=false; staying headless");
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to read open_window_at_startup; defaulting to show");
            window.show().context("show main window (fallback)")?;
        }
    }

    let db_for_close = db;
    let rt_for_close = worker.handle().clone();
    window.window().on_close_requested(move || {
        tracing::info!("window close requested; daemon stays running");
        // Background hint: one-time notification on first close after onboarding.
        let db = db_for_close.clone();
        rt_for_close.spawn(async move {
            match check_background_hint(&db).await {
                Ok(BackgroundHintAction::Fire) => {
                    fire_background_hint();
                }
                Ok(BackgroundHintAction::Skip) => {}
                Err(e) => tracing::warn!(error = %e, "background-hint check failed"),
            }
        });
        slint::CloseRequestResponse::HideWindow
    });

    tracing::info!("entering slint event loop");
    window.run().context("slint event loop")?;

    cancel.cancel();
    tracing::info!("mcp-router stopped");
    drop(bus);
    drop(worker);
    Ok(())
}

#[derive(Default, Clone)]
struct PendingWizard {
    server: Option<Server>,
    tools: Vec<DbToolMeta>,
    descriptions: std::collections::HashMap<String, String>,
    oauth_session: Option<String>,
    oauth_authority: Option<String>,
    oauth_client_id: Option<String>,
}

#[expect(clippy::too_many_lines, reason = "wizard wires 4 Slint callbacks in one place")]
fn install_wizard_callbacks(window: &MainWindow, db: Db, rt: tokio::runtime::Handle) {
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
            ui.set_add_server_status(SharedString::from(
                "Name must match [a-z0-9_-]{1,64}.",
            ));
            ui.set_add_server_status_is_error(true);
            return;
        }
        let transport = ui.get_add_server_transport().to_string();
        let command = ui.get_add_server_command().to_string();
        let args_raw = ui.get_add_server_args().to_string();
        let url = ui.get_add_server_url().to_string();
        let headers_raw = ui.get_add_server_headers().to_string();
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

        let draft = build_draft(&name, &transport, &command, args.clone(), &url, &headers_raw);
        let server_row = Server {
            name: name.clone(),
            transport: transport.clone(),
            command: if transport == TRANSPORT_STDIO && !command.is_empty() {
                Some(command)
            } else {
                None
            },
            args,
            env: Vec::<EnvEntry>::new(),
            url: if transport == TRANSPORT_HTTP && !url.is_empty() {
                Some(url)
            } else {
                None
            },
            lifecycle_seconds: 300,
            last_init_duration_ms: None,
            package_version: None,
            mcp_self_version: None,
            created_at: 0,
            server_name: None,
            server_version: None,
            last_probed_at: None,
            last_probe_error: None,
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
                    if let Ok(mut guard) = pending.lock() {
                        guard.server = Some(updated_server);
                        guard.tools = tools;
                        guard.descriptions = descriptions;
                        guard.oauth_session = probed.oauth_session;
                        guard.oauth_authority = probed.oauth_authority;
                        guard.oauth_client_id = probed.oauth_client_id;
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
        let pending = pending_for_save.clone();
        let tools = staged.tools.clone();
        let name = server_row.name.clone();
        let oauth_session = staged.oauth_session.clone();
        let server_to_save = server_row;
        rt_for_save.spawn(async move {
            // Promote OAuth tokens from draft session → canonical server name.
            if let Some(draft) = oauth_session.as_deref() {
                if let Err(e) = tokio::task::spawn_blocking({
                    let draft = draft.to_string();
                    let server = name.clone();
                    move || mcp_router::daemon::secrets::promote_oauth_tokens(&draft, &server)
                })
                .await
                {
                    tracing::warn!(error = %e, "oauth token promotion join failed");
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

fn build_draft(
    name: &str,
    transport: &str,
    command: &str,
    args: Vec<String>,
    url: &str,
    headers_raw: &str,
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
        lifecycle_seconds: 300,
        draft_session: uuid::Uuid::new_v4().to_string(),
    }
}

fn parse_headers_field(raw: &str) -> Vec<(String, String)> {
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

fn relative_time(delta_secs: i64) -> String {
    if delta_secs < 0 {
        return "just now".to_string();
    }
    if delta_secs < 60 {
        return format!("{delta_secs}s ago");
    }
    let mins = delta_secs / 60;
    if mins < 60 {
        return format!("{mins}m ago");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    format!("{}d ago", hours / 24)
}

fn unix_now() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let s = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    i64::try_from(s).unwrap_or(i64::MAX)
}

fn fire_background_hint() {
    let summary = "mcp-router is running in the background";
    let body = "Click the tray icon, press Ctrl+Alt+Shift+M, or run `mcp-router` to reopen.";
    if let Err(e) = notify_rust::Notification::new()
        .summary(summary)
        .body(body)
        .show()
    {
        tracing::warn!(error = %e, "background-hint notification failed");
    }
}

fn install_tray(
    db: &Db,
    bus: &Bus,
    cancel: &CancellationToken,
    window: &MainWindow,
    rt: tokio::runtime::Handle,
) -> Option<gui_tray::TrayHandle> {
    let weak_show = window.as_weak();
    let on_show = Arc::new(move || {
        let weak = weak_show.clone();
        slint::invoke_from_event_loop(move || {
            if let Some(ui) = weak.upgrade() {
                ui.window().show().ok();
            }
        })
        .ok();
    });

    let cancel_for_quit = cancel.clone();
    let on_quit = Arc::new(move || {
        cancel_for_quit.cancel();
        slint::invoke_from_event_loop(|| {
            slint::quit_event_loop().ok();
        })
        .ok();
    });

    let db_for_pause = db.clone();
    let rt_for_pause = rt.clone();
    let on_pause_toggle = Arc::new(move |paused: bool| {
        let db = db_for_pause.clone();
        rt_for_pause.spawn(async move {
            let value = if paused { "true" } else { "false" };
            if let Err(e) = db.set_settings_kv("routing_paused", value).await {
                tracing::warn!(error = %e, "set routing_paused failed");
            }
        });
    });

    let callbacks = gui_tray::TrayCallbacks {
        on_show,
        on_quit,
        on_pause_toggle,
    };

    gui_tray::install(db.clone(), rt, bus.subscribe(), cancel.clone(), callbacks)
}

fn install_hotkey(window: &MainWindow) -> Option<hotkey::HotkeyHandles> {
    let handles = hotkey::try_install()?;
    let weak = window.as_weak();
    hotkey::spawn_event_pump(handles.hotkey_id, move || {
        focus_window(&weak);
    });
    tracing::info!("global hotkey Ctrl+Alt+Shift+M registered");
    Some(handles)
}

fn focus_window(weak: &slint::Weak<MainWindow>) {
    let weak = weak.clone();
    slint::invoke_from_event_loop(move || {
        if let Some(ui) = weak.upgrade() {
            ui.window().show().ok();
        }
    })
    .ok();
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

#[expect(clippy::too_many_lines, reason = "every UI callback wires here")]
fn install_callbacks(
    window: &MainWindow,
    db: &Db,
    approvals: &Arc<ApprovalCenter>,
    bus: &Bus,
    port: u16,
    rt: tokio::runtime::Handle,
    skills: Arc<SkillManager>,
) {
    install_skills_callbacks(window, &rt, &skills);
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
                    ui.set_selected_server_tools(ModelRc::new(VecModel::from(Vec::<ToolPolicyUiRow>::new())));
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
    let rt_for_retest = rt.clone();
    let weak_for_retest = window.as_weak();
    window.on_retest_clicked(move |name| {
        let db = db_for_retest.clone();
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

    let db_for_remove_ide = db.clone();
    let rt_for_remove_ide = rt.clone();
    window.on_remove_ide_clicked(move |name| {
        let db = db_for_remove_ide.clone();
        rt_for_remove_ide.spawn(async move {
            let keys = db
                .list_ide_keys_for_name(name.as_str())
                .await
                .unwrap_or_default();
            for k in keys {
                if let Err(e) = db.delete_ide_key(&k.key).await {
                    tracing::warn!(error = %e, "delete ide key failed");
                }
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

    install_wizard_callbacks(window, db.clone(), rt.clone());

    let weak_for_ide = window.as_weak();
    let db_for_ide = db.clone();
    let rt_for_ide = rt;
    window.on_submit_add_ide(move || {
        let Some(ui) = weak_for_ide.upgrade() else { return; };
        let selected = ui.get_add_ide_selected().to_string();
        let Some(kind) = parse_ide_kind(&selected) else {
            tracing::warn!(ide = %selected, "unknown ide name");
            return;
        };
        let db = db_for_ide.clone();
        let weak = weak_for_ide.clone();
        rt_for_ide.spawn(async move {
            // 1. Generate key in the DB.
            let key = match db.create_ide_key(&selected).await {
                Ok(k) => k,
                Err(e) => {
                    tracing::warn!(error = %e, "create_ide_key failed");
                    return;
                }
            };
            // 2. Pull current server names so we can list them in the IDE config.
            let servers = db.list_servers().await.unwrap_or_default();
            let server_names: Vec<String> = servers.into_iter().map(|s| s.name).collect();
            // 3. Auto-write the IDE config (or skip if we lack a path for this OS).
            match ide_writer::write(kind, &key, &server_names, port) {
                Ok(managed) => {
                    let log_path = managed.config_path.display().to_string();
                    if let Err(e) = db.record_ide_config_write(managed).await {
                        tracing::warn!(error = %e, "record_ide_config_write failed");
                    } else {
                        tracing::info!(ide = %selected, key = %key.key, path = %log_path, "added IDE");
                        if let Err(e) = mark_first_run_completed(&db).await {
                            tracing::warn!(error = %e, "mark_first_run_completed failed");
                        }
                    }
                }
                Err(e) => tracing::warn!(error = %e, ide = %selected, "ide_writer::write failed"),
            }
            refresh_ides(&weak, &db).await;
        });
    });

    let _ = EnvValue::Plain(String::new());
    let _ = ManagedIdeConfig {
        ide_name: String::new(),
        ide_key: String::new(),
        config_path: std::path::PathBuf::new(),
        managed_entries: Vec::new(),
        written_at: 0,
    };
}

fn parse_ide_kind(name: &str) -> Option<IdeKind> {
    match name {
        "Claude Code" | "ClaudeCode" => Some(IdeKind::ClaudeCode),
        "Cursor" => Some(IdeKind::Cursor),
        "VsCodeCopilot" | "VS Code Copilot" | "VS Code" => Some(IdeKind::VsCodeCopilot),
        "OpenCode" => Some(IdeKind::OpenCode),
        "OpenClaude" => Some(IdeKind::OpenClaude),
        _ => None,
    }
}

#[expect(clippy::too_many_lines, reason = "Skills tab wires seven callbacks at once")]
fn install_skills_callbacks(
    window: &MainWindow,
    rt: &tokio::runtime::Handle,
    skills: &Arc<SkillManager>,
) {
    let weak = window.as_weak();
    let rt_for_refresh = rt.clone();
    let skills_for_refresh = skills.clone();
    window.on_skills_refresh_clicked(move || {
        let skills = skills_for_refresh.clone();
        let weak = weak.clone();
        weak.upgrade_in_event_loop(|ui| {
            ui.set_skills_busy(true);
            ui.set_skills_status_message(SharedString::from("Refreshing…"));
        })
        .ok();
        rt_for_refresh.spawn(async move {
            match skills.discover(true).await {
                Ok(_) => {
                    refresh_skills(&weak, &skills).await;
                    weak.upgrade_in_event_loop(|ui| {
                        ui.set_skills_busy(false);
                        ui.set_skills_status_message(SharedString::from(""));
                    })
                    .ok();
                }
                Err(e) => {
                    let msg = format!("Refresh failed: {e}");
                    tracing::warn!(error = %e, "skill discover failed");
                    weak.upgrade_in_event_loop(move |ui| {
                        ui.set_skills_busy(false);
                        ui.set_skills_status_message(SharedString::from(msg));
                    })
                    .ok();
                }
            }
        });
    });

    let weak = window.as_weak();
    let rt_for_install = rt.clone();
    let skills_for_install = skills.clone();
    window.on_skills_install_clicked(
        move |owner, name, branch, dir, readme, candidate_name| {
            let skills = skills_for_install.clone();
            let weak = weak.clone();
            let candidate = SkillCandidate {
                key: format!("{owner}/{name}:{dir}"),
                name: candidate_name.to_string(),
                description: String::new(),
                directory: dir.to_string(),
                readme_url: readme.to_string(),
                repo_owner: owner.to_string(),
                repo_name: name.to_string(),
                repo_branch: branch.to_string(),
            };
            weak.upgrade_in_event_loop(|ui| {
                ui.set_skills_busy(true);
                ui.set_skills_status_message(SharedString::from("Installing…"));
            })
            .ok();
            rt_for_install.spawn(async move {
                // No targets by default — user opts in per-IDE after install.
                let result = skills.install(candidate, Vec::new()).await;
                match result {
                    Ok(skill) => {
                        let msg = format!("Installed {}", skill.skill_name);
                        weak.upgrade_in_event_loop(move |ui| {
                            ui.set_skills_busy(false);
                            ui.set_skills_status_message(SharedString::from(msg));
                        })
                        .ok();
                    }
                    Err(e) => {
                        let msg = format!("Install failed: {e}");
                        tracing::warn!(error = %e, "skill install failed");
                        weak.upgrade_in_event_loop(move |ui| {
                            ui.set_skills_busy(false);
                            ui.set_skills_status_message(SharedString::from(msg));
                        })
                        .ok();
                    }
                }
            });
        },
    );

    let weak = window.as_weak();
    let rt_for_uninstall = rt.clone();
    let skills_for_uninstall = skills.clone();
    window.on_skills_uninstall_clicked(move |directory| {
        let skills = skills_for_uninstall.clone();
        let weak = weak.clone();
        let directory = directory.to_string();
        rt_for_uninstall.spawn(async move {
            if let Err(e) = skills.uninstall(&directory).await {
                let msg = format!("Uninstall failed: {e}");
                tracing::warn!(error = %e, directory = %directory, "skill uninstall failed");
                weak.upgrade_in_event_loop(move |ui| {
                    ui.set_skills_status_message(SharedString::from(msg));
                })
                .ok();
            }
        });
    });

    let rt_for_toggle = rt.clone();
    let skills_for_toggle = skills.clone();
    let weak = window.as_weak();
    window.on_skills_toggle_target(move |directory, ide, enable| {
        let skills = skills_for_toggle.clone();
        let weak = weak.clone();
        let directory = directory.to_string();
        let ide = ide.to_string();
        rt_for_toggle.spawn(async move {
            let Ok(Some(current)) = skills.list_installed().await.map(|all| {
                all.into_iter().find(|s| s.directory == directory)
            }) else {
                return;
            };
            let mut targets: Vec<String> = current.targets.clone();
            targets.retain(|t| t != &ide);
            if enable {
                targets.push(ide.clone());
            }
            if let Err(e) = skills.set_targets(&directory, targets).await {
                let msg = format!("Toggle failed: {e}");
                tracing::warn!(error = %e, "skill set_targets failed");
                weak.upgrade_in_event_loop(move |ui| {
                    ui.set_skills_status_message(SharedString::from(msg));
                })
                .ok();
            }
        });
    });

    let rt_for_add_repo = rt.clone();
    let skills_for_add_repo = skills.clone();
    window.on_skills_add_repo(move |owner, name, branch| {
        let skills = skills_for_add_repo.clone();
        let repo = SkillRepo {
            owner: owner.to_string(),
            name: name.to_string(),
            branch: branch.to_string(),
            enabled: true,
        };
        rt_for_add_repo.spawn(async move {
            if let Err(e) = skills.add_repo(repo).await {
                tracing::warn!(error = %e, "skill add_repo failed");
            }
        });
    });

    let rt_for_remove_repo = rt.clone();
    let skills_for_remove_repo = skills.clone();
    window.on_skills_remove_repo(move |owner, name| {
        let skills = skills_for_remove_repo.clone();
        let owner = owner.to_string();
        let name = name.to_string();
        rt_for_remove_repo.spawn(async move {
            if let Err(e) = skills.remove_repo(&owner, &name).await {
                tracing::warn!(error = %e, "skill remove_repo failed");
            }
        });
    });

    let rt_for_toggle_repo = rt.clone();
    let skills_for_toggle_repo = skills.clone();
    window.on_skills_toggle_repo(move |owner, name, enabled| {
        let skills = skills_for_toggle_repo.clone();
        let owner = owner.to_string();
        let name = name.to_string();
        rt_for_toggle_repo.spawn(async move {
            if let Err(e) = skills.set_repo_enabled(&owner, &name, enabled).await {
                tracing::warn!(error = %e, "skill set_repo_enabled failed");
            }
        });
    });

    let rt_for_open_folder = rt.clone();
    let skills_for_open_folder = skills.clone();
    let weak = window.as_weak();
    window.on_skills_open_folder_clicked(move || {
        let skills = skills_for_open_folder.clone();
        let weak = weak.clone();
        rt_for_open_folder.spawn(async move {
            let dir = skills.paths().skills_managed_dir();
            if let Err(e) = tokio::fs::create_dir_all(&dir).await {
                tracing::warn!(error = %e, "create skills dir failed");
            }
            let dir_for_open = dir.clone();
            let open_result =
                tokio::task::spawn_blocking(move || open::that(&dir_for_open)).await;
            match open_result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    let msg = format!("Couldn't open folder: {e}");
                    tracing::warn!(error = %e, path = %dir.display(), "open folder failed");
                    weak.upgrade_in_event_loop(move |ui| {
                        ui.set_skills_status_message(SharedString::from(msg));
                    })
                    .ok();
                }
                Err(e) => tracing::warn!(error = %e, "open folder join failed"),
            }
        });
    });
}

async fn refresh_skills(weak: &slint::Weak<MainWindow>, skills: &SkillManager) {
    if let Err(e) = skills.register_local_skills().await {
        tracing::warn!(error = %e, "register_local_skills failed");
    }
    paint_skills_from_db(weak, skills).await;
    match skills.discover(false).await {
        Ok(candidates) => paint_skills_with_discovery(weak, skills, candidates).await,
        Err(e) => tracing::warn!(error = %e, "skill discover failed during refresh"),
    }
}

async fn paint_skills_from_db(weak: &slint::Weak<MainWindow>, skills: &SkillManager) {
    let installed = skills.list_installed().await.unwrap_or_default();
    let repos = skills.list_repos().await.unwrap_or_default();
    let rows = installed_skills_to_rows(&installed);
    let repo_rows = repos_to_rows(repos);
    let _ignored = weak.upgrade_in_event_loop(move |ui| {
        ui.set_skills(ModelRc::new(VecModel::from(rows)));
        ui.set_skill_repos(ModelRc::new(VecModel::from(repo_rows)));
    });
}

async fn paint_skills_with_discovery(
    weak: &slint::Weak<MainWindow>,
    skills: &SkillManager,
    candidates: Vec<SkillCandidate>,
) {
    let installed = skills.list_installed().await.unwrap_or_default();
    let repos = skills.list_repos().await.unwrap_or_default();
    let mut rows = installed_skills_to_rows(&installed);
    let installed_dirs: std::collections::HashSet<String> =
        installed.iter().map(|s| s.directory.clone()).collect();

    {
        {
            for candidate in candidates {
                let install_name = candidate
                    .directory
                    .rsplit('/')
                    .next()
                    .unwrap_or(&candidate.directory)
                    .to_string();
                if installed_dirs.contains(&install_name) {
                    continue;
                }
                rows.push(SkillRow {
                    key: SharedString::from(candidate.key.clone()),
                    directory: SharedString::from(install_name),
                    name: SharedString::from(candidate.name),
                    description: SharedString::from(candidate.description),
                    repo_label: SharedString::from(format!(
                        "{}/{} · {}",
                        candidate.repo_owner, candidate.repo_name, candidate.repo_branch
                    )),
                    repo_owner: SharedString::from(candidate.repo_owner),
                    repo_name: SharedString::from(candidate.repo_name),
                    repo_branch: SharedString::from(candidate.repo_branch),
                    source_directory: SharedString::from(candidate.directory),
                    readme_url: SharedString::from(candidate.readme_url),
                    installed: false,
                    target_claude: false,
                    target_openclaude: false,
                    target_opencode: false,
                });
            }
        }
    }

    rows.sort_by(|a, b| match (a.installed, b.installed) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });
    let repo_rows = repos_to_rows(repos);

    let _ignored = weak.upgrade_in_event_loop(move |ui| {
        ui.set_skills(ModelRc::new(VecModel::from(rows)));
        ui.set_skill_repos(ModelRc::new(VecModel::from(repo_rows)));
    });
}

fn installed_skills_to_rows(
    installed: &[mcp_router::daemon::db::types::Skill],
) -> Vec<SkillRow> {
    installed
        .iter()
        .map(|skill| SkillRow {
            key: SharedString::from(format!(
                "{}/{}:{}",
                skill.repo_owner, skill.repo_name, skill.directory
            )),
            directory: SharedString::from(skill.directory.clone()),
            name: SharedString::from(skill.skill_name.clone()),
            description: SharedString::from(skill.description.clone()),
            repo_label: SharedString::from(format!(
                "{}/{} · {}",
                skill.repo_owner, skill.repo_name, skill.repo_branch
            )),
            repo_owner: SharedString::from(skill.repo_owner.clone()),
            repo_name: SharedString::from(skill.repo_name.clone()),
            repo_branch: SharedString::from(skill.repo_branch.clone()),
            source_directory: SharedString::from(skill.source_directory.clone()),
            readme_url: SharedString::from(skill.readme_url.clone().unwrap_or_default()),
            installed: true,
            target_claude: skill.targets.iter().any(|x| x == "ClaudeCode"),
            target_openclaude: skill.targets.iter().any(|x| x == "OpenClaude"),
            target_opencode: skill.targets.iter().any(|x| x == "OpenCode"),
        })
        .collect()
}

fn repos_to_rows(repos: Vec<SkillRepo>) -> Vec<SkillRepoRow> {
    repos
        .into_iter()
        .map(|r| SkillRepoRow {
            owner: SharedString::from(r.owner),
            name: SharedString::from(r.name),
            branch: SharedString::from(r.branch),
            enabled: r.enabled,
        })
        .collect()
}

fn push_initial_models(
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
    rt.spawn(async move {
        // Drop any half-finished install scratch left over from a prior boot
        // (no CancellationToken plumbed into install yet — see TODO).
        skills.purge_tmp().await;
        refresh_all(&weak, &db_for_refresh).await;
        refresh_skills(&weak_for_skills, &skills).await;
        // Pull settings_kv toggles into the GUI.
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

fn spawn_event_bridge(
    window: &MainWindow,
    mut rx: tokio::sync::broadcast::Receiver<Event>,
    db: Db,
    rt: tokio::runtime::Handle,
    skills: Arc<SkillManager>,
) {
    let weak = window.as_weak();
    rt.spawn(async move {
        loop {
            match rx.recv().await {
                Ok(Event::ApprovalPending { .. } | Event::ApprovalResolved { .. }) => {
                    refresh_pending(&weak, &db).await;
                    refresh_audit(&weak, &db).await;
                }
                Ok(Event::ConfigReloaded { settings }) => {
                    let row = settings_to_row(&settings);
                    weak.upgrade_in_event_loop(move |ui| ui.set_settings(row))
                        .ok();
                }
                Ok(Event::ServerHealthChanged { name }) => {
                    tracing::debug!(server = %name, "health changed; consumer to refresh");
                    refresh_servers(&weak, &db).await;
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
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "event bridge lagged");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

async fn refresh_all(weak: &slint::Weak<MainWindow>, db: &Db) {
    refresh_pending(weak, db).await;
    refresh_servers(weak, db).await;
    refresh_ides(weak, db).await;
    refresh_policies(weak, db).await;
    refresh_audit(weak, db).await;
}

async fn refresh_pending(weak: &slint::Weak<MainWindow>, db: &Db) {
    let pending = db.list_pending_approvals().await.unwrap_or_default();
    let ide_names = lookup_ide_names_for_keys(db, pending.iter().map(|p| p.ide_key.as_str())).await;
    let rows: Vec<ApprovalRow> = pending
        .into_iter()
        .map(|p| ApprovalRow {
            id: SharedString::from(p.id),
            ide_name: SharedString::from(
                ide_names
                    .get(&p.ide_key)
                    .cloned()
                    .unwrap_or_else(|| "?".to_string()),
            ),
            server: SharedString::from(p.server),
            tool: SharedString::from(p.tool),
            created_at: SharedString::from(p.created_at.to_string()),
        })
        .collect();
    weak.upgrade_in_event_loop(move |ui| {
        ui.set_approvals(ModelRc::new(VecModel::from(rows)));
    })
    .ok();
}

async fn refresh_servers(weak: &slint::Weak<MainWindow>, db: &Db) {
    let servers = db.list_servers().await.unwrap_or_default();
    let mut rows: Vec<ServerRow> = Vec::with_capacity(servers.len());
    for s in servers {
        let tool_count = db.list_server_tools(&s.name).await.map(|v| v.len()).unwrap_or(0);
        let count_label = if tool_count == 0 {
            "no tools".to_string()
        } else {
            format!("{tool_count} tools")
        };
        rows.push(ServerRow {
            name: SharedString::from(s.name),
            transport: SharedString::from(s.transport),
            lifecycle_label: SharedString::from(lifecycle_label(s.lifecycle_seconds)),
            status: SharedString::from("idle"),
            tool_count_label: SharedString::from(count_label),
        });
    }
    weak.upgrade_in_event_loop(move |ui| {
        ui.set_servers(ModelRc::new(VecModel::from(rows)));
    })
    .ok();
}

async fn refresh_server_detail(weak: &slint::Weak<MainWindow>, db: &Db, name: &str) {
    let Ok(Some(server)) = db.get_server(name).await else {
        let n = name.to_string();
        let _ignored = weak.upgrade_in_event_loop(move |ui| {
            if ui.get_selected_server().as_str() == n {
                ui.set_selected_server(SharedString::from(""));
                ui.set_selected_server_tools(ModelRc::new(VecModel::from(Vec::<ToolPolicyUiRow>::new())));
            }
        });
        return;
    };

    let tools = db.list_server_tools(name).await.unwrap_or_default();
    let mut tool_rows: Vec<ToolPolicyUiRow> = Vec::with_capacity(tools.len());
    for t in &tools {
        let decision = db
            .get_policy(name, &t.name)
            .await
            .ok()
            .flatten()
            .map_or_else(|| "ask".to_string(), |p| p.decision.as_db_str().to_string());
        tool_rows.push(ToolPolicyUiRow {
            name: SharedString::from(t.name.clone()),
            description: SharedString::from(t.description.clone().unwrap_or_default()),
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
    let version_label = server
        .server_version
        .clone()
        .or_else(|| server.package_version.clone())
        .unwrap_or_default();
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
    };

    let name_owned = name.to_string();
    let _ignored = weak.upgrade_in_event_loop(move |ui| {
        ui.set_selected_server(SharedString::from(name_owned));
        ui.set_selected_server_detail(detail);
        ui.set_selected_server_tools(ModelRc::new(VecModel::from(tool_rows)));
    });
}

async fn refresh_ides(weak: &slint::Weak<MainWindow>, db: &Db) {
    // No list-all in DB; cross-reference managed_ide_configs to surface IDE names.
    let mut rows = Vec::new();
    for ide in [
        "Claude Code",
        "Cursor",
        "VsCodeCopilot",
        "OpenCode",
        "OpenClaude",
    ] {
        let cfgs = db.list_ide_configs_for_ide(ide).await.unwrap_or_default();
        for c in cfgs {
            rows.push(IdeRow {
                name: SharedString::from(c.ide_name.clone()),
                key: SharedString::from(c.ide_key),
                config_path: SharedString::from(c.config_path.display().to_string()),
            });
        }
    }
    weak.upgrade_in_event_loop(move |ui| {
        ui.set_ides(ModelRc::new(VecModel::from(rows)));
    })
    .ok();
}

async fn refresh_policies(weak: &slint::Weak<MainWindow>, db: &Db) {
    let policies = db.list_all_policies().await.unwrap_or_default();
    let rows: Vec<PolicyRow> = policies
        .into_iter()
        .map(|p| PolicyRow {
            server: SharedString::from(p.server),
            tool: SharedString::from(p.tool),
            decision: SharedString::from(p.decision.as_db_str()),
            source: SharedString::from(p.decision_source.as_db_str()),
        })
        .collect();
    weak.upgrade_in_event_loop(move |ui| {
        ui.set_policies(ModelRc::new(VecModel::from(rows)));
    })
    .ok();
}

async fn refresh_audit(weak: &slint::Weak<MainWindow>, db: &Db) {
    let filter = mcp_router::daemon::db::types::AuditFilter {
        limit: Some(100),
        ..Default::default()
    };
    let entries = db.query_audit_log(filter).await.unwrap_or_default();
    let rows: Vec<AuditRow> = entries
        .into_iter()
        .map(|e| AuditRow {
            ts_label: SharedString::from(e.ts.to_string()),
            ide_name: SharedString::from(e.ide_name.unwrap_or_else(|| "?".to_string())),
            server: SharedString::from(e.server),
            tool: SharedString::from(e.tool.unwrap_or_default()),
            decision: SharedString::from(e.decision.as_db_str()),
            latency_label: SharedString::from(
                e.latency_ms.map(|n| format!("{n}ms")).unwrap_or_default(),
            ),
        })
        .collect();
    weak.upgrade_in_event_loop(move |ui| {
        ui.set_audit(ModelRc::new(VecModel::from(rows)));
    })
    .ok();
}

async fn lookup_ide_names_for_keys<'a>(
    db: &Db,
    keys: impl Iterator<Item = &'a str>,
) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for key in keys {
        if !out.contains_key(key) {
            if let Ok(Some(ik)) = db.lookup_ide_key(key).await {
                out.insert(ik.key, ik.ide_name);
            }
        }
    }
    out
}

fn lifecycle_label(secs: i32) -> String {
    match secs {
        -1 => "persistent".to_string(),
        0 => "ephemeral".to_string(),
        n => format!("{n}s idle"),
    }
}

fn settings_to_row(s: &Settings) -> SettingsRow {
    SettingsRow {
        port: SharedString::from(s.daemon.port.to_string()),
        approval_timeout: SharedString::from(s.daemon.approval_timeout_seconds.to_string()),
        default_lifecycle: SharedString::from(s.daemon.default_lifecycle_seconds.to_string()),
        audit_retention: SharedString::from(s.daemon.audit_retention_days.to_string()),
        theme: SharedString::from(s.gui.theme.clone()),
        autostart_at_login: SharedString::from(s.daemon.autostart_at_login.to_string()),
        notifications_enabled: SharedString::from(s.notifications.enabled.to_string()),
    }
}
