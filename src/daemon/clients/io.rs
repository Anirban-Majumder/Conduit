//! Atomic file I/O primitives used by the IDE config writers.
//!
//! Provides `atomic_write_json` / `atomic_write_toml` (directory-create +
//! tempfile + platform-safe rename) and the shared `tmp_path_for` naming
//! helper. The Windows arm uses `ReplaceFileW` to replace a locked file;
//! the Unix arm uses plain `fs::rename` (POSIX atomic replace).

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value as JsonValue;

use super::ClientError;
use crate::daemon::time_util::now_unix_nanos;

pub(super) fn atomic_write_json(path: &Path, value: &JsonValue) -> Result<(), ClientError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let mut serialized = serde_json::to_vec_pretty(value)?;
    serialized.push(b'\n');
    let tmp = tmp_path_for(path)?;
    fs::write(&tmp, &serialized)?;
    atomic_rename_or_replace(&tmp, path).map_err(|e| {
        let _ignored = fs::remove_file(&tmp);
        ClientError::Io(e)
    })
}

pub(super) fn atomic_write_toml(path: &Path, value: &toml::Value) -> Result<(), ClientError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let mut serialized = toml::to_string_pretty(value)?.into_bytes();
    if !serialized.ends_with(b"\n") {
        serialized.push(b'\n');
    }
    let tmp = tmp_path_for(path)?;
    fs::write(&tmp, &serialized)?;
    atomic_rename_or_replace(&tmp, path).map_err(|e| {
        let _ignored = fs::remove_file(&tmp);
        ClientError::Io(e)
    })
}

fn tmp_path_for(path: &Path) -> Result<PathBuf, ClientError> {
    let file_name = path
        .file_name()
        .ok_or(ClientError::NonUtf8Path)?
        .to_str()
        .ok_or(ClientError::NonUtf8Path)?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let pid = std::process::id();
    let nonce = now_unix_nanos();
    Ok(parent.join(format!(".{file_name}.{pid}.{nonce}.tmp")))
}

#[cfg(windows)]
fn atomic_rename_or_replace(tmp: &Path, target: &Path) -> std::io::Result<()> {
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
fn atomic_rename_or_replace(tmp: &Path, target: &Path) -> std::io::Result<()> {
    fs::rename(tmp, target)
}
