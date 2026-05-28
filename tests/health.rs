#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test ergonomics")]

use conduit::daemon::upstream::health::{derive_state, ServerHealth, ServerState};

const fn fresh() -> ServerHealth {
    ServerHealth {
        last_probed_at: None,
        last_probe_error: None,
        last_call_at: None,
        last_call_error: None,
        last_auth_error_msg: None,
        peer_alive: false,
        needs_auth: false,
        probing: false,
        routing_paused: false,
        now_unix: 1000,
    }
}

#[test]
fn fresh_unprobed_returns_unprobed() {
    assert_eq!(derive_state(&fresh()), ServerState::Unprobed);
}

#[test]
fn paused_overrides_everything() {
    let mut h = fresh();
    h.routing_paused = true;
    h.peer_alive = true;
    h.last_probed_at = Some(500);
    assert_eq!(derive_state(&h), ServerState::Paused);
}

#[test]
fn probing_overrides_completed_state() {
    let mut h = fresh();
    h.probing = true;
    h.peer_alive = true;
    assert_eq!(derive_state(&h), ServerState::Probing);
}

#[test]
fn needs_auth_wins_over_down() {
    let mut h = fresh();
    h.needs_auth = true;
    h.last_call_error = Some("401".into());
    assert_eq!(derive_state(&h), ServerState::NeedsAuth);
}

#[test]
fn any_error_yields_down() {
    let mut h = fresh();
    h.last_probed_at = Some(500);
    h.last_call_error = Some("503".into());
    assert_eq!(derive_state(&h), ServerState::Down);

    let mut h = fresh();
    h.last_probe_error = Some("init failed".into());
    assert_eq!(derive_state(&h), ServerState::Down);
}

#[test]
fn live_peer_after_probe_is_live() {
    let mut h = fresh();
    h.last_probed_at = Some(500);
    h.peer_alive = true;
    assert_eq!(derive_state(&h), ServerState::Live);
}

#[test]
fn probed_without_peer_within_recent_window_is_live() {
    let mut h = fresh();
    h.last_probed_at = Some(500);
    h.peer_alive = false;
    h.last_call_at = Some(970); // 30s ago, within 60s window
    assert_eq!(derive_state(&h), ServerState::Live);
}

#[test]
fn probed_without_peer_beyond_recent_window_is_idle() {
    let mut h = fresh();
    h.last_probed_at = Some(500);
    h.peer_alive = false;
    h.last_call_at = Some(800); // 200s ago, outside 60s window
    assert_eq!(derive_state(&h), ServerState::Idle);
}

#[test]
fn probed_with_no_call_history_is_idle() {
    let mut h = fresh();
    h.last_probed_at = Some(500);
    h.peer_alive = false;
    h.last_call_at = None;
    assert_eq!(derive_state(&h), ServerState::Idle);
}

#[test]
fn auth_error_msg_yields_needs_auth() {
    let mut h = fresh();
    h.last_probed_at = Some(500);
    h.peer_alive = true;
    h.last_auth_error_msg = Some("token expired".into());
    assert_eq!(derive_state(&h), ServerState::NeedsAuth);
}
