#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test ergonomics"
)]

use mcp_router::daemon::events::{Bus, Event};

fn sample_pending(id: &str) -> Event {
    Event::ApprovalPending {
        id: id.to_string(),
        server: "gmail".to_string(),
        tool: "send_email".to_string(),
        ide_key: "abc12345".to_string(),
    }
}

#[tokio::test]
async fn bus_delivers_event_to_subscriber() {
    let bus = Bus::new();
    let mut rx = bus.subscribe();
    bus.publish(sample_pending("approval-42"));
    let evt = rx.recv().await.expect("recv event");
    match evt {
        Event::ApprovalPending { id, .. } => assert_eq!(id, "approval-42"),
        other => panic!("unexpected event: {other:?}"),
    }
}

#[tokio::test]
async fn bus_publish_with_no_subscribers_is_silent() {
    let bus = Bus::new();
    bus.publish(sample_pending("approval-1"));
}
