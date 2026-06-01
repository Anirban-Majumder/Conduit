//! Downstream MCP — the IDE-facing side where Conduit is the MCP *server*.
//!
//! `http_server` is the axum endpoint IDEs connect to; `proxy` routes each
//! `tools/call` to the upstream manager through the policy + approval gate and
//! audits the outcome; `approval` holds a request open for human-in-the-loop
//! decisions. (The outbound side — where Conduit is the MCP *client* — is
//! `daemon::upstream`.)

pub mod approval;
pub mod http_server;
pub mod proxy;
