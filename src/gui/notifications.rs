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
use tokio::sync::mpsc::Sender;
use tokio::sync::Semaphore;
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
///
/// Windows limitation: `close_by_id` is a no-op because `WinRT` exposes no
/// close handle. Toasts resolved in-GUI before their timeout stay on screen
/// until self-dismiss. The timeout is also coerced — `WinRT` only supports
/// ~7s (short) or ~25s (long); our 10s falls to short.
pub fn spawn(
    mut rx: Receiver<Event>,
    db: Db,
    settings: Arc<Settings>,
    cancel: CancellationToken,
    action_tx: Sender<NotificationAction>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if !settings.notifications.enabled {
            tracing::info!("notifications disabled in config; bridge idle");
            return;
        }
        let registry: Registry = Arc::new(Mutex::new(HashMap::new()));
        // reason: caps close threads spawned per ApprovalResolved burst; bounded loop rule
        let close_sem = Arc::new(Semaphore::new(32));
        // reason: caps concurrent pending notification threads (Linux); bounded loop rule
        let notif_sem = Arc::new(Semaphore::new(64));
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                ev = rx.recv() => match ev {
                    Ok(Event::ApprovalPending { id, ide_key, server, tool, .. }) => {
                        let ide_name = match db.lookup_ide_key(&ide_key).await {
                            Ok(Some(k)) => k.ide_name,
                            Ok(None) => {
                                tracing::debug!(ide_key, "lookup_ide_key returned None; using fallback name");
                                "Unknown IDE".to_string()
                            }
                            Err(e) => {
                                tracing::warn!(ide_key, error = %e, "lookup_ide_key failed; using fallback name");
                                "Unknown IDE".to_string()
                            }
                        };
                        spawn_notification(
                            id,
                            ide_name,
                            server,
                            tool,
                            action_tx.clone(),
                            registry.clone(),
                            notif_sem.clone(),
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
                            match close_sem.clone().try_acquire_owned() {
                                Ok(permit) => {
                                    std::thread::Builder::new()
                                        .name(format!("notif-close-{nid}"))
                                        .spawn(move || {
                                            close_by_id(nid);
                                            drop(permit);
                                        })
                                        .ok();
                                }
                                Err(_) => {
                                    tracing::warn!(notif_id = nid, "notification close semaphore full; skipping close");
                                }
                            }
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

#[cfg(not(target_os = "windows"))]
fn close_by_id(notif_id: u32) {
    let _result = notify_rust::Notification::new()
        .id(notif_id)
        .summary("")
        .body("")
        .timeout(notify_rust::Timeout::Milliseconds(1))
        .show()
        .map(notify_rust::NotificationHandle::close);
}

#[cfg(target_os = "windows")]
const fn close_by_id(_notif_id: u32) {
    // Windows notifications via WinRT don't expose a close handle.
}

#[cfg(target_os = "linux")]
fn spawn_notification(
    id: String,
    ide_name: String,
    server: String,
    tool: String,
    action_tx: Sender<NotificationAction>,
    registry: Registry,
    sem: Arc<Semaphore>,
) {
    let Ok(permit) = sem.try_acquire_owned() else {
        tracing::warn!(approval = %id, "notification semaphore full; dropping pending notification");
        return;
    };
    std::thread::Builder::new()
        .name(format!("notif-{id}"))
        .spawn(move || {
            let _permit = permit; // released when thread exits (user responds or notification dismissed)
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
                    _ => {
                        tracing::warn!(action_id, "unrecognised notification action; ignoring");
                        None
                    }
                };
                if let Some(m) = msg {
                    if let Err(e) = send.try_send(m) {
                        tracing::warn!(error = %e, "notification action channel full; action dropped");
                    }
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
    _action_tx: Sender<NotificationAction>,
    _registry: Registry,
    _sem: Arc<Semaphore>,
) {
    let body = format!("{server} · {tool}");
    let mut notif = notify_rust::Notification::new();
    notif.summary(&ide_name).body(&body).appname("Conduit");
    // Windows: auto-dismiss after 10s since we can't programmatically close.
    #[cfg(target_os = "windows")]
    // WinRT only supports short (~7s) or long (~25s); 10s coerces to short.
    notif.timeout(notify_rust::Timeout::Milliseconds(10_000));
    if let Err(e) = notif.show() {
        tracing::warn!(error = %e, "notification show failed");
    }
}
