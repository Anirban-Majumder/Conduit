#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test ergonomics")]

// Tests use keyring_core's mock in-memory store so they never touch the
// developer's real OS keyring.
//
// The keyring default store is a process-wide global, so tests must not run
// concurrently. A static Mutex serializes them; each test gets a fresh mock
// store with an empty credential set.

use std::sync::Mutex;

use keyring_core::{mock, set_default_store, unset_default_store};
use conduit::daemon::secrets;

static STORE_LOCK: Mutex<()> = Mutex::new(());

fn with_mock_store<F: FnOnce()>(f: F) {
    let _guard = STORE_LOCK.lock().expect("store lock poisoned");
    set_default_store(mock::Store::new().expect("mock store"));
    f();
    unset_default_store();
}

#[test]
fn store_fetch_round_trip() {
    with_mock_store(|| {
        let uuid = secrets::store("myserver", "super-secret").expect("store failed");
        let got = secrets::fetch("myserver", &uuid).expect("fetch failed");
        assert_eq!(got, "super-secret");
    });
}

#[test]
fn delete_then_fetch_returns_not_found() {
    with_mock_store(|| {
        let uuid = secrets::store("myserver", "deleteme").expect("store failed");
        secrets::delete("myserver", &uuid).expect("delete failed");
        let err = secrets::fetch("myserver", &uuid).expect_err("expected NotFound");
        assert!(
            matches!(err, secrets::SecretsError::NotFound),
            "unexpected error: {err}"
        );
    });
}

#[test]
fn delete_nonexistent_is_ok() {
    with_mock_store(|| {
        let result = secrets::delete("myserver", "00000000-0000-0000-0000-000000000000");
        assert!(
            result.is_ok(),
            "expected Ok for missing entry, got {result:?}"
        );
    });
}

#[test]
fn store_returns_valid_uuid_v4() {
    with_mock_store(|| {
        let uuid_str = secrets::store("myserver", "value").expect("store failed");
        let parsed = uuid::Uuid::parse_str(&uuid_str).expect("not a valid UUID");
        assert_eq!(
            parsed.get_version(),
            Some(uuid::Version::Random),
            "UUID is not v4: {uuid_str}"
        );
    });
}
