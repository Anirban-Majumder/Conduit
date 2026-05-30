//! Daemon: the headless core — HTTP MCP server, request routing, policy and
//! approval enforcement, audit, upstream lifecycle, IDE config sync, and IPC.
//!
//! Submodules cover the request path (`http_server`, `mcp_proxy`, `approval`),
//! state (`db`, `secrets`, `config_toml`), IDE integration (`ide_detect`,
//! `clients`, `cascade`), and lifecycle plumbing (`events`, `config_watcher`,
//! `ipc`, `onboarding`). Shared error enums live in `errors`.

pub mod approval;
pub mod cascade;
pub mod clients;
pub mod config_toml;
pub mod config_watcher;
pub mod db;
pub mod errors;
pub mod events;
pub mod http_server;
pub mod ide_detect;
pub mod ipc;
pub mod mcp_proxy;
pub mod onboarding;
pub mod paste_parser;
pub mod secrets;
pub mod server_validators;
pub mod skills;
pub mod time_util;
pub mod upstream;
