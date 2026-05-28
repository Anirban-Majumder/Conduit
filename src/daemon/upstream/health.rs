//! Derived server-health state.
//!
//! `ServerState` is computed from inputs (last probe, peer aliveness, paused
//! flag) — never persisted directly. One derivation function keeps the
//! Servers-tab health card and the Idle/Live distinction consistent.

use std::time::Duration;

const RECENT_CALL_WINDOW_SECS: i64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerState {
    Unprobed,
    Probing,
    Live,
    Idle,
    Down,
    NeedsAuth,
    Paused,
}

#[derive(Debug, Clone)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent input to the state-machine derivation"
)]
pub struct ServerHealth {
    pub last_probed_at: Option<i64>,
    pub last_probe_error: Option<String>,
    pub last_call_at: Option<i64>,
    pub last_call_error: Option<String>,
    pub last_auth_error_msg: Option<String>,
    pub peer_alive: bool,
    pub needs_auth: bool,
    pub probing: bool,
    pub routing_paused: bool,
    pub now_unix: i64,
}

/// Decide the visible state. Precedence: Paused > Probing > `NeedsAuth` >
/// Down > Live/Idle > Unprobed.
pub const fn derive_state(h: &ServerHealth) -> ServerState {
    if h.routing_paused {
        return ServerState::Paused;
    }
    if h.probing {
        return ServerState::Probing;
    }
    if h.needs_auth || h.last_auth_error_msg.is_some() {
        return ServerState::NeedsAuth;
    }
    if h.last_call_error.is_some() || h.last_probe_error.is_some() {
        return ServerState::Down;
    }
    let probed = h.last_probed_at.is_some();
    if !probed {
        return ServerState::Unprobed;
    }
    if h.peer_alive {
        return ServerState::Live;
    }
    let recent = matches!(h.last_call_at, Some(ts) if h.now_unix - ts <= RECENT_CALL_WINDOW_SECS);
    if recent {
        ServerState::Live
    } else {
        ServerState::Idle
    }
}

/// Threshold for emitting `Event::ServerHealthChanged` from `UpstreamManager`
/// (debounced upstream). Exported for any consumer that wants to mirror the
/// pacing.
pub const HEALTH_EVENT_DEBOUNCE: Duration = Duration::from_millis(250);
