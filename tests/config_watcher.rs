#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_truncation,
    reason = "test ergonomics"
)]

use std::time::Duration;

use conduit::daemon::config_watcher;
use conduit::daemon::events::{Bus, Event};
use tokio_util::sync::CancellationToken;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watcher_publishes_config_reloaded_after_debounce() {
    let tmp_dir = std::env::temp_dir().join(format!(
        "conduit-watcher-{}-{}",
        std::process::id(),
        rand_suffix(),
    ));
    std::fs::create_dir_all(&tmp_dir).expect("mkdir");
    let path = tmp_dir.join("config.toml");
    std::fs::write(&path, "[daemon]\nport = 9876\n").expect("write initial");

    let bus = Bus::new();
    let mut rx = bus.subscribe();
    let cancel = CancellationToken::new();

    let _watcher =
        config_watcher::spawn(&path, bus.clone(), cancel.clone()).expect("spawn watcher");

    // Give the inotify (or platform equivalent) watch a moment to register.
    tokio::time::sleep(Duration::from_millis(250)).await;

    std::fs::write(&path, "[daemon]\nport = 12345\n").expect("rewrite");

    // Debounce is 5s; allow 4s extra for delivery noise and macOS FSEvents lag.
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match rx.recv().await {
                Ok(Event::ConfigReloaded { settings }) => return settings,
                Ok(_) => {}
                Err(e) => panic!("bus closed: {e}"),
            }
        }
    })
    .await
    .expect("ConfigReloaded delivered within 10s of change");

    assert_eq!(result.daemon.port, 12345);

    cancel.cancel();
    std::fs::remove_dir_all(&tmp_dir).ok();
}

fn rand_suffix() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}
