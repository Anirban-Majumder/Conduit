#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test ergonomics")]

use conduit::daemon::config::toml::Settings;

#[test]
fn empty_toml_parses_to_spec_defaults() {
    let s = Settings::parse("").expect("parse empty toml");

    assert_eq!(s.daemon.port, 9876);
    assert_eq!(s.daemon.approval_timeout_seconds, 300);
    assert_eq!(s.daemon.default_lifecycle_seconds, 300);
    assert_eq!(s.daemon.audit_retention_days, 7);
    assert_eq!(s.daemon.log_level, "info");
    assert!(s.daemon.autostart_at_login);

    assert_eq!(s.gui.theme, "verge-dark");
    assert_eq!(s.gui.focus_hotkey, "Ctrl+Alt+Shift+M");
    assert_eq!(s.gui.update_channel, "stable");
    assert_eq!(s.gui.update_check_interval_hours, 24);
    assert!(s.gui.show_rss_in_servers_tab);

    assert!(s.notifications.enabled);
    assert!(!s.notifications.sound);
}

#[test]
fn full_spec_toml_round_trips() {
    let toml = r#"[daemon]
port = 9876
approval_timeout_seconds = 300
default_lifecycle_seconds = 300
audit_retention_days = 30
log_level = "info"
autostart_at_login = true

[gui]
theme = "verge-dark"
focus_hotkey = "Ctrl+Alt+Shift+M"
update_channel = "stable"
update_check_interval_hours = 24
show_rss_in_servers_tab = true

[notifications]
enabled = true
sound = false"#;
    let s = Settings::parse(toml).expect("parse full spec toml");
    assert_eq!(s.daemon.port, 9876);
    assert_eq!(s.gui.theme, "verge-dark");
    assert!(!s.notifications.sound);
}

#[test]
fn partial_toml_keeps_defaults_for_omitted_fields() {
    let toml = r"
[daemon]
port = 19876

[notifications]
sound = true
";
    let s = Settings::parse(toml).expect("parse partial toml");
    assert_eq!(s.daemon.port, 19876);
    assert_eq!(s.daemon.approval_timeout_seconds, 300);
    assert_eq!(s.gui.theme, "verge-dark");
    assert!(s.notifications.sound);
}

#[test]
fn invalid_toml_returns_parse_error() {
    let result = Settings::parse("not [valid] toml :::");
    assert!(result.is_err());
}

#[test]
fn load_or_default_missing_file_returns_defaults() {
    let path =
        std::env::temp_dir().join(format!("conduit-cfg-missing-{}.toml", std::process::id()));
    std::fs::remove_file(&path).ok();
    let s = Settings::load_or_default(&path);
    assert_eq!(s.daemon.port, 9876);
}

#[test]
fn load_or_default_present_file_parses() {
    let path =
        std::env::temp_dir().join(format!("conduit-cfg-present-{}.toml", std::process::id()));
    std::fs::write(&path, "[daemon]\nport = 12345\n").expect("write fixture");
    let s = Settings::load_or_default(&path);
    assert_eq!(s.daemon.port, 12345);
    std::fs::remove_file(&path).ok();
}

#[test]
fn load_or_default_garbage_file_returns_defaults() {
    let path =
        std::env::temp_dir().join(format!("conduit-cfg-garbage-{}.toml", std::process::id()));
    std::fs::write(&path, "not [valid] :::").expect("write fixture");
    let s = Settings::load_or_default(&path);
    assert_eq!(s.daemon.port, 9876);
    std::fs::remove_file(&path).ok();
}
