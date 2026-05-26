use std::path::PathBuf;

use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::daemon::db::types::{ApprovalStatus, ManagedIdeConfig};
use crate::daemon::db::Db;
use crate::daemon::errors::DbError;
use crate::daemon::events::{Bus, Event};
use crate::daemon::ide_writer::{self, IdeWriterError};

const APPLY_PARALLELISM: usize = 4;

#[derive(Debug, Clone)]
pub struct TargetIde {
    pub ide_name: String,
    pub ide_key: String,
    pub config_path: PathBuf,
    pub remaining: Vec<String>,
}

#[derive(Debug, Default)]
pub struct CascadeReport {
    pub ides_trimmed: Vec<TargetIde>,
    pub ides_skipped: Vec<(TargetIde, String)>,
    pub cancelled_approvals: Vec<String>,
}

pub async fn delete_server_with_cascade(
    db: &Db,
    bus: &Bus,
    server_name: &str,
    port: u16,
    delete_audit_history: bool,
) -> Result<CascadeReport, DbError> {
    let plan = plan_targets(db, server_name).await?;

    let mut joinset = JoinSet::new();
    let sem = std::sync::Arc::new(Semaphore::new(APPLY_PARALLELISM));
    for target in &plan {
        let permit = match sem.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_closed) => break,
        };
        let path = target.config_path.clone();
        let ide_key = target.ide_key.clone();
        let server = server_name.to_string();
        let target_clone = target.clone();
        joinset.spawn_blocking(move || {
            let _permit = permit;
            let result = ide_writer::purge_one_server_for_ide(&path, port, &ide_key, &server);
            (target_clone, result)
        });
    }

    let mut report = CascadeReport::default();
    while let Some(joined) = joinset.join_next().await {
        match joined {
            Ok((target, Ok(()))) => report.ides_trimmed.push(target),
            Ok((target, Err(e))) => {
                let reason = describe(&e);
                tracing::warn!(
                    ide = %target.ide_name,
                    key = %target.ide_key,
                    error = %reason,
                    "cascade: skipping IDE",
                );
                report.ides_skipped.push((target, reason));
            }
            Err(e) => tracing::warn!(error = %e, "cascade apply join failed"),
        }
    }

    let trimmed_for_commit: Vec<(String, String, Vec<String>)> = report
        .ides_trimmed
        .iter()
        .map(|t| (t.ide_name.clone(), t.ide_key.clone(), t.remaining.clone()))
        .collect();
    let cancelled = db
        .commit_cascade_delete(server_name, trimmed_for_commit, delete_audit_history)
        .await?;
    for id in &cancelled {
        bus.publish(Event::ApprovalResolved {
            id: id.clone(),
            status: ApprovalStatus::Cancelled,
        });
    }
    report.cancelled_approvals = cancelled;
    Ok(report)
}

async fn plan_targets(db: &Db, server_name: &str) -> Result<Vec<TargetIde>, DbError> {
    let configs: Vec<ManagedIdeConfig> = db.list_all_ide_configs().await?;
    let plan = configs
        .into_iter()
        .filter(|c| c.managed_entries.iter().any(|n| n == server_name))
        .map(|c| {
            let remaining: Vec<String> = c
                .managed_entries
                .iter()
                .filter(|n| n.as_str() != server_name)
                .cloned()
                .collect();
            TargetIde {
                ide_name: c.ide_name,
                ide_key: c.ide_key,
                config_path: c.config_path,
                remaining,
            }
        })
        .collect();
    Ok(plan)
}

fn describe(err: &IdeWriterError) -> String {
    match err {
        IdeWriterError::Io(e) if e.kind() == std::io::ErrorKind::NotFound => {
            "config file no longer exists".to_string()
        }
        IdeWriterError::Io(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            "permission denied".to_string()
        }
        other => other.to_string(),
    }
}

