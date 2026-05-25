#![warn(rustdoc::broken_intra_doc_links)]
#![warn(rustdoc::private_intra_doc_links)]

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

use mcp_router::daemon::approval::{spawn_timeout_sweeper, ApprovalCenter};
use mcp_router::daemon::config_toml::Settings;
use mcp_router::daemon::config_watcher;
use mcp_router::daemon::db::types::{
    ApprovalStatus, Decision, DecisionSource, EnvEntry, EnvValue, ManagedIdeConfig, Server,
    TRANSPORT_HTTP, TRANSPORT_STDIO,
};
use mcp_router::daemon::db::Db;
use mcp_router::daemon::events::{Bus, Event};
use mcp_router::daemon::http_server::{self, AppState};
use mcp_router::daemon::ide_detect::IdeKind;
use mcp_router::daemon::ide_writer;
use mcp_router::daemon::paste_parser;
use mcp_router::daemon::upstream::manager::UpstreamManager;
use mcp_router::gui::{hotkey, notifications as gui_notifications};
use mcp_router::paths::Paths;
use mcp_router::runtime::Worker;

mod ui;
use ui::{ApprovalRow, AuditRow, IdeRow, MainWindow, PolicyRow, ServerRow, SettingsRow};

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
        Arc::new(UpstreamManager::new(paths, cancel.clone()))
    };

    let app_state = AppState {
        db: db.clone(),
        approvals: approvals.clone(),
        upstream,
        port: settings.daemon.port,
    };

    {
        let _guard = worker.handle().enter();
        let _sweeper = spawn_timeout_sweeper(db.clone(), approval_timeout, cancel.clone());
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
        settings.daemon.port,
        worker.handle().clone(),
    );
    push_initial_models(&window, &db, &settings, worker.handle().clone());
    let settings_arc = Arc::new(settings);
    spawn_event_bridge(
        &window,
        bus.subscribe(),
        db.clone(),
        worker.handle().clone(),
    );

    // Notifications bridge — fires an OS notification on every ApprovalPending.
    {
        let _guard = worker.handle().enter();
        let _notif = gui_notifications::spawn(bus.subscribe(), db, settings_arc, cancel.clone());
    }

    // Tray-icon deferred (its libxdo system dep is not universally available).
    // Spec §"Open questions" already designates the global hotkey as the
    // Wayland-no-SNI fallback; we ship it as the primary entry point in v0.
    let _hotkey = install_hotkey(&window);

    let cancel_for_close = cancel.clone();
    window.window().on_close_requested(move || {
        tracing::info!("window close requested");
        cancel_for_close.cancel();
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
    port: u16,
    rt: tokio::runtime::Handle,
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
    let rt_for_remove_server = rt.clone();
    window.on_remove_server_clicked(move |name| {
        let db = db_for_remove_server.clone();
        rt_for_remove_server.spawn(async move {
            if let Err(e) = db.delete_server(name.as_str()).await {
                tracing::warn!(error = %e, "delete server failed");
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

    // Parse paste — read the textarea, run paste_parser, fill the form fields.
    let weak_for_paste = window.as_weak();
    window.on_parse_paste(move || {
        let Some(ui) = weak_for_paste.upgrade() else {
            return;
        };
        let raw = ui.get_add_server_paste().to_string();
        match paste_parser::parse(&raw) {
            Ok(parsed) => {
                if let Some(n) = parsed.name {
                    ui.set_add_server_name(SharedString::from(n));
                }
                if let Some(c) = parsed.command {
                    ui.set_add_server_command(SharedString::from(c));
                }
                if !parsed.args.is_empty() {
                    ui.set_add_server_args(SharedString::from(parsed.args.join(" ")));
                }
                // Env vars from paste aren't surfaced into the form for v0
                // (no env grid in the dialog yet) — just log so user knows.
                if !parsed.env.is_empty() {
                    tracing::info!(
                        env_count = parsed.env.len(),
                        "paste contained env vars; env-var editor lands in a follow-up"
                    );
                }
            }
            Err(e) => tracing::warn!(error = %e, "paste parse failed"),
        }
    });

    let weak_for_save = window.as_weak();
    let db_for_save = db.clone();
    let rt_for_save = rt.clone();
    window.on_submit_add_server(move || {
        let Some(ui) = weak_for_save.upgrade() else {
            return;
        };
        let name = ui.get_add_server_name().to_string();
        let transport = ui.get_add_server_transport().to_string();
        if name.is_empty() {
            tracing::warn!("add-server: name is required");
            return;
        }
        let command = ui.get_add_server_command().to_string();
        let args_raw = ui.get_add_server_args().to_string();
        let url = ui.get_add_server_url().to_string();
        let server = Server {
            name,
            transport: transport.clone(),
            command: if transport == TRANSPORT_STDIO && !command.is_empty() {
                Some(command)
            } else {
                None
            },
            args: if args_raw.is_empty() {
                Vec::new()
            } else {
                args_raw.split_whitespace().map(str::to_string).collect()
            },
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
        };
        let db = db_for_save.clone();
        let weak = weak_for_save.clone();
        rt_for_save.spawn(async move {
            if let Err(e) = db.upsert_server(server).await {
                tracing::warn!(error = %e, "upsert_server failed");
            }
            refresh_servers(&weak, &db).await;
        });
    });

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

fn push_initial_models(
    window: &MainWindow,
    db: &Db,
    settings: &Settings,
    rt: tokio::runtime::Handle,
) {
    window.set_settings(settings_to_row(settings));

    let weak = window.as_weak();
    let db_for_refresh = db.clone();
    rt.spawn(async move {
        refresh_all(&weak, &db_for_refresh).await;
    });
}

fn spawn_event_bridge(
    window: &MainWindow,
    mut rx: tokio::sync::broadcast::Receiver<Event>,
    db: Db,
    rt: tokio::runtime::Handle,
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
    let rows: Vec<ServerRow> = servers
        .into_iter()
        .map(|s| ServerRow {
            name: SharedString::from(s.name),
            transport: SharedString::from(s.transport),
            lifecycle_label: SharedString::from(lifecycle_label(s.lifecycle_seconds)),
            status: SharedString::from("idle"),
            tool_count_label: SharedString::from("? tools"),
        })
        .collect();
    weak.upgrade_in_event_loop(move |ui| {
        ui.set_servers(ModelRc::new(VecModel::from(rows)));
    })
    .ok();
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
