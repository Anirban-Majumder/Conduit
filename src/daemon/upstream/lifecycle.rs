// Auto-detect lifecycle_seconds rules per spec §"Server lifecycle" (rule precedence).
//
// Auto-detection only UPGRADES (longer-lived), never downgrades.

use std::time::Duration;

use serde_json::Value;

use crate::daemon::upstream::allow_list;

const PERSISTENT: i32 = -1;
const SLOW_INIT_THRESHOLD: Duration = Duration::from_secs(2);
const SLOW_INIT_PROMOTED_SECONDS: i32 = 1800;

/// Decide a possibly-upgraded `lifecycle_seconds` from observed init signals.
///
/// Rules (first match wins, but never downgrade):
/// 1. `resources.subscribe` or `sampling` capability advertised → -1
/// 2. `saw_notifications_changed_in_first_60s` → -1
/// 3. allow-list exact match → upgrade to that value (only if >= initial)
/// 4. `playwright`/`puppeteer`/`browser-use` substring → -1
/// 5. `initialize_duration` >= 2s → upgrade to at least 1800
/// 6. otherwise → initial
pub fn detect_lifecycle(
    initial: i32,
    initialize_response: &Value,
    initialize_duration: Duration,
    binary_path_or_pkg: &str,
    saw_notifications_changed_in_first_60s: bool,
) -> i32 {
    if initial == PERSISTENT {
        return PERSISTENT;
    }

    if response_advertises_persistent_capability(initialize_response) {
        return PERSISTENT;
    }

    if saw_notifications_changed_in_first_60s {
        return PERSISTENT;
    }

    if let Some(recommended) = allow_list::lookup(binary_path_or_pkg) {
        return upgrade_to(initial, recommended);
    }

    if allow_list::is_browser_pkg(binary_path_or_pkg) {
        return PERSISTENT;
    }

    if initialize_duration >= SLOW_INIT_THRESHOLD {
        return upgrade_to(initial, SLOW_INIT_PROMOTED_SECONDS);
    }

    initial
}

/// Returns the larger of `current` and `candidate`, treating `-1` (persistent)
/// as "infinite". Auto-detect never downgrades.
fn upgrade_to(current: i32, candidate: i32) -> i32 {
    if current == PERSISTENT || candidate == PERSISTENT {
        return PERSISTENT;
    }
    current.max(candidate)
}

fn response_advertises_persistent_capability(initialize_response: &Value) -> bool {
    let Some(caps) = initialize_response.get("capabilities") else {
        return false;
    };
    let subscribes = caps
        .get("resources")
        .and_then(|r| r.get("subscribe"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let samples = caps.get("sampling").is_some();
    subscribes || samples
}
