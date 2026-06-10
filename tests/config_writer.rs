//! Config-writer core: format-preserving edits + per-field validation.
//!
//! Exercises `ConfigEdit::parse` (the validation chokepoint) and `apply_edit`
//! (the `toml_edit` read-modify-write) without the actor/file plumbing.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test ergonomics")]

use conduit::daemon::config::writer::{apply_edit, ConfigEdit};

#[test]
fn edit_round_trips_into_settings() {
    let edit = ConfigEdit::parse("daemon.approval_timeout_seconds", "120").unwrap();
    let (text, settings) = apply_edit("", &edit).unwrap();
    assert_eq!(settings.daemon.approval_timeout_seconds, 120);
    // The new text re-parses to the same value (file ↔ settings agree).
    assert!(text.contains("approval_timeout_seconds = 120"));
}

#[test]
fn edit_preserves_comments_and_unrelated_keys() {
    let toml = "# hand-written header\n[daemon]\nport = 1234 # my port\nlog_level = \"debug\"\n";
    let edit = ConfigEdit::parse("daemon.audit_retention_days", "30").unwrap();
    let (text, settings) = apply_edit(toml, &edit).unwrap();
    assert!(text.contains("# hand-written header"));
    assert!(text.contains("port = 1234 # my port"));
    assert_eq!(settings.daemon.port, 1234);
    assert_eq!(settings.daemon.log_level, "debug");
    assert_eq!(settings.daemon.audit_retention_days, 30);
}

#[test]
fn edit_creates_missing_section() {
    let edit = ConfigEdit::parse("gui.close_to_tray", "false").unwrap();
    let (_, settings) = apply_edit("[daemon]\nport = 9876\n", &edit).unwrap();
    assert!(!settings.gui.close_to_tray);
}

#[test]
fn second_edit_keeps_external_hand_edit() {
    // Simulates the actor's re-read-per-mutation: a hand-edit between two UI
    // writes survives.
    let edit1 = ConfigEdit::parse("notifications.enabled", "false").unwrap();
    let (text, _) = apply_edit("", &edit1).unwrap();
    let hand_edited = format!("{text}\n[daemon]\nport = 4321\n");
    let edit2 = ConfigEdit::parse("notifications.sound", "true").unwrap();
    let (_, settings) = apply_edit(&hand_edited, &edit2).unwrap();
    assert_eq!(settings.daemon.port, 4321);
    assert!(!settings.notifications.enabled);
    assert!(settings.notifications.sound);
}

#[test]
fn invalid_values_are_rejected() {
    for (key, value) in [
        ("daemon.port", "0"),
        ("daemon.port", "70000"),
        ("daemon.port", "abc"),
        ("daemon.approval_timeout_seconds", "0"),
        ("daemon.default_lifecycle_seconds", "-2"),
        ("daemon.log_level", "verbose"),
        ("daemon.default_unknown_tool_policy", "maybe"),
        ("gui.theme", "light"),
        ("gui.focus_hotkey", "NotAKey+Q+"),
        ("gui.update_channel", "nightly"),
        ("gui.update_check_interval_hours", "0"),
        ("gui.close_to_tray", "yes"),
        ("skills.sync_mode", "hardlink"),
        ("daemon.no_such_key", "1"),
    ] {
        assert!(
            ConfigEdit::parse(key, value).is_err(),
            "{key}={value} should be rejected"
        );
    }
}

#[test]
fn lifecycle_tri_state_allows_sentinels() {
    for v in ["-1", "0", "300"] {
        assert!(
            ConfigEdit::parse("daemon.default_lifecycle_seconds", v).is_ok(),
            "lifecycle {v} should be accepted"
        );
    }
}

#[test]
fn default_hotkey_string_parses() {
    assert!(ConfigEdit::parse("gui.focus_hotkey", "Ctrl+Alt+Shift+M").is_ok());
}
