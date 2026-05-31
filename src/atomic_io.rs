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

    if ok != 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(windows))]
pub fn atomic_rename_or_replace(tmp: &Path, target: &Path) -> std::io::Result<()> {
    fs::rename(tmp, target)
}
