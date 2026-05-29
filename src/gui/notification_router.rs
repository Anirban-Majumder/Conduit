//! Dispatch notification-toast button clicks back into the daemon.
//!
//! `notifications.rs` forwards `NotificationAction`s here; this module resolves
//! the approval via `ApprovalCenter` and (for "Approve always") upgrades the
//! policy. Dispatch is idempotent — stale ids are no-ops because `resolve`
//! checks the row's current status.

use std::sync::Arc;

use crate::daemon::approval::ApprovalCenter;
use crate::daemon::db::types::{ApprovalStatus, Decision, DecisionSource};
use crate::daemon::db::Db;
use crate::daemon::events::{Bus, Event};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationAction {
    /// Body-click — open the GUI and focus the approval row.
    Focus(String),
    Approve(String),
    Deny(String),
    /// Approve THIS call AND upgrade the (server, tool) policy to `allow` so
    /// future invocations skip the toast.
    ApproveAlways {
        id: String,
        server: String,
        tool: String,
    },
}

/// Dispatch one action. Idempotent: stale ids (already resolved) are no-ops
/// because `ApprovalCenter::resolve` checks the row's current status before
/// touching state.
pub async fn dispatch_action(
    action: NotificationAction,
    db: &Db,
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
        NotificationAction::ApproveAlways { id, server, tool } => {
            if let Err(e) = approvals.resolve(&id, ApprovalStatus::Approved).await {
                tracing::warn!(error = %e, id = %id, "notification approve_always resolve failed");
                return;
            }
            if let Err(e) = db
                .set_policy(
                    &server,
                    &tool,
                    Decision::Allow,
                    DecisionSource::Notification,
                )
                .await
            {
                tracing::warn!(
                    error = %e,
                    server = %server,
                    tool = %tool,
                    "notification approve_always policy upgrade failed",
                );
            }
        }
    }
}
