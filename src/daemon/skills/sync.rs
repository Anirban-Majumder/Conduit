// Mirror a managed-skill directory into each agentic IDE's skills folder.
// Prefer a directory symlink (cheap, edits propagate); fall back to recursive
// copy when symlinks aren't available (sandboxed Windows accounts, etc.).

use std::fs;
use std::path::Path;

use crate::daemon::errors::SkillError;
use crate::daemon::clients::{self, ide_kind_from_db_name};

pub fn sync_to_target(source: &Path, ide_db_name: &str) -> Result<(), SkillError> {
    let ide = ide_kind_from_db_name(ide_db_name)
        .ok_or_else(|| SkillError::UnknownTarget(ide_db_name.to_string()))?;
    let Some(base) = clients::skills_dir_for(ide) else {
        return Err(SkillError::UnknownTarget(ide_db_name.to_string()));
    };
    let install_name = source
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| SkillError::InvalidPath(source.display().to_string()))?;
    fs::create_dir_all(&base)?;
    let dest = base.join(install_name);
    // Atomic swap: write the new link/dir under a sibling tmp name, then
    // rename over `dest`. Avoids the race window where another process could
    // create something at `dest` between our remove and re-create.
    let staging = base.join(format!(".{install_name}.staging-{}", staging_nonce()));
    remove_path(&staging);
    let needs_copy = try_symlink_dir(source, &staging).is_err();
    if needs_copy {
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

pub fn remove_from_target(install_name: &str, ide_db_name: &str) {
    let Some(ide) = ide_kind_from_db_name(ide_db_name) else {
        return;
    };
    let Some(base) = clients::skills_dir_for(ide) else {
        return;
    };
    let dest = base.join(install_name);
    remove_path(&dest);
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
