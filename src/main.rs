#![warn(rustdoc::broken_intra_doc_links)]
#![warn(rustdoc::private_intra_doc_links)]

use std::path::PathBuf;

use anyhow::{Context, Result};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

use mcp_router::daemon::db::Db;
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

    let cancel = CancellationToken::new();
    let _worker = Worker::spawn(cancel.clone()).context("spawn tokio worker")?;
    let _db = Db::spawn_at(&state_db_path()?).context("spawn db actor")?;

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
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

// Stage 1 placeholder. Stage 2 swaps this for dirs::config_dir().join("mcp-router")
// per spec §Storage, alongside config.toml load + notify watcher.
fn state_db_path() -> Result<PathBuf> {
    let mut dir = std::env::temp_dir();
    dir.push("mcp-router-stage1");
    std::fs::create_dir_all(&dir).context("create state dir")?;
    Ok(dir.join("state.db"))
}
