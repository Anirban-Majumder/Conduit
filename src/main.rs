#![warn(rustdoc::broken_intra_doc_links)]
#![warn(rustdoc::private_intra_doc_links)]

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

use mcp_router::daemon::approval::{spawn_timeout_sweeper, ApprovalCenter};
use mcp_router::daemon::config_toml::Settings;
use mcp_router::daemon::config_watcher;
use mcp_router::daemon::db::Db;
use mcp_router::daemon::events::Bus;
use mcp_router::daemon::http_server::{self, AppState};
use mcp_router::daemon::upstream::manager::UpstreamManager;
use mcp_router::paths::Paths;
use mcp_router::runtime::Worker;

slint::slint! {
    export component MainWindow inherits Window {
        title: "MCP Router";
        preferred-width: 720px;
        preferred-height: 480px;
        background: #0a0a0a;

        Text {
            text: "mcp-router — daemon running";
            color: #c8c8c8;
            font-size: 18px;
            horizontal-alignment: center;
            vertical-alignment: center;
            width: parent.width;
            height: parent.height;
        }
    }
}

fn main() -> Result<()> {
    init_tracing();
    tracing::info!("mcp-router starting");

    let paths = Paths::from_user_dirs().context("resolve mcp-router config dir")?;
    paths.ensure_created().context("create state dirs")?;
    tracing::info!(root = %paths.root().display(), "state root ready");

    let settings = Settings::load_or_default(&paths.config_toml());
    let port = settings.daemon.port;
    let approval_timeout = Duration::from_secs(u64::from(settings.daemon.approval_timeout_seconds));
    tracing::info!(port, "settings loaded");

    let cancel = CancellationToken::new();
    let worker = Worker::spawn(cancel.clone()).context("spawn tokio worker")?;
    let db = Db::spawn_at(&paths.state_db()).context("spawn db actor")?;
    let bus = Bus::new();

    let _watcher = {
        let _guard = worker.handle().enter();
        config_watcher::spawn(&paths.config_toml(), bus.clone(), cancel.clone())
            .context("spawn config watcher")?
    };

    let approvals = Arc::new(ApprovalCenter::new(
        db.clone(),
        bus.clone(),
        approval_timeout,
    ));
    let upstream = Arc::new(UpstreamManager::new(paths, cancel.clone()));

    let state = AppState {
        db: db.clone(),
        approvals: approvals.clone(),
        upstream: upstream.clone(),
        port,
    };

    // Spawn HTTP server + approval-timeout sweeper into the tokio worker.
    {
        let _guard = worker.handle().enter();
        let server_cancel = cancel.clone();
        tokio::spawn(async move {
            if let Err(e) = http_server::serve(state, server_cancel).await {
                tracing::error!(error = %e, "axum server returned");
            }
        });
        spawn_timeout_sweeper(db.clone(), approval_timeout, cancel.clone());
    }

    let window = MainWindow::new().context("build slint main window")?;
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
    drop(approvals);
    drop(upstream);
    drop(db);
    drop(worker);
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
