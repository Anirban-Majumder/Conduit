#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test ergonomics"
)]

use std::time::Duration;

use mcp_router::daemon::upstream::lifecycle::detect_lifecycle;
use serde_json::json;

const PERSISTENT: i32 = -1;
const FAST_INIT: Duration = Duration::from_millis(100);
const SLOW_INIT: Duration = Duration::from_secs(3);

#[test]
fn already_persistent_stays_persistent() {
    let r = detect_lifecycle(PERSISTENT, &json!({}), FAST_INIT, "anything", false);
    assert_eq!(r, PERSISTENT);
}

#[test]
fn resources_subscribe_capability_forces_persistent() {
    let resp = json!({
        "capabilities": { "resources": { "subscribe": true } }
    });
    let r = detect_lifecycle(300, &resp, FAST_INIT, "any-pkg", false);
    assert_eq!(r, PERSISTENT);
}

#[test]
fn sampling_capability_forces_persistent() {
    let resp = json!({
        "capabilities": { "sampling": {} }
    });
    let r = detect_lifecycle(300, &resp, FAST_INIT, "any-pkg", false);
    assert_eq!(r, PERSISTENT);
}

#[test]
fn list_changed_notification_forces_persistent() {
    let r = detect_lifecycle(300, &json!({}), FAST_INIT, "any-pkg", true);
    assert_eq!(r, PERSISTENT);
}

#[test]
fn allow_list_match_upgrades_to_recommended() {
    // server-postgres → 300 in the allow-list. Caller passed 30 (smaller);
    // detection upgrades to 300.
    let r = detect_lifecycle(
        30,
        &json!({}),
        FAST_INIT,
        "@modelcontextprotocol/server-postgres",
        false,
    );
    assert_eq!(r, 300);
}

#[test]
fn allow_list_match_never_downgrades() {
    // server-fetch → 0 in the allow-list. Caller passed 600 (larger);
    // detection keeps 600.
    let r = detect_lifecycle(
        600,
        &json!({}),
        FAST_INIT,
        "@modelcontextprotocol/server-fetch",
        false,
    );
    assert_eq!(r, 600);
}

#[test]
fn browser_pkg_substring_forces_persistent() {
    let r = detect_lifecycle(
        300,
        &json!({}),
        FAST_INIT,
        "/usr/local/bin/playwright-bin",
        false,
    );
    assert_eq!(r, PERSISTENT);
}

#[test]
fn slow_init_promotes_to_1800() {
    let r = detect_lifecycle(300, &json!({}), SLOW_INIT, "unknown-pkg", false);
    assert_eq!(r, 1800);
}

#[test]
fn slow_init_doesnt_downgrade_high_initial() {
    let r = detect_lifecycle(3600, &json!({}), SLOW_INIT, "unknown-pkg", false);
    assert_eq!(r, 3600);
}

#[test]
fn fast_init_unknown_pkg_returns_initial() {
    let r = detect_lifecycle(300, &json!({}), FAST_INIT, "unknown-pkg", false);
    assert_eq!(r, 300);
}
