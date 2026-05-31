//! Tool-call approval gate: holds an HTTP request open until the user resolves
//! it from the GUI/tray or the timeout fires.
//!
//! `ApprovalCenter::request` persists a `PendingApproval`, registers an in-memory
//! `oneshot` waiter, publishes `Event::ApprovalPending`, then `select!`s the
//! waiter against the timeout. `resolve` (driven by GUI/IPC) flips the DB row and
//! wakes the waiter. A background sweeper times out approvals orphaned by a daemon
//! restart. Invariant: the DB row is the source of truth; the waiter map is a
//! best-effort fast path and may be empty (e.g. after restart).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use thiserror::Error;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::daemon::db::types::{ApprovalStatus, PendingApproval};
use crate::daemon::db::Db;
use crate::daemon::errors::DbError;
use crate::daemon::events::{Bus, Event};
use crate::daemon::time_util::now_unix_secs;

const SWEEPER_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Error)]
pub enum ApprovalError {
    #[error(transparent)]
    Db(#[from] DbError),

    #[error("cannot resolve to ApprovalStatus::Pending")]
    InvalidStatus,
}

#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    pub server: String,
    pub tool: String,
    pub args_json: String,
    pub ide_key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalOutcome {
    Approved,
    Denied,
    Timeout,
}

type WaiterMap = Arc<Mutex<HashMap<String, oneshot::Sender<ApprovalStatus>>>>;

#[derive(Debug)]
pub struct ApprovalCenter {
    db: Db,
    bus: Bus,
    timeout: Duration,
    waiters: WaiterMap,
}


impl ApprovalCenter {
    pub fn new(db: Db, bus: Bus, timeout: Duration) -> Self {
        Self {
            db,
            bus,
            timeout,
            waiters: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub const fn db(&self) -> &Db {
        &self.db
    }

    pub const fn bus(&self) -> &Bus {
        &self.bus
    }

    pub async fn request(&self, req: ApprovalRequest) -> Result<ApprovalOutcome, ApprovalError> {
        let id = Uuid::new_v4().to_string();
        let approval = PendingApproval {
            id: id.clone(),
            server: req.server.clone(),
            tool: req.tool.clone(),
            args_json: req.args_json,
            ide_key: req.ide_key.clone(),
            created_at: now_unix_secs(),
            status: ApprovalStatus::Pending,
        };

        self.db.create_pending_approval(approval).await?;

        let (tx, rx) = oneshot::channel::<ApprovalStatus>();
        // Lock scope is just the dictionary insert — guard never crosses .await.
        match self.waiters.lock() {
            Ok(mut guard) => {
                guard.insert(id.clone(), tx);
            }
            Err(poisoned) => {
                // Recover from a panicked holder rather than propagating poison
                // up to the HTTP handler — a single bad approval shouldn't kill
                // every subsequent one.
                let mut guard = poisoned.into_inner();
                guard.insert(id.clone(), tx);
            }
        }

        self.bus.publish(Event::ApprovalPending {
            id: id.clone(),
            server: req.server,
            tool: req.tool,
            ide_key: req.ide_key,
        });

        let outcome = tokio::select! {
            biased;
            received = rx => match received {
                Ok(ApprovalStatus::Approved) => ApprovalOutcome::Approved,
                Ok(ApprovalStatus::Denied) => ApprovalOutcome::Denied,
                // resolve() never sends Pending (InvalidStatus blocks it) and
                // Timeout is owned by this branch; either means we treat it as
                // a timeout so the handler returns -32001 rather than hanging.
                Ok(ApprovalStatus::Pending | ApprovalStatus::Timeout | ApprovalStatus::Cancelled) => {
                    ApprovalOutcome::Timeout
                }
                Err(_dropped) => ApprovalOutcome::Timeout,
            },
            () = tokio::time::sleep(self.timeout) => {
                self.remove_waiter(&id);
                if let Err(e) = self
                    .db
                    .resolve_pending_approval(&id, ApprovalStatus::Timeout)
                    .await
                {
                    tracing::warn!(error = %e, id = %id, "flipping approval to timeout failed");
                }
                self.bus.publish(Event::ApprovalResolved {
                    id: id.clone(),
                    status: ApprovalStatus::Timeout,
                });
                ApprovalOutcome::Timeout
            }
        };

        Ok(outcome)
    }

    pub async fn resolve(&self, id: &str, status: ApprovalStatus) -> Result<(), ApprovalError> {
        match status {
            ApprovalStatus::Approved
            | ApprovalStatus::Denied
            | ApprovalStatus::Timeout
            | ApprovalStatus::Cancelled => {}
            ApprovalStatus::Pending => return Err(ApprovalError::InvalidStatus),
        }

        let current = self.db.get_pending_approval(id).await?;
        let Some(row) = current else {
            return Ok(());
        };
        match row.status {
            ApprovalStatus::Pending => {}
            ApprovalStatus::Approved
            | ApprovalStatus::Denied
            | ApprovalStatus::Timeout
            | ApprovalStatus::Cancelled => {
                return Ok(());
            }
        }

        self.db.resolve_pending_approval(id, status).await?;

        let waiter = self.take_waiter(id);
        if let Some(tx) = waiter {
            // send returns Err if the requester already dropped its rx (e.g.
            // the HTTP handler was cancelled). Nothing to do — DB row is
            // already updated so the audit trail is consistent.
            tx.send(status).ok();
        }

        self.bus.publish(Event::ApprovalResolved {
            id: id.to_string(),
            status,
        });

        Ok(())
    }

    fn remove_waiter(&self, id: &str) {
        let mut guard = match self.waiters.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.remove(id);
    }

    fn take_waiter(&self, id: &str) -> Option<oneshot::Sender<ApprovalStatus>> {
        let mut guard = match self.waiters.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.remove(id)
    }
}

/// Sweeps stale approvals and publishes `Event::ApprovalResolved { status: Timeout }`
/// for every swept id. Returns the count of swept rows.
pub async fn sweep_and_publish_timeouts(db: &Db, bus: &Bus, cutoff: i64) -> Result<usize, DbError> {
    let ids = db.sweep_stale_pending_approvals(cutoff).await?;
    let n = ids.len();
    for id in ids {
        bus.publish(Event::ApprovalResolved {
            id,
            status: ApprovalStatus::Timeout,
        });
    }
    Ok(n)
}

pub fn spawn_timeout_sweeper(
    db: Db,
    bus: Bus,
    timeout: Duration,
    cancel: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(SWEEPER_INTERVAL);
        // Skip the immediate first tick so the sweeper doesn't fire the moment
        // the daemon starts — there's nothing stale yet at that point.
        ticker.tick().await;
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                _ = ticker.tick() => {
                    let cutoff = now_unix_secs().saturating_sub(
                        i64::try_from(timeout.as_secs()).unwrap_or(i64::MAX),
                    );
                    match sweep_and_publish_timeouts(&db, &bus, cutoff).await {
                        Ok(0) => {}
                        Ok(n) => tracing::info!(swept = n, "swept stale pending approvals"),
                        Err(e) => tracing::warn!(error = %e, "sweeping stale approvals failed"),
                    }
                }
            }
        }
    })
}
