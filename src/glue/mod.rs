//! Binary-crate Slint↔Rust glue, split out of `main.rs`.
//!
//! `callbacks*` wire `window.on_*` handlers, `refresh` repaints UI models from
//! the DB, `wizard` drives the add-server flow, `skills` the Skills tab, `util`
//! holds shared helpers. The load-bearing rule throughout: callbacks run on the
//! Slint thread, real work is `rt.spawn`ed onto tokio, and UI mutation only ever
//! happens via `upgrade_in_event_loop`.

pub(crate) mod callbacks;
pub(crate) mod callbacks_ide;
pub(crate) mod callbacks_misc;
pub(crate) mod callbacks_servers;
pub(crate) mod refresh;
pub(crate) mod skills;
pub(crate) mod util;
pub(crate) mod wizard;
