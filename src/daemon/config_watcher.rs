//! Filesystem watcher for `config.toml` that debounces edits and republishes
//! settings.
//!
//! `spawn` installs a `notify` watcher on the config's parent directory and runs
//! a tokio debounce loop; after `DEBOUNCE` quiet seconds it reloads and publishes
//! `Event::ConfigReloaded` on the bus. Caller must keep the returned watcher
//! alive — dropping it detaches all watches.

use std::path::{Path, PathBuf};
use std::time::Duration;

use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;

use crate::daemon::config_toml::Settings;
use crate::daemon::errors::WatcherError;
use crate::daemon::events::{Bus, Event};

const DEBOUNCE: Duration = Duration::from_secs(5);
const EVENT_CHANNEL_DEPTH: usize = 32;

/// Watch `config_path` and publish [`Event::ConfigReloaded`] on changes.
///
/// Spawns a tokio task that debounces inbound events. After `DEBOUNCE`
/// quiet seconds following any change touching `config_path`, the task
/// reloads via [`Settings::load_or_default`] and publishes on `bus`.
///
/// The returned [`RecommendedWatcher`] must be held alive — dropping it
/// detaches all watches. Firing `cancel` ends the debounce task; the
/// watcher itself stops generating events when dropped.
///
/// We watch the parent directory (not the file directly) because editors
/// commonly save by rename, which moves the inode and breaks a single-file
/// watch. Directory-level watching catches create / rename / write uniformly.
pub fn spawn(
    config_path: &Path,
    bus: Bus,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<RecommendedWatcher, WatcherError> {
    let parent = config_path.parent().unwrap_or_else(|| Path::new("."));
    let target: PathBuf = config_path.to_path_buf();

    let (tx, rx) = mpsc::channel::<notify::Event>(EVENT_CHANNEL_DEPTH);

    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res {
                if matches!(
                    event.kind,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                ) {
                    // Try-send so a slow consumer never blocks the notify
                    // thread; lost events are fine — the debouncer reloads
                    // once whether one event or twenty arrived.
                    tx.try_send(event).ok();
                }
            }
        },
        Config::default(),
    )?;

    watcher.watch(parent, RecursiveMode::NonRecursive)?;

    tokio::spawn(debounce_loop(rx, target, bus, cancel));

    Ok(watcher)
}

async fn debounce_loop(
    mut rx: mpsc::Receiver<notify::Event>,
    target: PathBuf,
    bus: Bus,
    cancel: tokio_util::sync::CancellationToken,
) {
    let mut pending_since: Option<tokio::time::Instant> = None;

    loop {
        let next_fire = pending_since.map(|t| t + DEBOUNCE);

        tokio::select! {
            biased;

            () = cancel.cancelled() => break,

            event = rx.recv() => {
                match event {
                    Some(ev) if event_touches(&ev, &target) => {
                        pending_since = Some(tokio::time::Instant::now());
                    }
                    Some(_) => {}
                    None => break,
                }
            }

            () = sleep_until_opt(next_fire), if next_fire.is_some() => {
                pending_since = None;
                let settings = Settings::load_or_default(&target);
                tracing::info!(
                    path = %target.display(),
                    "config.toml reloaded; publishing ConfigReloaded"
                );
                bus.publish(Event::ConfigReloaded { settings });
            }
        }
    }
}

fn event_touches(event: &notify::Event, target: &Path) -> bool {
    event.paths.iter().any(|p| p == target)
}

async fn sleep_until_opt(at: Option<tokio::time::Instant>) {
    match at {
        Some(t) => tokio::time::sleep_until(t).await,
        None => std::future::pending::<()>().await,
    }
}
