//! OS notification bridge: emits a toast for every pending approval.
//!
//! Listens on the event bus and spawns one blocking `std::thread` per toast
//! (notify-rust's action wait blocks). User clicks are forwarded as
//! `NotificationAction`s to `notification_router`. Linux registers Approve /
//! Deny actions; macOS/Windows fall back to fire-and-forget
//! (those backends silently drop unregistered actions).
//!
//! The `registry` tracks each on-screen toast so `ApprovalResolved` can dismiss
//! it (`close_by_id`) and so a clean shutdown can clear them. The close path is
//! Linux-only: notify-rust's rich `NotificationHandle` (`id()` / action wait)
//! exists only on the XDG backend; elsewhere toasts are fire-and-forget.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::broadcast::error::RecvError;
use tokio::sync::broadcast::Receiver;
use tokio::sync::mpsc::Sender;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::daemon::config::SharedSettings;
use crate::daemon::db::Db;
use crate::daemon::events::Event;
use crate::os::notification_router::NotificationAction;

type Registry = Arc<Mutex<HashMap<String, ToastState>>>;

/// Lifecycle of one approval toast, keyed by approval id in the `Registry`.
// reason: the close-by-id lifecycle only runs on Linux — notify-rust's rich
// NotificationHandle (id()/wait_for_action) is Linux/BSD-only. On other targets
// the registry stays empty, so Pending/Shown are never constructed there.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug)]
enum ToastState {
    /// Toast thread spawned; `show()` has not completed yet.
    Pending,
    /// Toast is on screen; the value is the server id used to close it.
    Shown(u32),
    /// Resolved before `show()` completed; the toast thread will self-close.
    Closing,
}

fn lock(registry: &Registry) -> MutexGuard<'_, HashMap<String, ToastState>> {
    match registry.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

/// Reserve a registry slot for a toast that is about to be shown. Done before
/// the toast thread runs so a fast `ApprovalResolved` can't race past it.
#[cfg(target_os = "linux")]
fn insert_pending(registry: &Registry, id: &str) {
    lock(registry).insert(id.to_string(), ToastState::Pending);
}

/// Promote a `Pending` toast to `Shown`. Returns `false` if it was resolved
/// while showing (`Closing`), meaning the caller must close it immediately.
#[cfg(target_os = "linux")]
fn register_shown(registry: &Registry, id: &str, notif_id: u32) -> bool {
    let mut map = lock(registry);
    if matches!(map.get(id), Some(ToastState::Closing)) {
        map.remove(id);
        false
    } else {
        map.insert(id.to_string(), ToastState::Shown(notif_id));
        true
    }
}

#[cfg(target_os = "linux")]
fn unregister(registry: &Registry, id: &str) {
    lock(registry).remove(id);
}

/// React to an approval resolving: return the toast's server id if it's on
/// screen (close it), or flag a not-yet-shown toast to self-close. Unknown ids
/// (e.g. on the fire-and-forget backends) are a no-op — no tombstone is left.
fn mark_resolved(registry: &Registry, id: &str) -> Option<u32> {
    let mut map = lock(registry);
    match map.remove(id) {
        Some(ToastState::Shown(notif_id)) => Some(notif_id),
        Some(ToastState::Pending) => {
            map.insert(id.to_string(), ToastState::Closing);
            None
        }
        Some(ToastState::Closing) | None => None,
    }
}

/// Take every on-screen toast id and clear the registry (shutdown path).
fn drain_shown(registry: &Registry) -> Vec<u32> {
    let mut map = lock(registry);
    let ids = map
        .values()
        .filter_map(|state| match state {
            ToastState::Shown(notif_id) => Some(*notif_id),
            ToastState::Pending | ToastState::Closing => None,
        })
        .collect();
    map.clear();
    ids
}

/// Dismiss a toast by id on a bounded close thread.
fn request_close(notif_id: u32, close_sem: &Arc<Semaphore>) {
    match close_sem.clone().try_acquire_owned() {
        Ok(permit) => {
            std::thread::Builder::new()
                .name(format!("notif-close-{notif_id}"))
                .spawn(move || {
                    close_by_id(notif_id);
                    drop(permit);
                })
                .ok();
        }
        Err(_) => {
            tracing::warn!(
                notif_id,
                "notification close semaphore full; skipping close"
            );
        }
    }
}

/// Resolve the IDE display name for a toast title, falling back when the client
/// can't be named. Approvals are only raised for tracked clients, so the
/// fallback is reached only by an un-track mid-flight or a blank stored name.
async fn resolve_ide_name(db: &Db, client_key: &str) -> String {
    const FALLBACK: &str = "Unknown IDE";
    match db.lookup_client_key(client_key).await {
        Ok(Some(client)) if !client.name.is_empty() => client.name,
        Ok(_) => {
            tracing::debug!(client_key, "client name unavailable; using fallback");
            FALLBACK.to_string()
        }
        Err(e) => {
            tracing::warn!(client_key, error = %e, "lookup_client_key failed; using fallback name");
            FALLBACK.to_string()
        }
    }
}

/// Listen to the bus and fire an OS notification for every `ApprovalPending`.
///
/// On Linux registers three actions (body click + Approve / Deny) and forwards
/// user clicks to `action_tx`; the toast self-dismisses after the approval
/// window and is closed early when the approval resolves. On macOS / Windows
/// the notification is fire-and-forget — those backends drop unregistered
/// actions silently.
///
/// Windows limitation: `close_by_id` is a no-op because `WinRT` exposes no
/// close handle. Toasts resolved in-GUI before their timeout stay on screen
/// until self-dismiss. The timeout is also coerced — `WinRT` only supports
/// ~7s (short) or ~25s (long); our 10s falls to short.
pub fn spawn(
    mut rx: Receiver<Event>,
    db: Db,
    settings: SharedSettings,
    cancel: CancellationToken,
    action_tx: Sender<NotificationAction>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let registry: Registry = Arc::new(Mutex::new(HashMap::new()));
        // reason: caps std::thread spawns for close_by_id per ApprovalResolved burst (32 max)
        let close_sem = Arc::new(Semaphore::new(32));
        // reason: caps concurrent pending-approval notification threads on Linux (64 max)
        let notif_sem = Arc::new(Semaphore::new(64));
        loop {
            tokio::select! {
                () = cancel.cancelled() => {
                    // Dismiss live toasts so their blocked wait_for_action threads
                    // return instead of lingering past shutdown.
                    for notif_id in drain_shown(&registry) {
                        request_close(notif_id, &close_sem);
                    }
                    break;
                }
                ev = rx.recv() => match ev {
                    Ok(Event::ApprovalPending { id, client_key, mcp_server, tool, .. }) => {
                        // Read at use-time so a toggle in the Settings tab
                        // applies without a restart.
                        let (enabled, approval_timeout_secs) = {
                            let s = settings.load();
                            (s.notifications.enabled, s.daemon.approval_timeout_seconds)
                        };
                        if !enabled {
                            continue;
                        }
                        let ide_name = resolve_ide_name(&db, &client_key).await;
                        spawn_notification(
                            id,
                            ide_name,
                            mcp_server,
                            tool,
                            action_tx.clone(),
                            registry.clone(),
                            notif_sem.clone(),
                            approval_timeout_secs,
                        );
                    }
                    Ok(Event::ApprovalResolved { id, .. }) => {
                        if let Some(notif_id) = mark_resolved(&registry, &id) {
                            request_close(notif_id, &close_sem);
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
    approval_timeout_secs: u32,
) {
    // Action IDs + labels together so the registration loop and the dispatch
    // match below can't drift. ID "default" is freedesktop's body-click action;
    // "__closed" is the synthetic id notify-rust delivers on dismissal.
    const ACTION_DEFAULT: &str = "default";
    const ACTION_APPROVE: &str = "approve";
    const ACTION_DENY: &str = "deny";
    const ACTION_CLOSED: &str = "__closed";
    const NOTIFICATION_ACTIONS: &[(&str, &str)] = &[
        (ACTION_DEFAULT, ""),
        (ACTION_APPROVE, "Approve"),
        (ACTION_DENY, "Deny"),
    ];

    let Ok(permit) = sem.try_acquire_owned() else {
        tracing::warn!(approval = %id, "notification semaphore full; dropping pending notification");
        return;
    };
    // Reserve the slot synchronously, before the toast thread runs, so a fast
    // ApprovalResolved can't race ahead and miss the not-yet-shown toast.
    insert_pending(&registry, &id);

    std::thread::Builder::new()
        .name(format!("notif-{id}"))
        .spawn(move || {
            let _permit = permit; // released when thread exits (user responds or notification dismissed)
            let body = format!("{server} · {tool}");
            let mut notif = notify_rust::Notification::new();
            notif
                .summary(&ide_name)
                .body(&body)
                .appname("Conduit")
                .icon("dialog-information")
                .timeout(notify_rust::Timeout::Milliseconds(
                    approval_timeout_secs.saturating_mul(1000),
                ));
            for &(action_id, label) in NOTIFICATION_ACTIONS {
                notif.action(action_id, label);
            }
            let handle = match notif.show() {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!(error = %e, "notification show failed");
                    unregister(&registry, &id);
                    return;
                }
            };
            if !register_shown(&registry, &id, handle.id()) {
                // Resolved while we were showing — dismiss immediately.
                handle.close();
                return;
            }
            let approval_id = id.clone();
            handle.wait_for_action(|action_id| {
                let msg = match action_id {
                    ACTION_DEFAULT => Some(NotificationAction::Focus(approval_id)),
                    ACTION_APPROVE => Some(NotificationAction::Approve(approval_id)),
                    ACTION_DENY => Some(NotificationAction::Deny(approval_id)),
                    ACTION_CLOSED => None,
                    _ => {
                        tracing::warn!(action_id, "unrecognised notification action; ignoring");
                        None
                    }
                };
                if let Some(m) = msg {
                    if let Err(e) = action_tx.try_send(m) {
                        tracing::warn!(error = %e, "notification action channel full; action dropped");
                    }
                }
            });
            unregister(&registry, &id);
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
    _approval_timeout_secs: u32,
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::{drain_shown, insert_pending, lock, mark_resolved, register_shown, Registry};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    fn registry() -> Registry {
        Arc::new(Mutex::new(HashMap::new()))
    }

    #[test]
    fn shown_then_resolved_returns_id_to_close() {
        let reg = registry();
        insert_pending(&reg, "a");
        assert!(register_shown(&reg, "a", 42));
        assert_eq!(mark_resolved(&reg, "a"), Some(42));
        assert!(lock(&reg).is_empty());
    }

    #[test]
    fn resolved_before_shown_makes_toast_self_close() {
        let reg = registry();
        insert_pending(&reg, "b");
        // Resolve arrives before show() completes: nothing to close yet...
        assert_eq!(mark_resolved(&reg, "b"), None);
        // ...and the toast, once shown, sees Closing and self-closes.
        assert!(!register_shown(&reg, "b", 7));
        assert!(lock(&reg).is_empty());
    }

    #[test]
    fn resolve_unknown_id_leaves_no_tombstone() {
        let reg = registry();
        assert_eq!(mark_resolved(&reg, "missing"), None);
        assert!(lock(&reg).is_empty());
    }

    #[test]
    fn drain_collects_only_shown_toasts() {
        let reg = registry();
        insert_pending(&reg, "pending-only");
        insert_pending(&reg, "shown");
        register_shown(&reg, "shown", 9);
        assert_eq!(drain_shown(&reg), vec![9]);
        assert!(lock(&reg).is_empty());
    }
}
