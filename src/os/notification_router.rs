//! Dispatch notification-toast button clicks back into the daemon.
//!
//! `notifications.rs` forwards `NotificationAction`s here; this module resolves
//! the approval via `ApprovalCenter`. Dispatch is idempotent — stale ids are
//! no-ops because `resolve` checks the row's current status.

use std::sync::Arc;

use crate::daemon::db::types::ApprovalStatus;
use crate::daemon::downstream::approval::ApprovalCenter;
use crate::daemon::events::{Bus, Event};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationAction {
    /// Body-click — open the GUI and focus the approval row.
    Focus(String),
    Approve(String),
    Deny(String),
}

/// Dispatch one action. Idempotent: stale ids (already resolved) are no-ops
/// because `ApprovalCenter::resolve` checks the row's current status before
/// touching state.
pub async fn dispatch_action(
    action: NotificationAction,
    approvals: &Arc<ApprovalCenter>,
    bus: &Bus,
) {
    match action {
        NotificationAction::Focus(id) => {
            bus.publish(Event::FocusApproval { id });
        }
        NotificationAction::Approve(id) => {
            if let Err(e) = approvals.resolve(&id, ApprovalStatus::Approved).await {
                tracing::warn!(error = %e, id = %id, "notification approve dispatch failed");
            }
        }
        NotificationAction::Deny(id) => {
            if let Err(e) = approvals.resolve(&id, ApprovalStatus::Denied).await {
                tracing::warn!(error = %e, id = %id, "notification deny dispatch failed");
            }
        }
    }
}
