use tokio::sync::broadcast;

use crate::daemon::config_toml::Settings;
use crate::daemon::db::types::ApprovalStatus;

const BUS_CAPACITY: usize = 256;

#[derive(Debug, Clone)]
pub enum Event {
    ApprovalPending {
        id: String,
        server: String,
        tool: String,
        ide_key: String,
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
