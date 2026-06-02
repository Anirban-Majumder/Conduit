//! Daemon: the headless core — HTTP MCP server, request routing, policy and
//! approval enforcement, audit, upstream lifecycle, IDE config sync, and IPC.
//!
//! Submodules cover the request path (`downstream` — `http_server`, `proxy`,
//! `approval`), state (`db`, `secrets`, `config`), editor integration
//! (`client_config` — detection, projection, config rendering — and `cascade`),
//! and lifecycle plumbing (`events`, `ipc`, `onboarding`). `paths` resolves
//! on-disk locations. Shared error enums live in `errors`.

pub mod cascade;
pub mod client_config;
pub mod config;
pub mod db;
pub mod downstream;
pub mod errors;
pub mod events;
pub mod ipc;
pub mod onboarding;
pub mod paths;
pub mod retention;
pub mod secrets;
pub mod skills;
pub mod time_util;
pub mod upstream;
pub mod wizard_input;
