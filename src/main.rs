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
use mcp_router::daemon::db::types::{ApprovalStatus, Decision, DecisionSource};
use mcp_router::daemon::db::Db;
use mcp_router::daemon::events::{Bus, Event};
use mcp_router::daemon::http_server::{self, AppState};
use mcp_router::daemon::upstream::manager::UpstreamManager;
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
    let upstream = Arc::new(UpstreamManager::new(paths, cancel.clone()));

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
    install_callbacks(&window, &db, &approvals, worker.handle().clone());
    push_initial_models(&window, &db, &settings, worker.handle().clone());
    spawn_event_bridge(&window, bus.subscribe(), db, worker.handle().clone());

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

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

fn install_callbacks(
    window: &MainWindow,
    db: &Db,
    approvals: &Arc<ApprovalCenter>,
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
    let rt_for_policy = rt;
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
