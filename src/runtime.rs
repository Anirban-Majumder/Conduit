use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use tokio::runtime::{Builder, Handle};
use tokio_util::sync::CancellationToken;

use crate::daemon::errors::RuntimeError;

const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub struct Worker {
    handle: Handle,
    cancel: CancellationToken,
    join: Option<JoinHandle<()>>,
}

impl Worker {
    pub fn spawn(cancel: CancellationToken) -> Result<Self, RuntimeError> {
        let (handle_tx, handle_rx) = mpsc::sync_channel::<Handle>(1);
        let cancel_for_thread = cancel.clone();

        let join = thread::Builder::new()
            .name("mcp-router-tokio".into())
            .spawn(move || run_runtime(cancel_for_thread, handle_tx))?;

        let handle = handle_rx
            .recv()
            .map_err(|_disconnected| RuntimeError::WorkerDied)?;
        Ok(Self {
            handle,
            cancel,
            join: Some(join),
        })
    }

    pub const fn handle(&self) -> &Handle {
        &self.handle
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(join) = self.join.take() {
            // The runtime thread completes block_on once cancel fires, then
            // calls shutdown_timeout itself. Joining here just waits for the
            // OS thread to finish unwinding — quick under normal conditions.
            join.join().ok();
        }
    }
}

fn run_runtime(cancel: CancellationToken, handle_tx: mpsc::SyncSender<Handle>) {
    let rt = match Builder::new_multi_thread()
        .enable_all()
        .thread_name("mcp-router-async")
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!(error = %e, "tokio runtime build failed");
            return;
        }
    };

    if handle_tx.send(rt.handle().clone()).is_err() {
        tracing::error!("runtime owner dropped before receiving handle");
        return;
    }

    rt.block_on(async {
        cancel.cancelled().await;
    });

    rt.shutdown_timeout(SHUTDOWN_GRACE);
}
