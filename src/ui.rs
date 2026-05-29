//! Re-exports the Slint-generated UI types and the tab-index constants.
//!
//! `include_modules!()` pulls in the tree emitted by `slint-build` from
//! `src/ui/main.slint`. The crate's lint rules don't apply to generated code, so
//! the blanket `#![allow(...)]` below silences it.

#![allow(
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::cargo,
    clippy::restriction,
    missing_debug_implementations,
    unused_qualifications,
    unreachable_pub,
    unsafe_code,
    rust_2018_idioms,
    unused_lifetimes,
    reason = "slint-build generated"
)]

slint::include_modules!();

#[allow(
    dead_code,
    reason = "constants are part of the module's public surface"
)]
pub mod tabs {
    pub const APPROVALS: i32 = 0;
    pub const SERVERS: i32 = 1;
    pub const EDITORS: i32 = 2;
    pub const SKILLS: i32 = 3;
    pub const LOG: i32 = 4;
    pub const SETTINGS: i32 = 5;
    pub const HELP: i32 = 6;
}
