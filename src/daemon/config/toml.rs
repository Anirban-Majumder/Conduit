//! `config.toml` schema and loader for user-tunable settings.
//!
//! `Settings` (daemon/gui/notifications sections) deserializes with `serde(default)`
//! so a partial or absent file always yields a valid value. `config.toml` is
//! authoritative for these settings, not the DB. The config watcher reloads via
//! `load_or_default` and publishes `Event::ConfigReloaded`.

use std::path::Path;

use serde::Deserialize;

use crate::daemon::errors::ConfigError;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub daemon: DaemonSettings,
    pub gui: GuiSettings,
    pub notifications: NotificationSettings,
    pub skills: SkillsSettings,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SkillsSettings {
    pub sync_mode: SyncMode,
}

/// How a managed skill is mirrored into an editor's skills dir (global + repo).
/// `Symlink` tries a symlink then falls back to copy; `Copy` always copies.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SyncMode {
    #[default]
    Symlink,
    Copy,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DaemonSettings {
    pub port: u16,
    pub approval_timeout_seconds: u32,
    pub default_lifecycle_seconds: i32,
    pub audit_retention_days: u32,
    pub log_level: String,
    pub autostart_at_login: bool,
    pub default_unknown_tool_policy: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct GuiSettings {
    pub theme: String,
    pub focus_hotkey: String,
    pub update_channel: String,
    pub update_check_interval_hours: u32,
    pub show_rss_in_servers_tab: bool,
    pub open_window_at_startup: bool,
    pub close_to_tray: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct NotificationSettings {
    pub enabled: bool,
    pub sound: bool,
}

impl Default for DaemonSettings {
    fn default() -> Self {
        Self {
            port: 9876,
            approval_timeout_seconds: 300,
            default_lifecycle_seconds: 300,
            audit_retention_days: 7,
            log_level: "info".to_string(),
            autostart_at_login: true,
            default_unknown_tool_policy: "ask".to_string(),
        }
    }
}

impl Default for GuiSettings {
    fn default() -> Self {
        Self {
            theme: "verge-dark".to_string(),
            focus_hotkey: "Ctrl+Alt+Shift+M".to_string(),
            update_channel: "stable".to_string(),
            update_check_interval_hours: 24,
            show_rss_in_servers_tab: true,
            open_window_at_startup: false,
            close_to_tray: true,
        }
    }
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            sound: false,
        }
    }
}

impl Settings {
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let settings: Self = ::toml::from_str(text)?;
        Ok(settings)
    }

    pub fn load_from_file(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|e| ConfigError::Read {
            path: path.to_path_buf(),
            source: e,
        })?;
        Self::parse(&text)
    }

    /// Load settings; on any failure (missing file, unreadable, invalid TOML),
    /// log and return defaults. Spec §Configuration: "Invalid TOML → logs
    /// error, keeps last-good settings". At startup there is no last-good,
    /// so defaults stand in.
    pub fn load_or_default(path: &Path) -> Self {
        match Self::load_from_file(path) {
            Ok(s) => s,
            Err(ConfigError::Read { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                tracing::info!(path = %path.display(), "config.toml not present; using defaults");
                Self::default()
            }
            Err(e) => {
                tracing::error!(error = %e, path = %path.display(), "config.toml load failed; using defaults");
                Self::default()
            }
        }
    }
}
