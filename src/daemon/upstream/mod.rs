//! MCP upstream subsystem: spawn, lifecycle, install, probe, and auth.
//!
//! `manager` coordinates live upstream clients; `stdio` and `http` are the two
//! transports; `installer` fetches Node/Python packages; `probe` connects a
//! draft server to enumerate tools (including the OAuth login flow); `oauth` /
//! `oauth_store` handle the OAuth callback and token storage; `lifecycle` and
//! `allow_list` decide idle TTLs; `health` derives the visible server state.

pub mod allow_list;
pub mod health;
pub mod http;
pub mod installer;
pub mod lifecycle;
pub mod manager;
pub mod oauth;
pub mod oauth_store;
pub mod probe;
pub mod stdio;
