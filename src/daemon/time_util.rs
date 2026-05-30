//! Timestamp helpers: a single authoritative clock source for all daemon code.
//!
//! All subsystems that need a Unix timestamp import from here.  Having one
//! implementation means the clock-skew and overflow policy is decided once:
//! log a warning and return a safe sentinel rather than silently producing a
//! nonsensical value (year 1970 or year 2262).

use std::time::{SystemTime, UNIX_EPOCH};

/// Current time as Unix seconds, saturating to `i64::MAX` on overflow.
///
/// Logs a `warn!` if the system clock is before the Unix epoch (clock skew)
/// or if the seconds count overflows `i64` (year 2262+).
pub fn now_unix_secs() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_secs()).unwrap_or_else(|_| {
            tracing::warn!("system clock overflows i64 seconds; clamping to i64::MAX");
            i64::MAX
        }),
        Err(e) => {
            tracing::warn!(error = %e, "system clock is before Unix epoch; using 0");
            0
        }
    }
}

/// Current time as Unix nanoseconds, returning 0 on clock skew.
///
/// Logs a `warn!` if the system clock is before the Unix epoch.
pub fn now_unix_nanos() -> u128 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos(),
        Err(e) => {
            tracing::warn!(error = %e, "system clock is before Unix epoch; using 0");
            0
        }
    }
}
