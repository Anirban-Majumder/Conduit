//! Stdio transport for MCP upstream servers.
//!
//! Spawns a child process and connects via stdin/stdout pipes.
//! On Windows, `CreationFlags` + `JobObject` ensure grandchild cleanup.
//! On Unix, `ProcessGroup::leader()` makes the child a process group leader so
//! that SIGKILL on reap reaches grandchildren (Node/Python workers).

use std::process::Stdio;

use rmcp::service::{serve_client_with_ct, RoleClient, RunningService};
use rmcp::transport::TokioChildProcess;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::daemon::upstream::manager::UpstreamError;

pub type StdioClient = RunningService<RoleClient, ()>;

pub async fn connect(
    command: &str,
    args: &[String],
    env: &[(String, String)],
    cancel: CancellationToken,
) -> Result<StdioClient, UpstreamError> {
    let mut cmd = Command::new(command);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // rmcp's TokioChildProcess overrides stderr to `inherit`;
        // this piped() is a no-op but matches the intent for symmetry.
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (k, v) in env {
        cmd.env(k, v);
    }

    #[cfg(windows)]
    let transport = {
        use process_wrap::tokio::{CommandWrap, CreationFlags, JobObject};
        use windows::Win32::System::Threading::PROCESS_CREATION_FLAGS;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        let mut wrapped = CommandWrap::from(cmd);
        // CreationFlags MUST be wrapped before JobObject — JobObject
        // ORs its CREATE_SUSPENDED into whatever CreationFlags stored.
        wrapped.wrap(CreationFlags(PROCESS_CREATION_FLAGS(
            CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP,
        )));
        wrapped.wrap(JobObject);
        TokioChildProcess::new(wrapped)?
    };
    #[cfg(not(windows))]
    let transport = {
        use process_wrap::tokio::{CommandWrap, ProcessGroup};
        let mut wrapped = CommandWrap::from(cmd);
        wrapped.wrap(ProcessGroup::leader());
        TokioChildProcess::new(wrapped)?
    };

    let client = serve_client_with_ct((), transport, cancel)
        .await
        .map_err(|e| UpstreamError::InitFailed(e.to_string()))?;
    Ok(client)
}
