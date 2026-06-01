//! In-process event bus: a cloneable `tokio::sync::broadcast` wrapper carrying
//! `Event`s from the daemon to GUI/notification subscribers.
//!
//! `Bus::publish` is fire-and-forget — sending with no receivers is not an error.
//! Subscribers (tray, window, OS notifications) call `subscribe`. Bounded at
//! `BUS_CAPACITY`; slow consumers lag rather than block publishers.

use tokio::sync::broadcast;

use crate::daemon::config::toml::Settings;
use crate::daemon::db::types::ApprovalStatus;

const BUS_CAPACITY: usize = 256;

#[derive(Debug, Clone)]
pub enum Event {
    ApprovalPending {
        id: String,
        mcp_server: String,
        tool: String,
        client_key: String,
    },
    ApprovalResolved {
        id: String,
        status: ApprovalStatus,
    },
    ConfigReloaded {
        settings: Settings,
    },
    /// Server's derived health state changed. Consumers re-query for fresh values.
    ServerHealthChanged {
        name: String,
    },
    /// User clicked an approval-pending toast — open the GUI on that row.
    FocusApproval {
        id: String,
    },
    /// Installed skills / discovery state changed — UI should re-pull.
    SkillsChanged,
    /// Transition: a tool call on this server failed with an auth-flavoured error
    /// and the prior state was healthy. Fires the OS notification once.
    AuthFailure {
        server: String,
        message: String,
    },
    /// Transition: a tool call on this server succeeded after a prior auth failure.
    AuthCleared {
        server: String,
    },
    /// A `tools/call` was handled and an audit row inserted. Log tab should refresh.
    AuditAppended,
}

#[derive(Debug, Clone)]
pub struct Bus {
    tx: broadcast::Sender<Event>,
}

impl Bus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(BUS_CAPACITY);
        Self { tx }
    }

    pub fn publish(&self, event: Event) {
        // broadcast::send errors only when there are no receivers; the bus is
        // fire-and-forget, so drop that result deliberately.
        self.tx.send(event).ok();
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}
