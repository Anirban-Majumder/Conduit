//! Mirror a managed-skill directory into an editor's skills folder.
//!
//! Pure path operations (D5): the caller resolves the absolute destination dir
//! (`ResolvedClient::skills_target(scope)`) and the `sync_mode`; this module
//! never reaches into the IDE catalog. `Symlink` mode tries a directory symlink
//! (cheap, edits propagate) and falls back to copy; `Copy` always copies. Writes
//! go through a sibling staging path then atomic rename. Synchronous `std::fs`
//! by design — callers invoke these from `spawn_blocking`.

use std::fs;
use std::path::Path;

use crate::daemon::config::toml::SyncMode;
use crate::daemon::errors::SkillError;

/// Mirror `source` into `dest_dir/install_name` using `mode`. `dest_dir` is the
/// editor's skills base for this scope (already resolved by the caller).
///
/// `we_manage`: when `true` the caller holds a `managed_skill` DB row for this
/// `(dest_dir, install_name)` pair, meaning Conduit wrote the destination and
/// may safely overwrite it. When `false` and the destination already exists,
/// this function returns `SkillError::TargetExists` instead of overwriting
/// user-authored content.
pub fn sync_to_target(
    source: &Path,
    dest_dir: &Path,
    install_name: &str,
    mode: SyncMode,
    we_manage: bool,
) -> Result<(), SkillError> {
    fs::create_dir_all(dest_dir)?;
    let dest = dest_dir.join(install_name);

    // SK-1: refuse to overwrite a destination we didn't create.
    if !we_manage && dest.exists() {
        tracing::warn!(
            dest = %dest.display(),
            "skipping sync: destination exists and is not managed by Conduit"
        );
        return Err(SkillError::TargetExists(dest));
    }

    // Atomic swap: write the new link/dir under a sibling tmp name, then
    // rename over `dest`. Avoids the race window where another process could
    // create something at `dest` between our remove and re-create.
    let staging = dest_dir.join(format!(".{install_name}.staging-{}", staging_nonce()));
    remove_path(&staging);
    let use_copy = match mode {
        SyncMode::Copy => true,
        SyncMode::Symlink => try_symlink_dir(source, &staging).is_err(),
    };
    if use_copy {
        if let Err(e) = copy_dir_all(source, &staging) {
            remove_path(&staging);
            return Err(SkillError::from(e));
        }
    }
    remove_path(&dest);
    if let Err(e) = fs::rename(&staging, &dest) {
        remove_path(&staging);
        return Err(SkillError::Io(e));
    }
    Ok(())
}

fn staging_nonce() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Remove a mirrored skill at `dest_dir/install_name`. Missing path is a no-op.
pub fn remove_from_target(dest_dir: &Path, install_name: &str) {
    remove_path(&dest_dir.join(install_name));
}

#[cfg(unix)]
fn try_symlink_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

#[cfg(windows)]
fn try_symlink_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(src, dst)
}

fn remove_path(p: &Path) {
    if let Ok(meta) = fs::symlink_metadata(p) {
        if meta.file_type().is_symlink() || meta.is_file() {
            let _ignored = fs::remove_file(p);
        } else if meta.is_dir() {
            let _ignored = fs::remove_dir_all(p);
        }
    }
}

pub fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&from, &to)?;
        } else if ty.is_file() {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}
