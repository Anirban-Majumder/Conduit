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

    #[cfg(windows)]
    let transport = {
        use process_wrap::tokio::{CommandWrap, JobObject};
        let mut wrapped = CommandWrap::from(cmd);
        wrapped.wrap(JobObject);
        TokioChildProcess::new(wrapped)?
    };
    #[cfg(not(windows))]
    let transport = TokioChildProcess::new(cmd)?;

    let client = serve_client_with_ct((), transport, cancel)
        .await
        .map_err(|e| UpstreamError::InitFailed(e.to_string()))?;
    Ok(client)
}
