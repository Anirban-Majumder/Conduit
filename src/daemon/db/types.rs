use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdeKey {
    pub key: String,
    pub ide_name: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedIdeConfig {
    pub ide_name: String,
    pub ide_key: String,
    pub config_path: PathBuf,
    pub managed_entries: Vec<String>,
    pub written_at: i64,
}

/// Generates an 8-char alphanumeric key per spec §Add IDE flow.
///
/// 8 chars × 62 alphabet = ~47 bits — the IDE key is a *capability*, not a
/// cryptographic secret; the threat model (spec §Threat model) explicitly
/// excludes same-uid attackers, and the daemon binds 127.0.0.1.
pub fn fresh_ide_key() -> String {
    use rand::distr::{Alphanumeric, SampleString};
    Alphanumeric.sample_string(&mut rand::rng(), 8)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, reason = "test ergonomics")]

    use super::*;

    #[test]
    fn fresh_ide_key_is_8_alphanumeric_chars() {
        for _ in 0..50 {
            let k = fresh_ide_key();
            assert_eq!(k.len(), 8);
            assert!(k.chars().all(|c| c.is_ascii_alphanumeric()), "got {k}");
        }
    }

    #[test]
    fn fresh_ide_key_collisions_are_rare() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            assert!(seen.insert(fresh_ide_key()));
        }
    }
}
