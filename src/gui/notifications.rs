//! OS notification bridge: emits a toast for every pending approval.
//!
//! Listens on the event bus and spawns one blocking `std::thread` per toast
//! (notify-rust's action wait blocks). User clicks are forwarded as
//! `NotificationAction`s to `notification_router`. Linux registers Approve /
//! Deny / Approve-always actions; macOS/Windows fall back to fire-and-forget
//! (those backends silently drop unregistered actions).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast::error::RecvError;
use tokio::sync::broadcast::Receiver;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::daemon::config_toml::Settings;
use crate::daemon::db::Db;
use crate::daemon::events::Event;
use crate::gui::notification_router::NotificationAction;

type Registry = Arc<Mutex<HashMap<String, u32>>>;

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
        let registry: Registry = Arc::new(Mutex::new(HashMap::new()));
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
                            registry.clone(),
                        );
                    }
                    Ok(Event::ApprovalResolved { id, .. }) => {
                        let notif_id = {
                            let mut map = match registry.lock() {
                                Ok(g) => g,
                                Err(p) => p.into_inner(),
                            };
                            map.remove(&id)
                        };
                        if let Some(nid) = notif_id {
                            std::thread::Builder::new()
                                .name(format!("notif-close-{nid}"))
                                .spawn(move || close_by_id(nid))
                                .ok();
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

fn close_by_id(notif_id: u32) {
    let _result = notify_rust::Notification::new()
        .id(notif_id)
        .summary("")
        .body("")
        .timeout(notify_rust::Timeout::Milliseconds(1))
        .show()
        .map(notify_rust::NotificationHandle::close);
}

#[cfg(target_os = "linux")]
fn spawn_notification(
    id: String,
    ide_name: String,
    server: String,
    tool: String,
    action_tx: UnboundedSender<NotificationAction>,
    registry: Registry,
) {
    std::thread::Builder::new()
        .name(format!("notif-{id}"))
        .spawn(move || {
            let body = format!("{server} · {tool}");
            let handle = match notify_rust::Notification::new()
                .summary(&ide_name)
                .body(&body)
                .appname("Conduit")
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
            if let Ok(mut map) = registry.lock() {
                map.insert(id.clone(), handle.id());
            }
            let send = action_tx;
            let approval_id = id.clone();
            handle.wait_for_action(|action_id| {
                let msg = match action_id {
                    "default" => Some(NotificationAction::Focus(approval_id.clone())),
                    "approve" => Some(NotificationAction::Approve(approval_id.clone())),
                    "deny" => Some(NotificationAction::Deny(approval_id.clone())),
                    "approve_always" => Some(NotificationAction::ApproveAlways {
                        id: approval_id.clone(),
                        server: server.clone(),
                        tool: tool.clone(),
                    }),
                    _ => None,
                };
                if let Some(m) = msg {
                    let _ignored = send.send(m);
                }
            });
            if let Ok(mut map) = registry.lock() {
                map.remove(&id);
            }
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
    _registry: Registry,
) {
    let body = format!("{server} · {tool}");
    if let Err(e) = notify_rust::Notification::new()
        .summary(&ide_name)
        .body(&body)
        .appname("Conduit")
        .show()
    {
        tracing::warn!(error = %e, "notification show failed");
    }
}
