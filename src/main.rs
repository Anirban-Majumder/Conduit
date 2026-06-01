//! Conduit binary entrypoint: bootstrap, then hand the main thread to Slint.
//!
//! `main` wires every subsystem — keyring backend, paths, settings, the tokio
//! worker, DB actor, event bus, HTTP server, notifications, IPC, tray, and
//! hotkey — builds the `MainWindow`, installs window callbacks, then runs the
//! Slint event loop. Shutdown order is load-bearing: the event loop returns,
//! the cancel token fires, then the worker is dropped (runtime shutdown) before
//! the keyring store is released.

#![warn(rustdoc::broken_intra_doc_links)]
#![warn(rustdoc::private_intra_doc_links)]
// reason: this is a binary crate (nothing is exported), so the window
// submodules under `window` are private; `pub(crate)` is the correct visibility
// for cross-module access between them, but `redundant_pub_crate` flags it as
// redundant against the private parent. `pub` is not an option — it would trip
// `unreachable_pub`. The two lints conflict for cross-module bin code.
#![expect(
    clippy::redundant_pub_crate,
    reason = "bin crate: window submodules need pub(crate) for cross-module access"
)]

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use slint::ComponentHandle;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

use conduit::daemon::config::toml::Settings;
use conduit::daemon::config::watcher;
use conduit::daemon::db::Db;
use conduit::daemon::downstream::approval::{spawn_timeout_sweeper, ApprovalCenter};
use conduit::daemon::downstream::http_server::{self, AppState};
use conduit::daemon::events::Bus;
use conduit::daemon::ipc;
use conduit::daemon::onboarding::{
    check_background_hint, should_show_window_at_startup, BackgroundHintAction,
};
use conduit::daemon::paths::Paths;
use conduit::daemon::skills::SkillManager;
use conduit::daemon::upstream::manager::UpstreamManager;
use conduit::os::{
    hotkey, notification_router as os_notification_router, notifications as os_notifications,
    tray as os_tray,
};
use conduit::runtime::Worker;

pub(crate) mod window;
use window::callbacks::install_callbacks;
use window::refresh::{push_initial_models, spawn_event_bridge};
use window::util::fire_background_hint;
use window::MainWindow;

#[expect(
    clippy::too_many_lines,
    reason = "single-binary entrypoint wires every subsystem"
)]
#[expect(
    clippy::cognitive_complexity,
    reason = "single-binary entrypoint: init + IPC + DB + HTTP + tray + event loop"
)]
fn main() -> Result<()> {
    init_tracing();
    tracing::info!("conduit starting");

    // keyring v4 / keyring-core requires registering a backend before any
    // Entry::new(); `true` picks Secret Service over kernel keyutils on Linux,
    // and is a no-op selector on macOS (Keychain) and Windows (Cred Manager).
    keyring::use_native_store(true).context("activate OS keyring backend")?;

    let paths = Paths::from_user_dirs().context("resolve conduit config dir")?;
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
                eprintln!("conduit: already running — opened existing window.");
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

    // Purge orphaned OAuth draft tokens from abandoned wizards, and clear any
    // pre-migration token entries for saved servers (D6 — the old 4-entry
    // format is unreadable now, so those servers re-login; this just tidies the
    // stale keyring entries. Idempotent: it only removes legacy suffixes, never
    // the current `:cred` blob).
    {
        let db_purge = db.clone();
        worker.handle().spawn(async move {
            match db_purge.list_oauth_drafts().await {
                Ok(drafts) if !drafts.is_empty() => {
                    let drafts_for_purge = drafts.clone();
                    let purged = match tokio::task::spawn_blocking(move || {
                        conduit::daemon::upstream::oauth::purge_draft_oauth_tokens(
                            &drafts_for_purge,
                        )
                    })
                    .await
                    {
                        Ok(n) => n,
                        Err(e) => {
                            tracing::warn!(error = %e, "purge_draft_oauth_tokens task panicked");
                            0
                        }
                    };
                    if purged > 0 {
                        tracing::info!(count = purged, "purged orphaned OAuth draft tokens");
                    }
                    // Clear the tracking entries regardless — tokens are gone.
                    for d in &drafts {
                        if let Err(e) = db_purge.delete_oauth_draft(d.clone()).await {
                            tracing::debug!(draft = %d, error = %e, "clear draft tracking failed");
                        }
                    }
                }
                _ => {}
            }
            if let Ok(servers) = db_purge.list_servers().await {
                let names: Vec<String> = servers.into_iter().map(|s| s.name).collect();
                let cleanup = tokio::task::spawn_blocking(move || {
                    for name in &names {
                        if let Err(e) = conduit::daemon::secrets::delete_legacy_oauth_entries(name) {
                            tracing::debug!(server = %name, error = %e, "legacy OAuth cleanup failed");
                        }
                    }
                })
                .await;
                if let Err(e) = cleanup {
                    tracing::debug!(error = %e, "legacy OAuth cleanup task failed");
                }
            }
        });
    }

    let _watcher = {
        let _guard = worker.handle().enter();
        watcher::spawn(&paths.config_toml(), bus.clone(), cancel.clone())
            .context("spawn config watcher")?
    };

    let approval_timeout = Duration::from_secs(u64::from(settings.daemon.approval_timeout_seconds));
    let approvals = Arc::new(ApprovalCenter::new(
        db.clone(),
        bus.clone(),
        approval_timeout,
    ));
    let upstream = {
        let _guard = worker.handle().enter();
        Arc::new(UpstreamManager::with_bus(
            paths.clone(),
            cancel.clone(),
            Some(bus.clone()),
        ))
    };
    let config_toml_path = paths.config_toml();
    let skill_manager = {
        let _guard = worker.handle().enter();
        Arc::new(SkillManager::new(paths, db.clone(), bus.clone()).context("build skill manager")?)
    };

    let app_state = AppState {
        db: db.clone(),
        approvals: approvals.clone(),
        upstream,
        bus: bus.clone(),
        port: settings.daemon.port,
    };

    {
        let _guard = worker.handle().enter();
        let _sweeper =
            spawn_timeout_sweeper(db.clone(), bus.clone(), approval_timeout, cancel.clone());
        let http_cancel = cancel.clone();
        tokio::spawn(async move {
            if let Err(e) = http_server::serve(app_state, http_cancel).await {
                tracing::error!(error = %e, "axum server exited with error");
            }
        });
    }

    {
        let _guard = worker.handle().enter();
        conduit::daemon::retention::spawn(db.clone(), config_toml_path, cancel.clone());
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
        cancel.clone(),
    );

    // Notifications bridge: emits OS toasts AND owns the click→dispatch loop.
    {
        let _guard = worker.handle().enter();
        // reason: bounded at 256; on full, sender uses try_send + drop (drop-on-full, not back-pressure — see notifications.rs)
        let (action_tx, mut action_rx) =
            tokio::sync::mpsc::channel::<os_notification_router::NotificationAction>(256);
        let _notif = os_notifications::spawn(
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
                        os_notification_router::dispatch_action(
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
    tracing::info!("conduit stopped");
    drop(bus);
    drop(worker);
    keyring::release_store();
    Ok(())
}

fn install_tray(
    db: &Db,
    bus: &Bus,
    cancel: &CancellationToken,
    window: &MainWindow,
    rt: tokio::runtime::Handle,
) -> Option<os_tray::TrayHandle> {
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

    let callbacks = os_tray::TrayCallbacks {
        on_show,
        on_quit,
        on_pause_toggle,
    };

    os_tray::install(db.clone(), rt, bus.subscribe(), cancel.clone(), callbacks)
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
