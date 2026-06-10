//! Audit-log retention: startup + daily purge of rows older than configured
//! `audit_retention_days`. Cancel-aware; `retention_days == 0` keeps forever.
//!
//! Spawned once from `main.rs` under the tokio worker guard. The daily tick
//! reads the shared settings each time so retention changes apply without a
//! restart.

use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::daemon::config::SharedSettings;
use crate::daemon::db::Db;
use crate::daemon::time_util::now_unix_secs;

pub fn spawn(db: Db, settings: SharedSettings, cancel: CancellationToken) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(86_400));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                _ = interval.tick() => {
                    let days = settings.load().daemon.audit_retention_days;
                    if days == 0 {
                        tracing::debug!("audit_retention_days=0; keeping all entries");
                        continue;
                    }
                    let cutoff = now_unix_secs().saturating_sub(i64::from(days) * 86_400);
                    match db.purge_old_audit_entries(cutoff).await {
                        Ok(n) if n > 0 => tracing::info!(count = n, days, "purged old audit entries"),
                        Ok(_) => {}
                        Err(e) => tracing::warn!(error = %e, "purge_old_audit_entries failed"),
                    }
                }
            }
        }
    })
}
