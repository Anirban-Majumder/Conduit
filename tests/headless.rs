#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test ergonomics")]

use conduit::daemon::db::Db;
use conduit::daemon::onboarding::{
    check_background_hint, mark_first_run_completed, should_show_window_at_startup,
    BackgroundHintAction,
};

#[tokio::test]
async fn first_run_forces_show_even_when_open_window_false() {
    let db = Db::spawn_in_memory().unwrap();
    // Defaults already seeded: open_window_at_startup=false, has_completed_first_run=false.
    let show = should_show_window_at_startup(&db).await.unwrap();
    assert!(show, "first-run should force the window open");
}

#[tokio::test]
async fn after_first_run_window_honors_open_window_at_startup_false() {
    let db = Db::spawn_in_memory().unwrap();
    mark_first_run_completed(&db).await.unwrap();
    let show = should_show_window_at_startup(&db).await.unwrap();
    assert!(
        !show,
        "after onboarding with open_window_at_startup=false, window should stay hidden"
    );
}

#[tokio::test]
async fn after_first_run_window_honors_open_window_at_startup_true() {
    let db = Db::spawn_in_memory().unwrap();
    mark_first_run_completed(&db).await.unwrap();
    db.set_settings_kv("open_window_at_startup", "true")
        .await
        .unwrap();
    let show = should_show_window_at_startup(&db).await.unwrap();
    assert!(show, "open_window_at_startup=true should show window");
}

#[tokio::test]
async fn mark_first_run_completed_is_idempotent() {
    let db = Db::spawn_in_memory().unwrap();
    mark_first_run_completed(&db).await.unwrap();
    mark_first_run_completed(&db).await.unwrap();
    assert_eq!(
        db.get_settings_kv("has_completed_first_run")
            .await
            .unwrap()
            .as_deref(),
        Some("true")
    );
}

#[tokio::test]
async fn background_hint_skipped_when_not_onboarded() {
    let db = Db::spawn_in_memory().unwrap();
    let action = check_background_hint(&db).await.unwrap();
    assert_eq!(action, BackgroundHintAction::Skip);
    // Hint state unchanged.
    assert_eq!(
        db.get_settings_kv("shown_background_hint")
            .await
            .unwrap()
            .as_deref(),
        Some("false")
    );
}

#[tokio::test]
async fn background_hint_fires_once_after_onboarding() {
    let db = Db::spawn_in_memory().unwrap();
    mark_first_run_completed(&db).await.unwrap();

    let action = check_background_hint(&db).await.unwrap();
    assert_eq!(
        action,
        BackgroundHintAction::Fire,
        "first close after onboarding should fire hint"
    );
    assert_eq!(
        db.get_settings_kv("shown_background_hint")
            .await
            .unwrap()
            .as_deref(),
        Some("true"),
        "fire should also flip the flag"
    );

    // Second call must NOT fire.
    let action = check_background_hint(&db).await.unwrap();
    assert_eq!(action, BackgroundHintAction::Skip, "must not fire twice");
}
