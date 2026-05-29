//! Headless-startup gating: decides whether to show the window at launch, tracks
//! first-run completion, and fires the one-time "running in background" hint.
//!
//! All state lives in `settings_kv` (`has_completed_first_run`,
//! `open_window_at_startup`, `shown_background_hint`). Before onboarding completes
//! the window always shows; afterwards it honors the saved preference.
//! `check_background_hint` flips its flag before returning `Fire`, so it is safe
//! to call repeatedly.

use crate::daemon::db::Db;
use crate::daemon::errors::DbError;

const KEY_AUTO_SHOW: &str = "open_window_at_startup";
const KEY_FIRST_RUN: &str = "has_completed_first_run";
const KEY_BG_HINT: &str = "shown_background_hint";

/// True if the daemon should call `window.show()` at startup.
///
/// If first-run has not completed, ALWAYS show (so the user has a UI).
/// Otherwise honor `settings_kv.open_window_at_startup`.
pub async fn should_show_window_at_startup(db: &Db) -> Result<bool, DbError> {
    let onboarded = db.get_settings_kv(KEY_FIRST_RUN).await?.as_deref() == Some("true");
    if !onboarded {
        return Ok(true);
    }
    let auto = db.get_settings_kv(KEY_AUTO_SHOW).await?.as_deref() == Some("true");
    Ok(auto)
}

/// Flip `has_completed_first_run` to "true" once the user has done at least
/// one onboarding task (add a server or add an IDE). Idempotent.
pub async fn mark_first_run_completed(db: &Db) -> Result<(), DbError> {
    db.set_settings_kv(KEY_FIRST_RUN, "true").await
}

/// What to do when the user closes the window: either skip (already shown or
/// not yet onboarded) or fire the one-time "running in the background" hint.
#[derive(Debug, PartialEq, Eq)]
pub enum BackgroundHintAction {
    Skip,
    Fire,
}

/// Decide whether to fire the one-time background-running hint.
///
/// Inspect `has_completed_first_run` + `shown_background_hint`, decide whether
/// to fire the hint, and (if so) flip `shown_background_hint = 'true'` before
/// returning Fire — so the caller can pop a notification without worrying
/// about repeats.
pub async fn check_background_hint(db: &Db) -> Result<BackgroundHintAction, DbError> {
    let onboarded = db.get_settings_kv(KEY_FIRST_RUN).await?.as_deref() == Some("true");
    if !onboarded {
        return Ok(BackgroundHintAction::Skip);
    }
    let already_shown = db.get_settings_kv(KEY_BG_HINT).await?.as_deref() == Some("true");
    if already_shown {
        return Ok(BackgroundHintAction::Skip);
    }
    db.set_settings_kv(KEY_BG_HINT, "true").await?;
    Ok(BackgroundHintAction::Fire)
}
