use tokio::sync::broadcast;

const BUS_CAPACITY: usize = 256;

#[derive(Debug, Clone)]
pub enum Event {
    ApprovalPending { id: i64 },
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
