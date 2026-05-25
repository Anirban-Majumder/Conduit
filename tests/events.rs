#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test ergonomics")]

use mcp_router::daemon::events::{Bus, Event};

#[tokio::test]
async fn bus_delivers_event_to_subscriber() {
    let bus = Bus::new();
    let mut rx = bus.subscribe();
    bus.publish(Event::ApprovalPending { id: 42 });
    let evt = rx.recv().await.expect("recv event");
    assert!(matches!(evt, Event::ApprovalPending { id: 42 }));
}

#[tokio::test]
async fn bus_publish_with_no_subscribers_is_silent() {
    let bus = Bus::new();
    bus.publish(Event::ApprovalPending { id: 1 });
}
