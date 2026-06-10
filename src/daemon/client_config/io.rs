//! Atomic file I/O primitives used by the IDE config writers.
//!
//! Provides `atomic_write_json` / `atomic_write_toml` (directory-create +
//! tempfile + platform-safe rename) and the shared `tmp_path_for` naming
//! helper. The platform-safe rename itself lives in `crate::atomic_io`,
//! shared with the installer.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde_json::Value as JsonValue;

use super::ClientError;
use crate::atomic_io::atomic_rename_or_replace;
use crate::daemon::time_util::now_unix_nanos;

pub(super) fn atomic_write_json(path: &Path, value: &JsonValue) -> Result<(), ClientError> {
    let mut serialized = serde_json::to_vec_pretty(value)?;
    serialized.push(b'\n');
    write_atomic(path, &serialized)
}

pub(super) fn atomic_write_toml(path: &Path, value: &toml::Value) -> Result<(), ClientError> {
    let mut serialized = toml::to_string_pretty(value)?.into_bytes();
    if !serialized.ends_with(b"\n") {
        serialized.push(b'\n');
    }
    write_atomic(path, &serialized)
}

/// Write already-serialized TOML (from `toml_edit::DocumentMut::to_string()`) atomically.
pub(super) fn atomic_write_toml_str(path: &Path, text: &str) -> Result<(), ClientError> {
    let mut bytes = text.as_bytes().to_vec();
    if !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    write_atomic(path, &bytes)
}

/// Tempfile + platform-safe rename. Skips the write entirely when the file is
/// already byte-identical — keeps the startup config reconcile idempotent (no
/// gratuitous rewrites, no comment-stripping churn when nothing changed).
fn write_atomic(path: &Path, serialized: &[u8]) -> Result<(), ClientError> {
    if fs::read(path).is_ok_and(|existing| existing == serialized) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let tmp = tmp_path_for(path)?;
    {
        let mut f = fs::File::create(&tmp).map_err(ClientError::Io)?;
        f.write_all(serialized).map_err(ClientError::Io)?;
        f.sync_all().map_err(ClientError::Io)?;
    }
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
