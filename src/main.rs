#![warn(rustdoc::broken_intra_doc_links)]
#![warn(rustdoc::private_intra_doc_links)]

use anyhow::{Context, Result};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

use mcp_router::daemon::config_toml::Settings;
use mcp_router::daemon::config_watcher;
use mcp_router::daemon::db::Db;
use mcp_router::daemon::events::Bus;
use mcp_router::paths::Paths;
use mcp_router::runtime::Worker;

slint::slint! {
    export component MainWindow inherits Window {
        title: "MCP Router";
        preferred-width: 720px;
        preferred-height: 480px;
        background: #0a0a0a;

        Text {
            text: "mcp-router — foundation";
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
    tracing::info!(port = settings.daemon.port, "settings loaded");

    let cancel = CancellationToken::new();
    let worker = Worker::spawn(cancel.clone()).context("spawn tokio worker")?;
    let _db = Db::spawn_at(&paths.state_db()).context("spawn db actor")?;
    let bus = Bus::new();

    let _watcher = {
        let _guard = worker.handle().enter();
        config_watcher::spawn(&paths.config_toml(), bus.clone(), cancel.clone())
            .context("spawn config watcher")?
    };

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
    drop(worker);
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
