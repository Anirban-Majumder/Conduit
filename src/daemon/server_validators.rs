//! Add Server wizard validators (name + lifecycle clamp).
//!
//! Server names become keyring account suffixes, URL path components, and JSON
//! keys in IDE configs — all places where odd characters cause subtle failures.

pub const LIFECYCLE_EPHEMERAL: i32 = 0;
pub const LIFECYCLE_PERSISTENT: i32 = -1;
pub const LIFECYCLE_IDLE_MIN: i32 = 1;
pub const LIFECYCLE_IDLE_MAX: i32 = 86_400;
pub const NAME_MAX_LEN: usize = 64;

/// True iff `name` matches `^[a-z0-9_-]{1,64}$`.
pub fn is_valid_server_name(name: &str) -> bool {
    if name.is_empty() || name.len() > NAME_MAX_LEN {
        return false;
    }
    name.bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// Clamp the wizard's "idle TTL" stepper to `[1, 86400]` seconds. The wizard
/// also exposes Ephemeral (`0`) and Persistent (`-1`) as separate radio
/// choices — those bypass this clamp.
pub fn clamp_idle_lifecycle(raw: i32) -> i32 {
    raw.clamp(LIFECYCLE_IDLE_MIN, LIFECYCLE_IDLE_MAX)
}

/// Coerce a wizard transport + idle-stepper into the canonical
/// `lifecycle_seconds` int. `mode`: "ephemeral" | "persistent" | "idle".
pub fn lifecycle_from_wizard(mode: &str, idle_raw: i32) -> i32 {
    match mode {
        "ephemeral" => LIFECYCLE_EPHEMERAL,
        "persistent" => LIFECYCLE_PERSISTENT,
        _ => clamp_idle_lifecycle(idle_raw),
    }
}
