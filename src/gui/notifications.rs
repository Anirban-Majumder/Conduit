use std::sync::Arc;

use tokio::sync::broadcast::error::RecvError;
use tokio::sync::broadcast::Receiver;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::daemon::config_toml::Settings;
use crate::daemon::db::Db;
use crate::daemon::events::Event;
use crate::gui::notification_router::NotificationAction;

/// Listen to the bus and fire an OS notification for every `ApprovalPending`.
///
/// On Linux registers four actions (body click + Approve / Deny / Approve
/// always) and forwards user clicks to `action_tx`. On macOS / Windows the
/// notification is fire-and-forget — those backends drop unregistered
/// actions silently.
pub fn spawn(
    mut rx: Receiver<Event>,
    db: Db,
    settings: Arc<Settings>,
    cancel: CancellationToken,
    action_tx: UnboundedSender<NotificationAction>,
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
                    Ok(Event::ApprovalPending { id, ide_key, server, tool, .. }) => {
                        let ide_name = match db.lookup_ide_key(&ide_key).await {
                            Ok(Some(k)) => k.ide_name,
                            Ok(None) | Err(_) => "Unknown IDE".to_string(),
                        };
                        spawn_notification(
                            id,
                            ide_name,
                            server,
                            tool,
                            action_tx.clone(),
                        );
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

#[cfg(target_os = "linux")]
fn spawn_notification(
    id: String,
    ide_name: String,
    server: String,
    tool: String,
    action_tx: UnboundedSender<NotificationAction>,
) {
    std::thread::Builder::new()
        .name(format!("notif-{id}"))
        .spawn(move || {
            let body = format!("{server} · {tool}");
            let handle = match notify_rust::Notification::new()
                .summary(&ide_name)
                .body(&body)
                .appname("mcp-router")
                .icon("dialog-information")
                .action("default", "")
                .action("approve", "Approve")
                .action("deny", "Deny")
                .action("approve_always", "Approve always")
                .show()
            {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!(error = %e, "notification show failed");
                    return;
                }
            };
            let send = action_tx;
            handle.wait_for_action(|action_id| {
                let msg = match action_id {
                    "default" => Some(NotificationAction::Focus(id.clone())),
                    "approve" => Some(NotificationAction::Approve(id.clone())),
                    "deny" => Some(NotificationAction::Deny(id.clone())),
                    "approve_always" => Some(NotificationAction::ApproveAlways {
                        id: id.clone(),
                        server: server.clone(),
                        tool: tool.clone(),
                    }),
                    _ => None,
                };
                if let Some(m) = msg {
                    let _ignored = send.send(m);
                }
            });
        })
        .ok();
}

#[cfg(not(target_os = "linux"))]
fn spawn_notification(
    _id: String,
    ide_name: String,
    server: String,
    tool: String,
    _action_tx: UnboundedSender<NotificationAction>,
) {
    let body = format!("{server} · {tool}");
    if let Err(e) = notify_rust::Notification::new()
        .summary(&ide_name)
        .body(&body)
        .appname("mcp-router")
        .show()
    {
        tracing::warn!(error = %e, "notification show failed");
    }
}
