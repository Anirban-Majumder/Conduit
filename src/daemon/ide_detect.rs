// IDE auto-detect at first-run. Spec §IDE auto-detect.
// Linux/macOS/Windows: scan $PATH + per-OS app locations for the 5 supported IDEs.

use std::path::PathBuf;

#[cfg(target_os = "macos")]
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IdeKind {
    ClaudeCode,
    Cursor,
    VsCodeCopilot,
    OpenCode,
    OpenClaude,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedIde {
    pub kind: IdeKind,
    pub display_name: &'static str,
    pub source: DetectionSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetectionSource {
    /// Found as an executable in $PATH at the given resolved path.
    Path(PathBuf),
    /// Found as a macOS application bundle.
    MacApp(PathBuf),
}

struct IdeSpec {
    kind: IdeKind,
    display_name: &'static str,
    binaries: &'static [&'static str],
    #[cfg(target_os = "macos")]
    mac_app: Option<&'static str>,
}

const IDE_SPECS: &[IdeSpec] = &[
    IdeSpec {
        kind: IdeKind::ClaudeCode,
        display_name: "Claude Code",
        binaries: &["claude"],
        #[cfg(target_os = "macos")]
        mac_app: Some("Claude.app"),
    },
    IdeSpec {
        kind: IdeKind::Cursor,
        display_name: "Cursor",
        binaries: &["cursor"],
        #[cfg(target_os = "macos")]
        mac_app: Some("Cursor.app"),
    },
    IdeSpec {
        kind: IdeKind::VsCodeCopilot,
        display_name: "VS Code",
        binaries: &["code"],
        #[cfg(target_os = "macos")]
        mac_app: Some("Visual Studio Code.app"),
    },
    IdeSpec {
        kind: IdeKind::OpenCode,
        display_name: "OpenCode",
        binaries: &["opencode"],
        #[cfg(target_os = "macos")]
        mac_app: Some("OpenCode.app"),
    },
    IdeSpec {
        kind: IdeKind::OpenClaude,
        display_name: "OpenClaude",
        binaries: &["openclaude"],
        #[cfg(target_os = "macos")]
        mac_app: None,
    },
];

/// Scan PATH directories for a binary matching any of the given names.
/// Returns the resolved path of the first match found.
pub fn scan_paths(paths: &[PathBuf], binary_names: &[&str]) -> Option<PathBuf> {
    for dir in paths {
        for name in binary_names {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
            // Windows: also try with .exe suffix
            #[cfg(target_os = "windows")]
            {
                let with_exe = dir.join(format!("{name}.exe"));
                if with_exe.is_file() {
                    return Some(with_exe);
                }
            }
        }
    }
    None
}

/// Check if an .app bundle exists under apps_root.
#[cfg(target_os = "macos")]
pub fn scan_apps(apps_root: &Path, app_name: &str) -> Option<PathBuf> {
    let candidate = apps_root.join(app_name);
    if candidate.is_dir() { Some(candidate) } else { None }
}

/// Scan for installed IDEs across the current OS. Uses only `std::env` / `std::fs` (no external crate) —
/// no external crate. Never panics, never errors: returns whatever it found.
pub fn detect_installed() -> Vec<DetectedIde> {
    let path_dirs: Vec<PathBuf> = std::env::var("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();

    let mut results = Vec::new();

    for spec in IDE_SPECS {
        // macOS: try .app bundle first, then PATH
        #[cfg(target_os = "macos")]
        if let Some(app_name) = spec.mac_app {
            if let Some(app_path) = scan_apps(Path::new("/Applications"), app_name) {
                results.push(DetectedIde {
                    kind: spec.kind,
                    display_name: spec.display_name,
                    source: DetectionSource::MacApp(app_path),
                });
                continue;
            }
        }

        if let Some(bin_path) = scan_paths(&path_dirs, spec.binaries) {
            results.push(DetectedIde {
                kind: spec.kind,
                display_name: spec.display_name,
                source: DetectionSource::Path(bin_path),
            });
        }
    }

    results
}
