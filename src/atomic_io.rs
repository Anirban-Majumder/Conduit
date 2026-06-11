//! Cross-platform atomic file replacement, shared by the installer and the IDE
//! config writers.
//!
//! `atomic_rename_or_replace` does a plain POSIX `fs::rename` on Unix and falls
//! back to `ReplaceFileW` on Windows when the destination is locked (an IDE
//! holding its config open). Both call sites write to a tempfile first, then
//! call this — so the Windows FFI lives in exactly one place instead of being
//! copy-pasted.

use std::fs;
use std::path::Path;

#[cfg(windows)]
pub fn atomic_rename_or_replace(tmp: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;

    // These could be imported from windows_sys::Win32_Foundation, but
    // the local defs avoid pulling in additional feature gates.
    const ERROR_ACCESS_DENIED: i32 = 5;
    const ERROR_SHARING_VIOLATION: i32 = 32;
    const REPLACEFILE_IGNORE_ACL_ERRORS: u32 = 0x0000_0004;

    match fs::rename(tmp, target) {
        Ok(()) => return Ok(()),
        Err(e) => {
            let raw = e.raw_os_error();
            if raw != Some(ERROR_ACCESS_DENIED) && raw != Some(ERROR_SHARING_VIOLATION) {
                return Err(e);
            }
        }
    }

    let replaced: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    let replacement: Vec<u16> = tmp.as_os_str().encode_wide().chain(Some(0)).collect();

    tracing::debug!(
        ?target,
        "rename failed (likely IDE lock), falling back to ReplaceFileW"
    );

    #[expect(
        unsafe_code,
        reason = "ReplaceFileW FFI — documented Windows API for atomic file replacement"
    )]
    let ok = unsafe {
        ReplaceFileW(
            replaced.as_ptr(),
            replacement.as_ptr(),
            std::ptr::null(),
            REPLACEFILE_IGNORE_ACL_ERRORS,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };

    // No parent-directory fsync here: Windows has no directory-handle fsync
    // equivalent, and both `fs::rename` (MoveFileEx) and `ReplaceFileW` are
    // already metadata-atomic. File *contents* are fsync'd by the caller before
    // the rename. The durability gap (a crash losing the rename) is the
    // documented Unix-only hardening.
    if ok != 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(windows))]
pub fn atomic_rename_or_replace(tmp: &Path, target: &Path) -> std::io::Result<()> {
    // CC-5: if `target` is a symlink (stow/chezmoi), resolve it so we rename
    // onto the real file and avoid replacing the symlink with a regular file.
    // When the resolved path is in a different directory we relocate the temp
    // file there first so that the final rename stays same-filesystem.
    symlink_aware_rename(tmp, target)?;
    // Best-effort parent-dir fsync so the rename itself is durable.
    let fsync_dir = fs::canonicalize(target)
        .unwrap_or_else(|_| target.to_owned())
        .parent()
        .map(Path::to_owned);
    if let Some(parent) = fsync_dir {
        if let Ok(dir) = fs::File::open(&parent) {
            if let Err(e) = dir.sync_all() {
                tracing::trace!("parent dir fsync failed (non-fatal): {e}");
            }
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn symlink_aware_rename(tmp: &Path, target: &Path) -> std::io::Result<()> {
    let real_target = fs::canonicalize(target).unwrap_or_else(|_| target.to_owned());
    if real_target == target {
        return fs::rename(tmp, target);
    }
    // Symlink target is in a different dir; move the temp there first.
    let real_parent = real_target.parent().unwrap_or_else(|| Path::new("."));
    let file_name = tmp
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new(".conduit.tmp"));
    let moved_tmp = real_parent.join(file_name);
    if fs::rename(tmp, &moved_tmp).is_err() {
        // Cross-filesystem symlink target (EXDEV): copy into the resolved
        // parent instead — renaming over the symlink itself would replace the
        // user's symlink with a regular file (the CC-5 bug).
        fs::copy(tmp, &moved_tmp)?;
        let _ignored = fs::remove_file(tmp);
    }
    fs::rename(&moved_tmp, &real_target)
}
