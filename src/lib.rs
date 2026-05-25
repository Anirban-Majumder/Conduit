#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, reason = "test ergonomics")
)]

pub mod daemon;
pub mod gui;
pub mod installer;
pub mod paths;
pub mod runtime;
