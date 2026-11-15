//! Declarative path resolution for built-in client config and skills paths.
//!
//! `PathSpec` encodes 4 strategies that cover all 6 current IDEs.
//! `resolve()` picks the real OS path; `resolve_under()` is the test seam.

use std::path::{Path, PathBuf};

/// Declarative path strategy — 4 variants cover all 6 current IDEs.
#[derive(Debug, Clone, Copy)]
pub enum PathSpec {
    /// `home_dir().join(rel)` — `ClaudeCode`, `OpenClaude`.
    Home(&'static str),
    /// `config_dir().join(rel)` on ALL platforms — `VsCodeCopilot`.
    ConfigDir(&'static str),
    /// `home_dir().join(unix)` normally, `config_dir().join(win)` on Windows — Cursor, `OpenCode`.
    PlatformSplit {
        unix: &'static str,
        windows: &'static str,
    },
    /// `$ENV/suffix`, fallback to `home_dir().join(fallback)` — Codex.
    ///
    /// NOTE (CC-15): `resolve()` re-reads the environment variable on every call,
    /// so paths are re-resolved at each detect/projection run rather than being
    /// frozen from first startup. However, the resolved path IS frozen in the DB
    /// by `project_builtins` and is only rewritten on a version bump. A service-
    /// launched daemon with no `$CODEX_HOME` in its environment will project the
    /// fallback path; if the user later sets `$CODEX_HOME` and Conduit is still
    /// running without a restart, the DB will retain the old path until the next
    /// version bump or fresh projection. To fully fix this a re-projection on every
    /// startup (ignoring `version_changed`) would be needed for `EnvOrHome` clients.
    EnvOrHome {
        env: &'static str,
        suffix: &'static str,
        fallback: &'static str,
    },
}

/// Resolve a `PathSpec` to an absolute path on the current OS.
/// Returns `None` if the required base directory (`home_dir`/`config_dir`) is unavailable.
pub fn resolve(spec: &PathSpec) -> Option<PathBuf> {
    match spec {
        PathSpec::Home(rel) => {
            let home = dirs::home_dir()?;
            Some(home.join(rel))
        }
        PathSpec::ConfigDir(rel) => {
            let cfg = dirs::config_dir()?;
            Some(cfg.join(rel))
        }
        PathSpec::PlatformSplit { unix, windows } => {
            if cfg!(target_os = "windows") {
                let cfg = dirs::config_dir()?;
                Some(cfg.join(windows))
            } else {
                let home = dirs::home_dir()?;
                Some(home.join(unix))
            }
        }
        PathSpec::EnvOrHome {
            env,
            suffix,
            fallback,
        } => {
            if let Some(custom) = std::env::var_os(env) {
                let p = PathBuf::from(custom);
                if !p.as_os_str().is_empty() {
                    return Some(p.join(suffix));
                }
            }
            let home = dirs::home_dir()?;
            Some(home.join(fallback))
        }
    }
}

/// Test seam: resolve a `PathSpec` against a fake `home` directory.
///
/// Uses `home` as the base for `Home`, `PlatformSplit` (unix path), and
/// `EnvOrHome` (fallback). `ConfigDir` resolves as `home/.config/rel`
/// (Linux-style layout for tests).
pub fn resolve_under(spec: &PathSpec, home: &Path) -> PathBuf {
    match spec {
        PathSpec::Home(rel) => home.join(rel),
        PathSpec::ConfigDir(rel) => home.join(".config").join(rel),
        PathSpec::PlatformSplit { unix, .. } => home.join(unix),
        PathSpec::EnvOrHome { fallback, .. } => home.join(fallback),
    }
}
