//! First-run IDE auto-detection. Spec §IDE auto-detect.
//!
//! Scans `$PATH` and per-OS app locations (macOS `.app` bundles) for the
//! supported IDEs described in `IDE_SPECS`. `detect_installed` is best-effort:
//! it never panics or errors, returning whatever it found. `IdeKind` is the
//! canonical identity used across the DB and IDE-config writers.

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
    Codex,
}

impl IdeKind {
    pub const fn as_db_kind(self) -> &'static str {
        match self {
            Self::ClaudeCode => "ClaudeCode",
            Self::Cursor => "Cursor",
            Self::VsCodeCopilot => "VsCodeCopilot",
            Self::OpenCode => "OpenCode",
            Self::OpenClaude => "OpenClaude",
            Self::Codex => "Codex",
        }
    }

    pub fn from_db_kind(s: &str) -> Option<Self> {
        match s {
            "ClaudeCode" => Some(Self::ClaudeCode),
            "Cursor" => Some(Self::Cursor),
            "VsCodeCopilot" => Some(Self::VsCodeCopilot),
            "OpenCode" => Some(Self::OpenCode),
            "OpenClaude" => Some(Self::OpenClaude),
            "Codex" => Some(Self::Codex),
            _ => None,
        }
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Cursor => "Cursor",
            Self::VsCodeCopilot => "VS Code Copilot",
            Self::OpenCode => "OpenCode",
            Self::OpenClaude => "OpenClaude",
            Self::Codex => "Codex",
        }
    }

    pub const ALL: &'static [Self] = &[
        Self::ClaudeCode,
        Self::Cursor,
        Self::VsCodeCopilot,
        Self::OpenCode,
        Self::OpenClaude,
        Self::Codex,
    ];
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

struct IdeSpec {
    kind: IdeKind,
    display_name: &'static str,
    binaries: &'static [&'static str],
    #[cfg(target_os = "macos")]
    mac_app: Option<&'static str>,
    #[cfg(target_os = "windows")]
    win_paths: &'static [&'static str],
}

const IDE_SPECS: &[IdeSpec] = &[
    IdeSpec {
        kind: IdeKind::ClaudeCode,
        display_name: "Claude Code",
        binaries: &["claude"],
        #[cfg(target_os = "macos")]
        mac_app: Some("Claude.app"),
        #[cfg(target_os = "windows")]
        win_paths: &["$APPDATA\\npm\\claude.cmd"],
    },
    IdeSpec {
        kind: IdeKind::Cursor,
        display_name: "Cursor",
        binaries: &["cursor"],
        #[cfg(target_os = "macos")]
        mac_app: Some("Cursor.app"),
        #[cfg(target_os = "windows")]
        win_paths: &["$LOCALAPPDATA\\Programs\\Cursor\\bin\\cursor.cmd"],
    },
    IdeSpec {
        kind: IdeKind::VsCodeCopilot,
        display_name: "VS Code",
        binaries: &["code"],
        #[cfg(target_os = "macos")]
        mac_app: Some("Visual Studio Code.app"),
        #[cfg(target_os = "windows")]
        win_paths: &[
            "$LOCALAPPDATA\\Programs\\Microsoft VS Code\\bin\\code.cmd",
            "C:\\Program Files\\Microsoft VS Code\\bin\\code.cmd",
        ],
    },
    IdeSpec {
        kind: IdeKind::OpenCode,
        display_name: "OpenCode",
        binaries: &["opencode"],
        #[cfg(target_os = "macos")]
        mac_app: Some("OpenCode.app"),
        #[cfg(target_os = "windows")]
        win_paths: &["$LOCALAPPDATA\\Programs\\OpenCode\\bin\\opencode.cmd"],
    },
    IdeSpec {
        kind: IdeKind::OpenClaude,
        display_name: "OpenClaude",
        binaries: &["openclaude"],
        #[cfg(target_os = "macos")]
        mac_app: None,
        #[cfg(target_os = "windows")]
        win_paths: &[],
    },
    IdeSpec {
        kind: IdeKind::Codex,
        display_name: "Codex",
        binaries: &["codex"],
        #[cfg(target_os = "macos")]
        mac_app: None,
        #[cfg(target_os = "windows")]
        win_paths: &["$APPDATA\\npm\\codex.cmd"],
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
    if candidate.is_dir() {
        Some(candidate)
    } else {
        None
    }
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

        // Windows: try well-known install paths, then PATH
        #[cfg(target_os = "windows")]
        if let Some(win_path) = scan_win_paths(spec.win_paths) {
            results.push(DetectedIde {
                kind: spec.kind,
                display_name: spec.display_name,
                source: DetectionSource::WinApp(win_path),
            });
            continue;
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

/// Expand `$LOCALAPPDATA` / `$APPDATA` / `$USERPROFILE` prefixes and check
/// if the resulting path exists as a file.
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

/// Replace `$LOCALAPPDATA`, `$APPDATA`, `$USERPROFILE` with their values.
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
