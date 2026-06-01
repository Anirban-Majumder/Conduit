//! User settings (`config.toml`): schema/loader plus the filesystem watcher.
//!
//! `toml` defines `Settings` (deserialized with `serde(default)`) and loads it;
//! `watcher` debounces edits and republishes `Event::ConfigReloaded`. This is
//! authoritative for user-tunable settings, not the DB.

pub mod toml;
pub mod watcher;
