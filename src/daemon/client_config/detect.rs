//! First-run client auto-detection, driven entirely by `BUILTINS`.
//!
//! Scans `$PATH` and per-OS app locations for each `ClientDef`'s `binaries` /
//! `mac_app` / `win_paths`. Best-effort: never panics or errors, returns
//! whatever it found. Detection data lives on `ClientDef`, so there is no
//! separate spec list to keep in sync.

use std::path::PathBuf;

#[cfg(target_os = "macos")]
use std::path::Path;

use super::{ClientDef, BUILTINS};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetectionSource {
    /// Found as an executable in `$PATH` at the resolved path.
    Path(PathBuf),
    /// Found as a macOS application bundle.
    MacApp(PathBuf),
    /// Found at a well-known Windows install path.
    WinApp(PathBuf),
}

impl DetectionSource {
    pub fn as_db_str(&self) -> String {
        match self {
            Self::Path(p) => format!("path:{}", p.display()),
            Self::MacApp(p) => format!("macapp:{}", p.display()),
            Self::WinApp(p) => format!("winapp:{}", p.display()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DetectedClient {
    pub def: &'static ClientDef,
    pub source: DetectionSource,
}

/// Scan PATH directories for a binary matching any of the given names.
pub fn scan_paths(paths: &[PathBuf], binary_names: &[&str]) -> Option<PathBuf> {
    for dir in paths {
        // CC-19: skip empty PATH components — they would resolve against CWD.
        if dir.as_os_str().is_empty() {
            continue;
        }
        for name in binary_names {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
            #[cfg(target_os = "windows")]
            {
                // CC-19: also probe .exe, .cmd, .ps1 — npm/pnpm/volta install
                // shims like claude.cmd rather than a plain binary.
                for ext in &[".exe", ".cmd", ".ps1"] {
                    let with_ext = dir.join(format!("{name}{ext}"));
                    if with_ext.is_file() {
                        return Some(with_ext);
                    }
                }
            }
        }
    }
    None
}

#[cfg(target_os = "macos")]
pub fn scan_apps(apps_root: &Path, app_name: &str) -> Option<PathBuf> {
    let candidate = apps_root.join(app_name);
    candidate.is_dir().then_some(candidate)
}

/// Detect installed built-in clients across the current OS. Never panics.
pub fn detect_installed() -> Vec<DetectedClient> {
    let path_dirs: Vec<PathBuf> = std::env::var("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();

    let mut results = Vec::new();
    for def in BUILTINS {
        #[cfg(target_os = "macos")]
        if let Some(app_name) = def.mac_app {
            if let Some(app_path) = scan_apps(Path::new("/Applications"), app_name) {
                results.push(DetectedClient {
                    def,
                    source: DetectionSource::MacApp(app_path),
                });
                continue;
            }
        }

        #[cfg(target_os = "windows")]
        if let Some(win_path) = scan_win_paths(def.win_paths) {
            results.push(DetectedClient {
                def,
                source: DetectionSource::WinApp(win_path),
            });
            continue;
        }

        if let Some(bin_path) = scan_paths(&path_dirs, def.binaries) {
            results.push(DetectedClient {
                def,
                source: DetectionSource::Path(bin_path),
            });
        }
    }
    results
}

#[cfg(target_os = "windows")]
fn scan_win_paths(paths: &[&str]) -> Option<PathBuf> {
    for raw in paths {
        let expanded = expand_win_env(raw);
        let p = PathBuf::from(&expanded);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Replace a leading `$LOCALAPPDATA` / `$APPDATA` / `$USERPROFILE` with its
/// value. Prefix-only — not general env substitution.
#[cfg(target_os = "windows")]
fn expand_win_env(s: &str) -> String {
    let mut out = s.to_string();
    for var in ["LOCALAPPDATA", "APPDATA", "USERPROFILE"] {
        let placeholder = format!("${var}");
        if out.starts_with(&placeholder) {
            if let Ok(val) = std::env::var(var) {
                out = out.replacen(&placeholder, &val, 1);
            }
        }
    }
    out
}
