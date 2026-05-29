#![allow(clippy::unwrap_used, reason = "test ergonomics")]
#![allow(clippy::expect_used, reason = "test ergonomics")]

use std::fs;
use std::path::PathBuf;

use conduit::daemon::ide_detect::{detect_installed, scan_paths, DetectionSource};

#[cfg(target_os = "macos")]
use conduit::daemon::ide_detect::scan_apps;

// ── scan_paths unit tests ────────────────────────────────────────────────────

struct TempDir(PathBuf);

impl TempDir {
    fn new(suffix: &str) -> Self {
        let mut path = std::env::temp_dir();
        path.push(format!("conduit-test-{suffix}-{}", std::process::id()));
        fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }

    const fn path(&self) -> &PathBuf {
        &self.0
    }

    fn create_file(&self, name: &str) -> PathBuf {
        let p = self.0.join(name);
        fs::write(&p, b"").expect("create temp file");
        p
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ignored = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn scan_paths_finds_matching_binary() {
    let dir = TempDir::new("scan-found");
    dir.create_file("mybin");

    let found = scan_paths(&[dir.path().clone()], &["mybin"]);
    assert_eq!(found, Some(dir.path().join("mybin")));
}

#[test]
fn scan_paths_returns_none_when_missing() {
    let dir = TempDir::new("scan-missing");
    // No files created.
    let found = scan_paths(&[dir.path().clone()], &["nothere"]);
    assert_eq!(found, None);
}

#[test]
fn scan_paths_returns_none_on_empty_paths() {
    let found = scan_paths(&[], &["anything"]);
    assert_eq!(found, None);
}

#[test]
fn scan_paths_returns_none_on_empty_names() {
    let dir = TempDir::new("scan-nonames");
    dir.create_file("something");
    let found = scan_paths(&[dir.path().clone()], &[]);
    assert_eq!(found, None);
}

#[test]
fn scan_paths_first_match_wins_across_dirs() {
    let dir1 = TempDir::new("scan-first1");
    let dir2 = TempDir::new("scan-first2");
    dir2.create_file("mybin");
    // dir1 has no matching file; dir2 does.
    let found = scan_paths(&[dir1.path().clone(), dir2.path().clone()], &["mybin"]);
    assert_eq!(found, Some(dir2.path().join("mybin")));
}

#[test]
fn scan_paths_first_binary_name_wins_within_dir() {
    let dir = TempDir::new("scan-namesorder");
    dir.create_file("beta");
    // "alpha" absent; "beta" present — should find beta.
    let found = scan_paths(&[dir.path().clone()], &["alpha", "beta"]);
    assert_eq!(found, Some(dir.path().join("beta")));
}

#[test]
fn scan_paths_does_not_match_directory() {
    let outer = TempDir::new("scan-notdir");
    // Create a sub-directory named the same as the target binary.
    let subdir = outer.path().join("mybin");
    fs::create_dir_all(&subdir).expect("create subdir");
    let found = scan_paths(&[outer.path().clone()], &["mybin"]);
    // is_file() on a directory returns false — should not match.
    assert_eq!(found, None);
}

// ── scan_apps unit tests (macOS only) ────────────────────────────────────────

#[cfg(target_os = "macos")]
#[test]
fn scan_apps_finds_app_bundle() {
    let root = TempDir::new("apps-found");
    let app_path = root.path().join("MyApp.app");
    fs::create_dir_all(&app_path).expect("create app bundle dir");

    let found = scan_apps(root.path(), "MyApp.app");
    assert_eq!(found, Some(app_path));
}

#[cfg(target_os = "macos")]
#[test]
fn scan_apps_returns_none_when_missing() {
    let root = TempDir::new("apps-missing");
    let found = scan_apps(root.path(), "NotThere.app");
    assert_eq!(found, None);
}

#[cfg(target_os = "macos")]
#[test]
fn scan_apps_does_not_match_file_with_app_name() {
    let root = TempDir::new("apps-file");
    // Create a regular file where a bundle (directory) is expected.
    fs::write(root.path().join("Fake.app"), b"").expect("write file");
    let found = scan_apps(root.path(), "Fake.app");
    // .app bundles are directories; a plain file must not match.
    assert_eq!(found, None);
}

// ── detect_installed smoke test ───────────────────────────────────────────────

#[test]
fn detect_installed_returns_vec_without_panic() {
    let results = detect_installed();
    // May be empty on CI; just assert it doesn't panic and returns a Vec.
    for ide in &results {
        match &ide.source {
            DetectionSource::Path(p) | DetectionSource::WinApp(p) => assert!(p.is_file()),
            DetectionSource::MacApp(p) => assert!(p.is_dir()),
        }
    }
}
