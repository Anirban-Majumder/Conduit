// Windows Job Object for grandchild process cleanup.
//
// When an MCP server spawns Node/Python workers, `kill_on_drop(true)` only
// kills the direct child. Grandchildren (worker threads, subprocesses) can
// survive and leak. A Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`
// ensures all descendants are terminated when the job handle is dropped.

#[cfg(windows)]
use std::sync::OnceLock;

#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
#[cfg(windows)]
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::AssignProcessToJobObject;

/// Global Job Object handle, created once on first use. Dropping the process
/// closes the handle, which kills all assigned processes.
#[cfg(windows)]
static JOB_HANDLE: OnceLock<HANDLE> = OnceLock::new();

/// Create the global Job Object with `KILL_ON_JOB_CLOSE`. Returns the handle.
/// Safe to call multiple times (idempotent via `OnceLock`).
#[cfg(windows)]
fn ensure_job_handle() -> Option<HANDLE> {
    let handle = JOB_HANDLE.get_or_init(|| {
        let h = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if h == 0 || h == INVALID_HANDLE_VALUE {
            return 0 as HANDLE;
        }

        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let size = std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;

        let ok = unsafe {
            SetInformationJobObject(
                h,
                JobObjectExtendedLimitInformation,
                std::ptr::addr_of!(limits).cast(),
                size,
            )
        };

        if ok == 0 {
            unsafe { CloseHandle(h) };
            return 0 as HANDLE;
        }

        h
    });

    if *handle == 0 {
        None
    } else {
        Some(*handle)
    }
}

/// Assign a process to the global Job Object. The `process_handle` must have
/// `PROCESS_SET_QUOTA | PROCESS_TERMINATE` access rights.
///
/// Returns `Ok(())` on success. Returns `Err` if the job object couldn't be
/// created or the assignment failed. In that case, `kill_on_drop(true)` is
/// still the fallback (but grandchildren may leak).
#[cfg(windows)]
pub fn assign_to_job(process_handle: HANDLE) -> Result<(), std::io::Error> {
    let job = ensure_job_handle().ok_or_else(|| {
        std::io::Error::other("failed to create Windows Job Object")
    })?;

    let ok = unsafe { AssignProcessToJobObject(job, process_handle) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }

    Ok(())
}

/// Assign a `tokio::process::Child` to the global Job Object.
///
/// This is the primary entry point. On non-Windows, this is a no-op.
pub fn assign_child_to_job(_child: &tokio::process::Child) -> Result<(), std::io::Error> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        let raw = _child.as_raw_handle();
        // SAFETY: `raw` is a valid HANDLE from a live Child. The handle
        // remains valid for the child's lifetime; we don't close it here.
        assign_to_job(raw as HANDLE)
    }
    #[cfg(not(windows))]
    {
        Ok(())
    }
}
