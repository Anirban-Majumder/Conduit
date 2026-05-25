// OS notifications via notify-rust. Spec §"Approval UX".
//
// No body args / preview — privacy-safe default. Clicking the notification
// surfaces the GUI (action buttons are per-OS-backend-specific and deferred
// until the macOS code-signing story lands in v0.5+).

use std::sync::Arc;

use tokio::sync::broadcast::error::RecvError;
use tokio::sync::broadcast::Receiver;
use tokio_util::sync::CancellationToken;

use crate::daemon::config_toml::Settings;
use crate::daemon::db::Db;
use crate::daemon::events::Event;

/// Spawn a tokio task that listens to the bus and fires an OS notification
/// for every `ApprovalPending` event.
///
/// Honours `settings.notifications.enabled` (read once at spawn time).
pub fn spawn(
    mut rx: Receiver<Event>,
    db: Db,
    settings: Arc<Settings>,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if !settings.notifications.enabled {
            tracing::info!("notifications disabled in config; bridge idle");
            return;
        }
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                ev = rx.recv() => match ev {
                    Ok(Event::ApprovalPending { ide_key, server, tool, .. }) => {
                        let ide_name = match db.lookup_ide_key(&ide_key).await {
                            Ok(Some(k)) => k.ide_name,
                            Ok(None) | Err(_) => "Unknown IDE".to_string(),
                        };
                        let body = format!("{server}.{tool}");
                        // notify-rust's show() does a sync dbus call on Linux;
                        // hop to a blocking pool so we don't stall the bridge.
                        if let Err(e) = tokio::task::spawn_blocking(move || {
                            notify_rust::Notification::new()
                                .summary(&ide_name)
                                .body(&body)
                                .appname("mcp-router")
                                .icon("dialog-information")
                                .show()
                                .map(|_| ())
                        })
                        .await
                        {
                            tracing::warn!(error = %e, "notification spawn_blocking joined with error");
                        }
                    }
                    Ok(_) => {}
                    Err(RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "notifications bridge lagged");
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        }
    })
}
