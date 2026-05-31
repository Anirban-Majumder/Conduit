//! Conduit library crate: the daemon, GUI OS integration, installer, and shared
//! types, consumed by the `conduit` binary.
//!
//! `daemon` holds the HTTP server, MCP routing, policy, audit, DB actor, and
//! upstream lifecycle; `gui` the tray/notifications/hotkey; `installer` the
//! first-run service setup. `paths` and `runtime` are the cross-cutting basics.

#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, reason = "test ergonomics")
)]

pub mod atomic_io;
pub mod daemon;
pub mod gui;
pub mod installer;
pub mod paths;
pub mod runtime;
