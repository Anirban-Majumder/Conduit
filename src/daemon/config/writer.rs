//! Config-writer actor: the single write path for `config.toml`.
//!
//! `ConfigWriter` is an mpsc handle (mirroring the DB-actor pattern) to one
//! tokio task that owns mutation: parse + validate the edit, re-read the file
//! (picking up external hand-edits), set the one key via `toml_edit`
//! (format-preserving), atomically replace the file, then `.store()` the new
//! `Settings` into the shared `ArcSwap` so live consumers apply instantly.

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use toml_edit::{value, DocumentMut};

use crate::atomic_io::atomic_rename_or_replace;
use crate::daemon::config::toml::Settings;
use crate::daemon::config::SharedSettings;
use crate::daemon::errors::ConfigWriteError;

const LOG_LEVELS: &[&str] = &["error", "warn", "info", "debug", "trace"];
const POLICIES: &[&str] = &["ask", "allow", "deny"];
const THEMES: &[&str] = &["verge-dark"];
const UPDATE_CHANNELS: &[&str] = &["stable", "beta"];
const SYNC_MODES: &[&str] = &["symlink", "copy"];

/// One validated mutation of a single `config.toml` key.
#[derive(Debug, Clone)]
pub enum ConfigEdit {
    Port(u16),
    ApprovalTimeoutSeconds(u32),
    DefaultLifecycleSeconds(i32),
    AuditRetentionDays(u32),
    LogLevel(String),
    AutostartAtLogin(bool),
    DefaultUnknownToolPolicy(String),
    Theme(String),
    FocusHotkey(String),
    UpdateChannel(String),
    UpdateCheckIntervalHours(u32),
    ShowRssInServersTab(bool),
    OpenWindowAtStartup(bool),
    CloseToTray(bool),
    NotificationsEnabled(bool),
    NotificationsSound(bool),
    SkillsSyncMode(String),
}

fn invalid(key: &str, reason: impl Into<String>) -> ConfigWriteError {
    ConfigWriteError::Invalid {
        key: key.to_string(),
        reason: reason.into(),
    }
}

fn parse_bool(key: &str, raw: &str) -> Result<bool, ConfigWriteError> {
    match raw {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(invalid(key, "expected true or false")),
    }
}

fn parse_num<T: FromStr>(key: &str, raw: &str) -> Result<T, ConfigWriteError> {
    raw.trim()
        .parse::<T>()
        .map_err(|_parse_err| invalid(key, "not a valid number"))
}

fn parse_choice(key: &str, raw: &str, allowed: &[&str]) -> Result<String, ConfigWriteError> {
    if allowed.contains(&raw) {
        Ok(raw.to_string())
    } else {
        Err(invalid(
            key,
            format!("must be one of: {}", allowed.join(", ")),
        ))
    }
}

impl ConfigEdit {
    /// Parse + validate a `section.key` / raw-string pair from the UI.
    pub fn parse(key: &str, raw: &str) -> Result<Self, ConfigWriteError> {
        match key {
            "daemon.port" => {
                let port: u16 = parse_num(key, raw)?;
                if port == 0 {
                    return Err(invalid(key, "port must be 1-65535"));
                }
                Ok(Self::Port(port))
            }
            "daemon.approval_timeout_seconds" => {
                let secs: u32 = parse_num(key, raw)?;
                if secs == 0 {
                    return Err(invalid(key, "must be at least 1 second"));
                }
                Ok(Self::ApprovalTimeoutSeconds(secs))
            }
            "daemon.default_lifecycle_seconds" => {
                let secs: i32 = parse_num(key, raw)?;
                if secs < -1 {
                    return Err(invalid(
                        key,
                        "-1 (persistent), 0 (ephemeral), or idle seconds",
                    ));
                }
                Ok(Self::DefaultLifecycleSeconds(secs))
            }
            "daemon.audit_retention_days" => Ok(Self::AuditRetentionDays(parse_num(key, raw)?)),
            "daemon.log_level" => Ok(Self::LogLevel(parse_choice(key, raw, LOG_LEVELS)?)),
            "daemon.autostart_at_login" => Ok(Self::AutostartAtLogin(parse_bool(key, raw)?)),
            "daemon.default_unknown_tool_policy" => Ok(Self::DefaultUnknownToolPolicy(
                parse_choice(key, raw, POLICIES)?,
            )),
            "gui.theme" => Ok(Self::Theme(parse_choice(key, raw, THEMES)?)),
            "gui.focus_hotkey" => {
                global_hotkey::hotkey::HotKey::from_str(raw)
                    .map_err(|e| invalid(key, e.to_string()))?;
                Ok(Self::FocusHotkey(raw.to_string()))
            }
            "gui.update_channel" => Ok(Self::UpdateChannel(parse_choice(
                key,
                raw,
                UPDATE_CHANNELS,
            )?)),
            "gui.update_check_interval_hours" => {
                let hours: u32 = parse_num(key, raw)?;
                if hours == 0 {
                    return Err(invalid(key, "must be at least 1 hour"));
                }
                Ok(Self::UpdateCheckIntervalHours(hours))
            }
            "gui.show_rss_in_servers_tab" => Ok(Self::ShowRssInServersTab(parse_bool(key, raw)?)),
            "gui.open_window_at_startup" => Ok(Self::OpenWindowAtStartup(parse_bool(key, raw)?)),
            "gui.close_to_tray" => Ok(Self::CloseToTray(parse_bool(key, raw)?)),
            "notifications.enabled" => Ok(Self::NotificationsEnabled(parse_bool(key, raw)?)),
            "notifications.sound" => Ok(Self::NotificationsSound(parse_bool(key, raw)?)),
            "skills.sync_mode" => Ok(Self::SkillsSyncMode(parse_choice(key, raw, SYNC_MODES)?)),
            _ => Err(ConfigWriteError::UnknownKey(key.to_string())),
        }
    }

    const fn section_key(&self) -> (&'static str, &'static str) {
        match self {
            Self::Port(_) => ("daemon", "port"),
            Self::ApprovalTimeoutSeconds(_) => ("daemon", "approval_timeout_seconds"),
            Self::DefaultLifecycleSeconds(_) => ("daemon", "default_lifecycle_seconds"),
            Self::AuditRetentionDays(_) => ("daemon", "audit_retention_days"),
            Self::LogLevel(_) => ("daemon", "log_level"),
            Self::AutostartAtLogin(_) => ("daemon", "autostart_at_login"),
            Self::DefaultUnknownToolPolicy(_) => ("daemon", "default_unknown_tool_policy"),
            Self::Theme(_) => ("gui", "theme"),
            Self::FocusHotkey(_) => ("gui", "focus_hotkey"),
            Self::UpdateChannel(_) => ("gui", "update_channel"),
            Self::UpdateCheckIntervalHours(_) => ("gui", "update_check_interval_hours"),
            Self::ShowRssInServersTab(_) => ("gui", "show_rss_in_servers_tab"),
            Self::OpenWindowAtStartup(_) => ("gui", "open_window_at_startup"),
            Self::CloseToTray(_) => ("gui", "close_to_tray"),
            Self::NotificationsEnabled(_) => ("notifications", "enabled"),
            Self::NotificationsSound(_) => ("notifications", "sound"),
            Self::SkillsSyncMode(_) => ("skills", "sync_mode"),
        }
    }

    fn item(&self) -> toml_edit::Item {
        match self {
            Self::Port(n) => value(i64::from(*n)),
            Self::ApprovalTimeoutSeconds(n)
            | Self::AuditRetentionDays(n)
            | Self::UpdateCheckIntervalHours(n) => value(i64::from(*n)),
            Self::DefaultLifecycleSeconds(n) => value(i64::from(*n)),
            Self::LogLevel(s)
            | Self::DefaultUnknownToolPolicy(s)
            | Self::Theme(s)
            | Self::FocusHotkey(s)
            | Self::UpdateChannel(s)
            | Self::SkillsSyncMode(s) => value(s.as_str()),
            Self::AutostartAtLogin(b)
            | Self::ShowRssInServersTab(b)
            | Self::OpenWindowAtStartup(b)
            | Self::CloseToTray(b)
            | Self::NotificationsEnabled(b)
            | Self::NotificationsSound(b) => value(*b),
        }
    }
}

/// Apply one edit to the raw `config.toml` text, format-preserving.
/// Returns the new file text plus the `Settings` it parses to.
pub fn apply_edit(
    doc_text: &str,
    edit: &ConfigEdit,
) -> Result<(String, Settings), ConfigWriteError> {
    let mut doc: DocumentMut = doc_text.parse()?;
    let (section, key) = edit.section_key();
    let section_item = doc.entry(section).or_insert(toml_edit::table());
    let Some(table) = section_item.as_table_mut() else {
        return Err(invalid(key, format!("[{section}] is not a table")));
    };
    table.insert(key, edit.item());
    let new_text = doc.to_string();
    let settings = Settings::parse(&new_text)?;
    Ok((new_text, settings))
}

struct Cmd {
    key: String,
    value: String,
    reply: oneshot::Sender<Result<(), ConfigWriteError>>,
}

/// Async handle to the writer actor; cheap to clone.
#[derive(Debug, Clone)]
pub struct ConfigWriter {
    tx: mpsc::Sender<Cmd>,
}

impl ConfigWriter {
    /// Spawn the writer actor. Must be called within a tokio runtime context.
    pub fn spawn(config_path: PathBuf, shared: SharedSettings, cancel: CancellationToken) -> Self {
        // reason: bounded at 16 — settings edits are user-paced UI events
        let (tx, mut rx) = mpsc::channel::<Cmd>(16);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = cancel.cancelled() => break,
                    cmd = rx.recv() => {
                        let Some(cmd) = cmd else { break };
                        let result = handle_set(&config_path, &shared, &cmd.key, &cmd.value).await;
                        if let Err(e) = &result {
                            tracing::warn!(key = %cmd.key, error = %e, "config write rejected");
                        }
                        cmd.reply.send(result).ok();
                    }
                }
            }
        });
        Self { tx }
    }

    /// Validate and persist one setting; `Err` means nothing was written.
    pub async fn set(&self, key: &str, value: &str) -> Result<(), ConfigWriteError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Cmd {
                key: key.to_string(),
                value: value.to_string(),
                reply,
            })
            .await
            .map_err(|_send_err| ConfigWriteError::ActorGone)?;
        rx.await.map_err(|_recv_err| ConfigWriteError::ActorGone)?
    }
}

async fn handle_set(
    path: &Path,
    shared: &SharedSettings,
    key: &str,
    raw: &str,
) -> Result<(), ConfigWriteError> {
    let edit = ConfigEdit::parse(key, raw)?;
    // Re-read per mutation so an external hand-edit made since the last write
    // is incorporated rather than clobbered.
    let doc_text = match tokio::fs::read_to_string(path).await {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let (new_text, new_settings) = apply_edit(&doc_text, &edit)?;

    let tmp = path.with_extension("toml.tmp");
    tokio::fs::write(&tmp, new_text).await?;
    let (tmp_owned, target) = (tmp.clone(), path.to_path_buf());
    tokio::task::spawn_blocking(move || atomic_rename_or_replace(&tmp_owned, &target))
        .await
        .map_err(|e| ConfigWriteError::Io(std::io::Error::other(e)))??;

    shared.store(Arc::new(new_settings));
    Ok(())
}
