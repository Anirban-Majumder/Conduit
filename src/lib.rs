//! Conduit library crate: the daemon, GUI OS integration, installer, and shared
//! types, consumed by the `conduit` binary.
//!
//! `daemon` holds the HTTP server, MCP routing, policy, audit, DB actor, and
//! upstream lifecycle; `os` the tray/notifications/hotkey; `installer` the
//! first-run service setup; `client` the built-in IDE catalog. `runtime` is
//! the cross-cutting tokio worker.

#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, reason = "test ergonomics")
)]

pub mod atomic_io;
pub mod client;
pub mod daemon;
pub mod installer;
pub mod os;
pub mod runtime;
