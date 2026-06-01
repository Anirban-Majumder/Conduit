#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test ergonomics")]

use std::path::PathBuf;

use conduit::daemon::paths::Paths;

#[test]
fn under_composes_paths_from_root() {
    let paths = Paths::under("/var/test");
    assert_eq!(paths.root(), PathBuf::from("/var/test"));
    assert_eq!(paths.state_db(), PathBuf::from("/var/test/state.db"));
    assert_eq!(paths.config_toml(), PathBuf::from("/var/test/config.toml"));
    assert_eq!(paths.installs_dir(), PathBuf::from("/var/test/installs"));
    assert_eq!(paths.logs_dir(), PathBuf::from("/var/test/logs"));
    assert_eq!(
        paths.daemon_log(),
        PathBuf::from("/var/test/logs/daemon.log")
    );
    assert_eq!(
        paths.server_log("gmail"),
        PathBuf::from("/var/test/logs/server-gmail.log")
    );
}

#[test]
fn ensure_created_makes_root_installs_and_logs() {
    let tmp = std::env::temp_dir().join(format!("conduit-paths-test-{}", std::process::id()));
    std::fs::remove_dir_all(&tmp).ok();
    let paths = Paths::under(&tmp);

    paths.ensure_created().expect("create dirs");

    assert!(paths.root().is_dir());
    assert!(paths.installs_dir().is_dir());
    assert!(paths.logs_dir().is_dir());

    std::fs::remove_dir_all(&tmp).expect("cleanup");
}

#[test]
fn from_user_dirs_lives_under_config_dir() {
    let paths = Paths::from_user_dirs().expect("user dirs resolvable");
    assert!(paths.root().ends_with("conduit"));
}
