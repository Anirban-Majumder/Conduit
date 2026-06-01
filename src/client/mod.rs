//! The built-in client catalog: one `static DEF: ClientDef` per supported
//! IDE/agent, plus the copy-template.
//!
//! Pure static data — no logic. The `daemon::client_config` engine consumes
//! these via its `BUILTINS` slice (detection, config rendering, writing). To add
//! a client: copy `example.rs` to `<name>.rs`, fill in its `ClientDef`, and add
//! it both here and to `client_config::BUILTINS`.

pub mod claude_code;
pub mod codex;
pub mod cursor;
pub mod openclaude;
pub mod opencode;
pub mod vscode_copilot;

// Template for adding a new client. Gated with `#![cfg(any())]` so it never
// compiles — copy this file to `<name>.rs` and wire it into client_config::BUILTINS.
pub mod example;
