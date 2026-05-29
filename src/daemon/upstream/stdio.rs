//! rmcp child-process transport for stdio MCP servers.
//!
//! Per spec §"Add MCP server flow". `connect` spawns the upstream binary and
//! hands rmcp the piped stdio; the returned client is owned by
//! `UpstreamManager`.
//!
//! TODO(v0.5): wrap the child in a process group (Unix `setsid`) / Windows Job
//! Object so Node/Python workers don't leak. `kill_on_drop(true)` only kills
//! the direct child (CLAUDE.md gotcha).

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
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (k, v) in env {
        cmd.env(k, v);
    }

    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    }

    let transport = TokioChildProcess::new(cmd)?;
    let client = serve_client_with_ct((), transport, cancel)
        .await
        .map_err(|e| UpstreamError::InitFailed(e.to_string()))?;
    Ok(client)
}
