//! User settings (`config.toml`): schema/loader, writer actor, and watcher.
//!
//! `toml` defines `Settings` (deserialized with `serde(default)`) and loads it;
//! `writer` is the single write path (validate → `toml_edit` → atomic replace);
//! `watcher` debounces external edits and republishes `Event::ConfigReloaded`.
//! Live state flows through `SharedSettings`: writer and watcher `.store()`,
//! live consumers `.load()` at use-time. `config.toml` is authoritative, not the DB.

use std::sync::Arc;

pub mod toml;
pub mod watcher;
pub mod writer;

/// Lock-free hot-swappable settings handle shared across subsystems.
pub type SharedSettings = Arc<arc_swap::ArcSwap<toml::Settings>>;
