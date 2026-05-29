# Windows-Specific Changes Plan

## Status: Comprehensive Review Complete (2026-05-29)

The codebase **already compiles on Windows** (`cargo check` passes, toolchain pinned to `1.90.0-x86_64-pc-windows-msvc`). These are functional improvements, not build fixes.

This plan covers **all** known Windows gaps — build, correctness, security, and UX. Items are ordered by priority within each severity tier.

---

## HIGH — Correctness / Security

### 1. Job Object for Grandchild Process Cleanup

**File:** `src/daemon/upstream/stdio.rs`, `src/daemon/upstream/job_object.rs`

**Problem:** `kill_on_drop(true)` only kills the direct child. Node/Python MCP servers spawn worker processes (child_process.fork, worker threads) that survive and leak. The TODO at `stdio.rs:3` has tracked this since v0.

**Critical finding: The existing `job_object.rs` is fundamentally broken and should be deleted.** Six issues:

| Issue | Severity | Description |
|-------|----------|-------------|
| Dead code | HIGH | `assign_child_to_job(&tokio::process::Child)` is unreachable — `TokioChildProcess::new(cmd)` spawns internally, caller never has a `Child` handle |
| Race condition | HIGH | `AssignProcessToJobObject` is called *after* the child is running. Window between `CreateProcess` and assignment where grandchildren can spawn outside the job |
| Global job, no cleanup | MEDIUM | `OnceLock<HANDLE>` lives forever. `KILL_ON_JOB_CLOSE` only fires at process exit — grandchildren survive individual MCP server restarts |
| Cross-contamination | MEDIUM | All MCP servers share one job. No per-server isolation |
| Wrong sentinel | LOW | Stores `0` on failure but `HANDLE` is `isize` — should use `INVALID_HANDLE_VALUE` (-1) |
| Redundant dep | LOW | Adds `windows-sys` when `process-wrap` (already in tree) uses `windows` crate |

**Solution: Use `process-wrap` (already a transitive dependency of `rmcp`).**

`process-wrap` 9.1.0 is in `Cargo.lock` via `rmcp`. It provides `process_wrap::tokio::JobObject` — a production-quality wrapper that:
1. Spawns with `CREATE_SUSPENDED` (prevents the race condition)
2. Creates a **per-spawn** job with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`
3. Associates an I/O completion port for proper child notification
4. Assigns the suspended process to the job, then resumes threads
5. Wraps in `JobObjectChild` whose `Drop` closes the job handle (killing all descendants)
6. Provides `start_kill()` that calls `TerminateJobObject` (kills entire process tree)

**Implementation:**
```rust
// In stdio::connect(), replace the current spawn logic:

#[cfg(windows)]
{
    use process_wrap::tokio::{CommandWrap, JobObject};
    let wrapped = CommandWrap::from(cmd).wrap(JobObject);
    let transport = TokioChildProcess::new(wrapped)?;
}
#[cfg(not(windows))]
{
    let transport = TokioChildProcess::new(cmd)?;
}
```

**Additional steps:**
1. Delete `src/daemon/upstream/job_object.rs` (dead code, broken implementation)
2. Add `process-wrap = { version = "9", features = ["tokio1"] }` to `[target.'cfg(windows)'.dependencies]`
3. Remove the `windows-sys` dependency (no longer needed — items #3 and #6 below use `icacls`/`ReplaceFileW` via different paths)
4. Remove the TODO comment at `stdio.rs:3-5`
5. Keep `kill_on_drop(true)` — it's redundant with `JobObjectChild::start_kill()` but harmless defense-in-depth

**Race condition detail (why `process-wrap` is correct):**
The manual implementation calls `AssignProcessToJobObject` after the child is already running. Between `CreateProcess` and `AssignProcessToJobObject`, the child can spawn its own children that are NOT in the job. `process-wrap` solves this by spawning with `CREATE_SUSPENDED`, assigning to the job, then resuming threads — the only correct way to guarantee no leaked grandchildren.

**Per-spawn vs global job (why `process-wrap` is correct):**
The manual `OnceLock<HANDLE>` creates one global job for all MCP servers. The handle never closes during normal operation, so `KILL_ON_JOB_CLOSE` never fires for individual server shutdowns. `process-wrap` creates a per-spawn `JobPort` — each server's process tree is isolated and cleaned up independently.

**Effort:** ~10 lines + Cargo.toml change + delete `job_object.rs`.

---

### 2. DB File Permissions on Windows

**File:** `src/daemon/db/mod.rs:960-975`

**Problem:** `restrict_db_perms` is `#[cfg(all(unix, not(debug_assertions)))]`. Release builds on Windows leave `state.db`, `state.db-wal`, and `state.db-shm` with default ACLs. On multi-user machines, other users can read the database.

**Current Unix behavior (lines 960-975):**
```rust
#[cfg(all(unix, not(debug_assertions)))]
fn restrict_db_perms(path: &Path) {
    // Loops over ["", "-wal", "-shm"], sets chmod 0600
}
```

**Solution:** Use `icacls` via `Command`, not raw `SetNamedSecurityInfoW`.

**Why `icacls` over `SetNamedSecurityInfoW`:**
- `icacls` handles DACL construction, inheritance stripping, and Unicode paths internally
- No `unsafe` blocks or manual memory management (no `SECURITY_DESCRIPTOR`, no `LocalFree`)
- Subprocess cost (~20ms) is negligible for a once-at-startup operation
- Battle-tested across billions of Windows installations
- `SetNamedSecurityInfoW` requires ~30-40 lines of unsafe FFI with multiple fallible allocations and cleanup points — high bug surface for security-critical code

**DACL (Access Control List):**

| Principal | SID | Permission | Why |
|-----------|-----|------------|-----|
| Current user | (resolved by icacls) | Full control (F) | Owner, equivalent of Unix 0600 |
| `NT AUTHORITY\SYSTEM` | `*S-1-5-18` | Full control (F) | Windows services run as SYSTEM; SQLite WAL mode needs service account access |
| `BUILTIN\Administrators` | `*S-1-5-32-544` | Full control (F) | Safety net for admin recovery; equivalent of Unix root always having access |

**Why SYSTEM is required:** Without it, a Windows service restart could fail to open the DB. The Unix equivalent implicitly allows root; this is the Windows counterpart.

**Implementation:**
```rust
#[cfg(all(windows, not(debug_assertions)))]
fn restrict_db_perms(path: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        let p = PathBuf::from(&name);
        if !p.exists() {
            continue;
        }
        let status = std::process::Command::new("icacls")
            .arg(&p)
            .args([
                "/inheritance:r",   // strip inherited ACEs (critical)
                "/grant:r",         // replace, not add
                "*S-1-5-18:F",      // SYSTEM full control
                "*S-1-5-32-544:F",  // Administrators full control
            ])
            .status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => tracing::warn!(path = %p.display(), status = %s, "icacls failed"),
            Err(e) => tracing::warn!(path = %p.display(), error = %e, "icacls spawn failed"),
        }
    }
}
```

**Key details:**
- Using SIDs (`*S-1-5-18`, `*S-1-5-32-544`) instead of localized names avoids i18n issues on non-English Windows
- `/inheritance:r` is critical — without it, `BUILTIN\Users` may retain inherited read access from `%APPDATA%`
- `state.db` is always under `dirs::config_dir()` (`%APPDATA%`), which is on the system NTFS volume
- Non-NTFS volumes (FAT32, exFAT) don't support ACLs — `icacls` will fail, logged as warning (not fatal)
- Network shares use share-level ACLs, not local NTFS — `icacls` may succeed but permissions have no effect (unlikely scenario since `%APPDATA%` is always local)

**Deliberate design decision: warn, don't fail.** Crashing the daemon over missing ACLs on FAT32 (a non-standard deployment) would be worse than the security gap. On NTFS (99.9% of Windows), failure here indicates a real problem worth investigating. This is not cargo-cult protection — it's a conscious trade-off: availability over security hardening on edge-case filesystems. The warning log ensures the gap is observable.

**Add to call site (line 268):**
```rust
#[cfg(all(windows, not(debug_assertions)))]
restrict_db_perms(path);
```

**Effort:** ~20 lines.

---

## MEDIUM — UX / Robustness

### 3. Atomic Rename on Windows with ReplaceFileW

**File:** `src/daemon/clients/mod.rs:346-365, 447-468`

**Problem:** `fs::rename` on Windows calls `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`. This does overwrite, but fails with `ERROR_ACCESS_DENIED` (5) or `ERROR_SHARING_VIOLATION` (32) when the target is held open by an IDE (VS Code, Cursor use `FileSystemWatcher` which holds handles on config directories).

**Current behavior (both `atomic_write_json` and `atomic_write_toml`):**
```rust
let tmp = tmp_path_for(path)?;
fs::write(&tmp, &serialized)?;
match fs::rename(&tmp, path) {
    Ok(()) => Ok(()),
    Err(e) => {
        let _ignored = fs::remove_file(&tmp);
        Err(ClientError::Io(e))  // fails cleanly, target untouched
    }
}
```

**Why NOT `fs::copy` as fallback (original plan proposed this):**
- `fs::copy` is non-atomic — reads in chunks, writes in chunks
- If interrupted mid-copy, target file is corrupted (partial write or zero-length)
- No `fs::sync_data` call, so even on clean exit OS buffering could lose data
- Current fail-clean behavior is strictly safer than a non-atomic overwrite

**Solution:** Three-tier fallback using `ReplaceFileW`:

```rust
#[cfg(windows)]
fn atomic_rename_or_replace(tmp: &Path, target: &Path) -> std::io::Result<()> {
    // Tier 1: plain rename (fast, works 99% of the time)
    match fs::rename(tmp, target) {
        Ok(()) => return Ok(()),
        Err(e) => {
            let raw = e.raw_os_error();
            // Only fall back for lock-related errors
            if raw != Some(5) && raw != Some(32) {
                let _ = fs::remove_file(tmp);
                return Err(e);
            }
        }
    }

    // Tier 2: ReplaceFileW (opens target with permissive sharing modes)
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;

    let replaced: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    let replacement: Vec<u16> = tmp.as_os_str().encode_wide().chain(Some(0)).collect();

    tracing::debug!(?target, "rename failed (likely IDE lock), falling back to ReplaceFileW");

    let ok = unsafe {
        // SAFETY: replaced and replacement are valid, null-terminated UTF-16
        // buffers owned by this function. ReplaceFileW borrows the pointers
        // and does not take ownership. The null pointers for backup file and
        // security attributes are documented as optional in the Windows API.
        ReplaceFileW(
            replaced.as_ptr(),
            replacement.as_ptr(),
            std::ptr::null(),       // no backup file
            0x00000004,             // REPLACEFILE_IGNORE_ACL_ERRORS
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };

    if ok != 0 {
        Ok(())
    } else {
        let _ = fs::remove_file(tmp);
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(windows))]
fn atomic_rename_or_replace(tmp: &Path, target: &Path) -> std::io::Result<()> {
    // POSIX rename(2) is atomic and always overwrites
    fs::rename(tmp, target)
}
```

**Why `ReplaceFileW`:**
- Opens target with `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE` — can replace files even when IDEs have them open
- Preserves metadata (creation time, ACLs, compression, encryption, named streams)
- `REPLACEFILE_IGNORE_ACL_ERRORS` avoids failures when caller lacks `WRITE_DAC` (common for non-admin)
- [Microsoft's documented recommendation for atomic file replacement](https://learn.microsoft.com/en-us/windows/win32/fileio/moving-and-replacing-files)

**AV / Defender note:** Windows Defender real-time scanning can briefly lock temp files (50-200ms) after `fs::write` creates them. If `ReplaceFileW` also fails with a lock error, a single 100ms retry handles this. Not included in the basic implementation above — add only if users report intermittent failures.

**`windows-sys` feature needed:** Add `"Win32_Storage_FileSystem"` to the existing dependency in `Cargo.toml`.

**Integration:** Replace both `atomic_write_json` and `atomic_write_toml` to call `atomic_rename_or_replace(&tmp, path)` instead of `fs::rename(&tmp, path)`.

**Function placement:** Keep `atomic_rename_or_replace` in `clients/mod.rs` as a private function. It's only used by `atomic_write_json` and `atomic_write_toml` in the same module — no DRY violation, no need for a shared utility. If future modules need atomic rename, extract to `daemon/fs_util.rs` at that point.

**Error type:** `std::io::Result<()>` is correct here. This is a leaf-level I/O function — the caller (`atomic_write_json`/`atomic_write_toml`) already wraps errors into `ClientError::Io(e)`. No new error type needed.

**Effort:** ~25 lines + Cargo.toml feature.

---

### 4. Notification Buttons on Windows

**File:** `src/gui/notifications.rs:153-171`

**Problem:** On Linux, notifications include Approve / Deny / Approve always buttons that forward clicks to the daemon. On macOS and Windows, notifications are fire-and-forget — the non-Linux `spawn_notification` (line 153) shows a plain notification with no actions and no callback.

**Impact:** Windows users must switch to the Conduit window to approve/deny MCP tool calls. Linux users can approve directly from the notification tray.

**Current code (non-Linux path, lines 153-171):**
```rust
#[cfg(not(target_os = "linux"))]
fn spawn_notification(
    _id: String, ide_name: String, server: String, tool: String,
    _action_tx: UnboundedSender<NotificationAction>, _registry: Registry,
) {
    let body = format!("{server} · {tool}");
    if let Err(e) = notify_rust::Notification::new()
        .summary(&ide_name).body(&body).appname("Conduit").show()
    { tracing::warn!(error = %e, "notification show failed"); }
}
```

**Root cause:** `notify-rust` uses different backends per OS:
- Linux: D-Bus (`notify-rust` native) — supports actions
- macOS: `mac-notification-sys` — limited action support, unsigned binary issues
- Windows: WinRT toast notifications — `notify-rust` has basic support but action buttons require COM activation handling

**Options:**
1. **Use `windows-rs` WinRT toast APIs directly** — full control over toast buttons, but significant complexity (~100+ lines of COM interop)
2. **Use `winrt-toast` crate** — higher-level WinRT toast wrapper, may support buttons
3. **Defer** — accept fire-and-forget on Windows, document as known limitation
4. **System tray balloon fallback** — use the existing tray icon for approval prompts (different UX but functional)

**Recommendation:** Option 3 (defer) for v0. Option 4 (tray-based approval) as a future enhancement if user feedback demands it. The notification system works; it's just less convenient on Windows.

**Effort:** 0 lines if deferred; ~100 lines for full WinRT toast with buttons.

---

### 5. Notification Close No-op on Windows

**File:** `src/gui/notifications.rs:90-93`

**Problem:** When an approval is resolved, `close_by_id` is called to dismiss the notification. On Windows, this function is empty:

```rust
#[cfg(target_os = "windows")]
fn close_by_id(_notif_id: u32) {
    // Windows notifications via WinRT don't expose a close handle.
}
```

**Impact:** After approving/denying via the Conduit window, the notification stays visible until the user manually dismisses it or it times out. Confusing UX — user might think the approval didn't go through.

**Root cause:** WinRT toast notifications don't expose a programmatic close API via `notify-rust`. The `NotificationHandle::close()` method available on Linux/macOS has no Windows equivalent.

**Options:**
1. **Use WinRT `ToastNotificationManager::History`** to remove by tag — requires direct WinRT interop
2. **Set a short timeout** on the notification so it auto-dismisses — reduces the window of confusion
3. **Defer** — document as known limitation, same as item #4

**Recommendation:** Option 2 (short timeout) as a minimal improvement. Set `notify_rust::Timeout::Milliseconds(10000)` (10 seconds) on Windows notifications.

**Effort:** ~2 lines (add timeout to the Windows notification).

---

## LOW — Correctness / Polish

### 6. Windows IDE Detection Beyond PATH

**File:** `src/daemon/ide_detect.rs`

**Problem:** `DetectionSource` has `Path` and `MacApp` variants but no `WinApp`. On Windows, IDEs may be installed in `C:\Program Files`, `%LOCALAPPDATA%\Programs\`, or via npm global, and not be on PATH.

**Current code (lines 72-77):**
```rust
pub enum DetectionSource {
    Path(PathBuf),      // Found in $PATH
    MacApp(PathBuf),    // Found as macOS .app bundle
}
```

**Solution:** Add `WinApp(PathBuf)` variant and scan well-known Windows paths.

**Well-known paths to scan:**

| IDE | Path | Install method |
|-----|------|---------------|
| VS Code (User) | `%LOCALAPPDATA%\Programs\Microsoft VS Code\bin\code.cmd` | User installer |
| VS Code (System) | `C:\Program Files\Microsoft VS Code\bin\code.cmd` | System installer |
| Cursor | `%LOCALAPPDATA%\Programs\Cursor\bin\cursor.cmd` | User installer |
| Claude Code | `%APPDATA%\npm\claude.cmd` | npm global |
| Codex | `%APPDATA%\npm\codex.cmd` | npm global |
| OpenCode | `%LOCALAPPDATA%\Programs\OpenCode\bin\opencode.cmd` | If packaged |

**Implementation:**
1. Add `WinApp(PathBuf)` to `DetectionSource` enum
2. Add `as_db_str` arm: `Self::WinApp(p) => format!("winapp:{}", p.display())`
3. Add `#[cfg(target_os = "windows")]` `win_paths` field to `IdeSpec` (like `mac_app` on macOS)
4. In `detect_installed()`, before PATH scan, check well-known Windows paths
5. Update `tests/ide_detect.rs` match to handle `WinApp`

**Note on `.cmd` files:** npm global installs create `.cmd` wrappers. These are the canonical entry points — the actual `.exe` is in a `node_modules` subdirectory. Treat `.cmd` as the binary (consistent with how `which` works on Windows with npm).

**Effort:** ~40 lines.

---

### 7. Dotfile Paths for Claude Code / OpenClaude / Codex

**Files:** `src/daemon/clients/claude_code.rs`, `openclaude.rs`, `codex.rs`

**Problem:** These clients use `dirs::home_dir()` for config/skills paths with no Windows `#[cfg]` branch. The concern was that Windows tools might use `%APPDATA%` instead.

**Analysis after checking actual tool behavior:**

| Client | Config path | Skills path | Uses `%APPDATA%`? |
|--------|------------|-------------|-------------------|
| Claude Code | `~/.claude.json` | `~/.claude/skills/` | No — dotfile in home |
| OpenClaude | `~/.openclaude.json` | `~/.openclaude/skills/` | No — dotfile in home |
| Codex | `~/.codex/config.toml` | `~/.codex/skills/` | No — dotfile in home, has `$CODEX_HOME` override |

**Finding:** These tools follow the Unix dotfile convention on Windows too (like `.gitconfig`, `.ssh/`). `dirs::home_dir()` returns `C:\Users\<user>` on Windows, which is correct. No change needed.

**Contrast with Cursor/VS Code:** Those IDEs use `%APPDATA%` because they're Electron-based and follow the Windows app convention. CLI tools (Claude Code, Codex) use dotfiles.

**Action:** No change needed. Document the convention difference.

---

### 8. Cursor `skills_dir()` — SKIP

**File:** `src/daemon/clients/cursor.rs:30-33`

**Finding:** [Cursor skills are project-scoped](https://www.agensi.io/learn/where-are-cursor-skills-stored), stored at `.cursor/skills/` within each project repo. There is no global user-level skills directory.

The current `skills_dir()` returning `~/.cursor/skills` is consistent with this convention. The `config_path()` using `%APPDATA%\Cursor\mcp.json` is correct for the MCP config (Electron app convention), but skills are fundamentally different — they live in the project, not the user profile.

**Action:** No change needed.

---

## TRIVIAL

### 9. Remove Unused Import

**File:** `src/daemon/upstream/stdio.rs:36`

```rust
use std::os::windows::process::CommandExt;  // <-- unused
```

Tokio's `Command` re-exports `creation_flags` via `Deref`, so the explicit import is unnecessary. The code at line 38 already calls `cmd.creation_flags(...)` without needing this import.

**Action:** Delete the line.

**Effort:** 1 line.

---

### 10. Unix Process Group (setsid)

**File:** `src/daemon/upstream/stdio.rs:3`

**Context:** The TODO at line 3 mentions both Unix `setsid` and Windows Job Objects. The Windows side is handled by item #1. The Unix side (`setsid` via `nix` or `process-wrap` crate) is not in this plan but should be tracked separately.

**Why:** On Unix, `kill_on_drop(true)` sends SIGKILL to the direct child but grandchildren (Node/Python workers) can survive if they detach from the process group. `setsid` creates a new session, so killing the session kills all descendants.

**Note:** `process-wrap` (used in item #1) also provides `process_wrap::tokio::UnixSessionLeader` which calls `setsid()` on the child. This could be wired into the same `stdio::connect()` code path with a `#[cfg(unix)]` branch, making items #1 and #10 a single change.

**Action:** Out of scope for this Windows-focused plan. Create a separate issue — but consider implementing alongside item #1 since `process-wrap` provides both.

---

## Priority Order

| # | Task | Severity | Effort | Status |
|---|------|----------|--------|--------|
| 1 | Job Object via process-wrap (delete job_object.rs) | HIGH | ~10 lines + delete | Ready |
| 2 | DB file permissions via icacls | HIGH (security) | ~20 lines | Ready |
| 3 | Atomic rename with ReplaceFileW | MEDIUM | ~25 lines | Ready |
| 4 | Notification buttons on Windows | MEDIUM (UX) | 0 (defer) | Defer |
| 5 | Notification close timeout | MEDIUM (UX) | ~2 lines | Ready |
| 6 | Windows IDE detection | LOW | ~40 lines | Ready |
| 7 | Dotfile paths | LOW | 0 | No change |
| 8 | Cursor skills_dir | LOW | 0 | No change |
| 9 | Remove unused import | TRIVIAL | 1 line | Ready |
| 10 | Unix setsid | — | — | Separate issue |

---

## Testing Strategy

| Item | Verification |
|------|-------------|
| #1 (Job Object) | Manual: spawn MCP server → kill Conduit → verify no orphaned Node/Python processes in Task Manager. Also `cargo clippy` to confirm no new warnings. |
| #2 (DB perms) | Run `cargo run` (release), then `icacls state.db` — verify only SYSTEM, Administrators, and current user have access. Test on non-NTFS volume (USB drive) to confirm warning-not-crash. |
| #3 (Atomic rename) | Unit test: create temp file, open with `FILE_SHARE_READ`, call `atomic_rename_or_replace`, verify success. Manual: open Cursor/VS Code in a project, run Conduit, trigger config write, verify no `ERROR_SHARING_VIOLATION` in logs. |
| #5 (Notif timeout) | Manual: trigger approval, verify notification auto-dismisses after ~10s. |
| #6 (IDE detection) | Add unit test for `WinApp` variant in `tests/ide_detect.rs`. Manual: uninstall IDE from PATH, verify detection via well-known path. |
| #9 (Unused import) | `cargo check` + `cargo clippy` — sufficient for a dead import removal. |

---

## Cargo.toml Changes Required

**`windows-sys` is already a direct dependency** (version `0.59`, line 110). Add the `Win32_Storage_FileSystem` feature to the existing entry:

```toml
[target.'cfg(windows)'.dependencies]
# Job Object (via process-wrap) — replaces manual job_object.rs
process-wrap = { version = "9", features = ["tokio1"] }

# Add Win32_Storage_FileSystem to existing windows-sys entry:
windows-sys = { version = "0.59", features = [
    "Win32_Foundation",
    "Win32_Storage_FileSystem",
] }
```

Note: `Win32_System_JobObjects` and `Win32_System_Threading` features are no longer needed (job_object.rs is being deleted). `process-wrap` handles Job Objects internally via the `windows` crate. After removing the old job_object.rs code, clean up any now-unused features from the `windows-sys` entry.

---

## What Already Works on Windows

- IPC via named pipes (`interprocess` crate)
- Child process spawning with `CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP`
- Autostart via Task Scheduler (`schtasks` CLI)
- All IDE config paths via `dirs::config_dir()` → `%APPDATA%`
- IDE detection scanning `.exe` suffix on PATH
- Node `.cmd` and Python `.exe` binary paths
- System tray icon (`tray-icon` crate)
- Hotkey registration (`global-hotkey` crate)
- Notifications (fire-and-forget via `notify-rust`)
- File manager reveal (`explorer /select,<path>`)
- Keyring via Windows Credential Manager
- Symlink fallback to copy (non-admin users)
- Job Object via `process-wrap` (transitive dep of `rmcp`, not yet wired)
- Dotfile paths for CLI tools (Claude Code, OpenClaude, Codex)

---

## References

- [`process-wrap` crate](https://docs.rs/process-wrap) — production-quality Job Object wrapper, already in `Cargo.lock` via `rmcp`
- [`process-wrap` Job Object source](https://github.com/nicoretti/process-wrap) — `CREATE_SUSPENDED` + per-spawn job + `KILL_ON_JOB_CLOSE`
- [Cargo Job Object implementation](https://doc.rust-lang.org/beta/nightly-rustc/src/cargo/util/job.rs.html) — manual `windows-sys` pattern (reference only, not recommended)
- [Microsoft Job Objects docs](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects)
- [ReplaceFileW API](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-replacefilew) — atomic replacement
- [Moving and Replacing Files (Microsoft)](https://learn.microsoft.com/en-us/windows/win32/fileio/moving-and-replacing-files)
- [windows-sys vs windows crate](https://kennykerr.ca/rust-getting-started/windows-or-windows-sys.html) — `windows-sys` is correct for C-style APIs
- [Cursor skills location](https://www.agensi.io/learn/where-are-cursor-skills-stored) — project-scoped, no global dir
- [icacls reference](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/icacls)
- [Rust fs::rename on Windows](https://doc.rust-lang.org/std/fs/fn.rename.html) — calls `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`
- [Rust issue #123985](https://github.com/rust-lang/rust/issues/123985) — `fs::rename` sometimes fails on Windows
