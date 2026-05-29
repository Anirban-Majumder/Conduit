//! Skills subsystem: discover, install, and sync SKILL.md packages.
//!
//! Discovers SKILL.md packages on GitHub, stashes a canonical copy under
//! `<config>/conduit/skills/managed/<name>/`, and mirrors that copy into each
//! agentic IDE's skills dir (`~/.claude/skills`, etc.).
//!
//! `SkillManager` is the coordinator: it owns the HTTP client and discover
//! cache and orchestrates install/uninstall/set-targets, persisting state via
//! `Db` and emitting `Event::SkillsChanged`. Submodules split the work:
//! `discover` (GitHub IO), `sync` (mirror into IDE dirs), `repos` (source-repo
//! management and the cached discover query). Leaf path/target/time helpers
//! live in `util`.

pub mod discover;
mod repos;
pub mod sync;
mod util;

use util::{
    first_existing_skill_md, last_segment, locate_skill_md, normalize_targets, now_unix,
    now_unix_nanos, relative_to_source, sanitize_relative, sanitize_segment,
};

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::daemon::clients::{self, SKILL_TARGET_IDES};
use crate::daemon::db::types::Skill;
use crate::daemon::db::Db;
use crate::daemon::errors::SkillError;
use crate::daemon::events::{Bus, Event};
use crate::paths::Paths;

const FETCH_TIMEOUT: Duration = Duration::from_secs(20);
const USER_AGENT: &str = "conduit-skills";
const DOWNLOAD_FILES_PER_SKILL_LIMIT: usize = 200;
const FILE_FETCH_CONCURRENCY: usize = 6;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillCandidate {
    pub key: String,
    pub name: String,
    pub description: String,
    pub directory: String,
    pub readme_url: String,
    pub repo_owner: String,
    pub repo_name: String,
    pub repo_branch: String,
}

#[derive(Debug)]
pub struct SkillManager {
    db: Db,
    bus: Bus,
    paths: Paths,
    http: reqwest::Client,
    discover_cache: Arc<Mutex<Option<DiscoverCache>>>,
}

#[derive(Debug, Clone)]
struct DiscoverCache {
    fingerprint: String,
    generated_at: SystemTime,
    skills: Vec<SkillCandidate>,
}

impl SkillManager {
    pub fn new(paths: Paths, db: Db, bus: Bus) -> Result<Self, SkillError> {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(FETCH_TIMEOUT)
            .build()?;
        Ok(Self {
            db,
            bus,
            paths,
            http,
            discover_cache: Arc::new(Mutex::new(None)),
        })
    }

    pub const fn paths(&self) -> &Paths {
        &self.paths
    }

    pub async fn list_installed(&self) -> Result<Vec<Skill>, SkillError> {
        Ok(self.db.list_skills().await?)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "single install orchestrates fetch + write + sync"
    )]
    pub async fn install(
        &self,
        candidate: SkillCandidate,
        targets: Vec<String>,
    ) -> Result<Skill, SkillError> {
        let install_name = sanitize_segment(last_segment(&candidate.directory))
            .ok_or_else(|| SkillError::InvalidPath(candidate.directory.clone()))?;

        if let Some(existing) = self.db.get_skill(&install_name).await? {
            if existing
                .repo_owner
                .eq_ignore_ascii_case(&candidate.repo_owner)
                && existing
                    .repo_name
                    .eq_ignore_ascii_case(&candidate.repo_name)
            {
                // Same slug, same repo — re-install (refresh) is fine.
            } else {
                return Err(SkillError::DirectoryConflict {
                    name: install_name,
                    owner: existing.repo_owner,
                    repo: existing.repo_name,
                });
            }
        }

        let tree = discover::resolve_repo_tree(
            &self.http,
            &candidate.repo_owner,
            &candidate.repo_name,
            &candidate.repo_branch,
        )
        .await?;
        let source_dir = sanitize_relative(&candidate.directory)
            .ok_or_else(|| SkillError::InvalidPath(candidate.directory.clone()))?;
        let files = tree
            .entries
            .into_iter()
            .filter(|e| {
                e.kind == discover::EntryKind::Blob
                    && (e.path == source_dir || e.path.starts_with(&format!("{source_dir}/")))
            })
            .take(DOWNLOAD_FILES_PER_SKILL_LIMIT)
            .collect::<Vec<_>>();

        if !files.iter().any(|e| {
            let p = e.path.as_str();
            p.eq_ignore_ascii_case(&format!("{source_dir}/SKILL.md"))
                || p.eq_ignore_ascii_case(&format!("{source_dir}/Skill.md"))
                || p.eq_ignore_ascii_case(&format!("{source_dir}/skill.md"))
        }) {
            return Err(SkillError::MissingSkillFile {
                repo: format!("{}/{}", candidate.repo_owner, candidate.repo_name),
                dir: source_dir,
            });
        }

        let dest = self.paths.skills_managed_dir().join(&install_name);
        let tmp = self
            .paths
            .skills_root()
            .join("tmp")
            .join(format!("{install_name}-{}", now_unix_nanos()));
        let _ignored = tokio::fs::remove_dir_all(&tmp).await;
        tokio::fs::create_dir_all(&tmp).await?;

        let install_result: Result<(), SkillError> = async {
            // Bounded-concurrency fan-out: at most FILE_FETCH_CONCURRENCY raw
            // requests in flight per install. Cuts elapsed time on multi-file
            // skills from O(n) RTT to O(n/N) RTT.
            let sem = Arc::new(tokio::sync::Semaphore::new(FILE_FETCH_CONCURRENCY));
            let mut joinset: tokio::task::JoinSet<Result<(), SkillError>> =
                tokio::task::JoinSet::new();
            for entry in files {
                let relative = relative_to_source(&entry.path, &source_dir);
                let safe = sanitize_relative(&relative)
                    .ok_or_else(|| SkillError::InvalidPath(relative.clone()))?;
                let out = tmp.join(&safe);
                let http = self.http.clone();
                let owner = candidate.repo_owner.clone();
                let name = candidate.repo_name.clone();
                let branch = tree.branch.clone();
                let path = entry.path.clone();
                let sem = sem.clone();
                joinset.spawn(async move {
                    let _permit = sem
                        .acquire_owned()
                        .await
                        .map_err(|_closed| SkillError::InvalidPath("semaphore closed".into()))?;
                    if let Some(parent) = out.parent() {
                        tokio::fs::create_dir_all(parent).await?;
                    }
                    let body = discover::fetch_raw(&http, &owner, &name, &branch, &path).await?;
                    tokio::fs::write(&out, body).await?;
                    Ok(())
                });
            }
            while let Some(res) = joinset.join_next().await {
                match res {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        joinset.shutdown().await;
                        return Err(e);
                    }
                    Err(e) => {
                        return Err(SkillError::InvalidPath(format!("join: {e}")));
                    }
                }
            }
            Ok(())
        }
        .await;

        if let Err(e) = install_result {
            let _ignored = tokio::fs::remove_dir_all(&tmp).await;
            return Err(e);
        }

        let _ignored = tokio::fs::remove_dir_all(&dest).await;
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::rename(&tmp, &dest).await?;

        let skill_md = locate_skill_md(&dest).await?;
        let bytes = tokio::fs::read(&skill_md).await?;
        let metadata =
            discover::read_skill_metadata(std::str::from_utf8(&bytes).unwrap_or(""), &install_name);

        let normalized_targets = normalize_targets(targets);
        let skill = Skill {
            directory: install_name.clone(),
            repo_owner: candidate.repo_owner,
            repo_name: candidate.repo_name,
            repo_branch: tree.branch.clone(),
            source_directory: source_dir,
            skill_name: if metadata.name.is_empty() {
                install_name.clone()
            } else {
                metadata.name
            },
            description: metadata.description,
            readme_url: Some(candidate.readme_url),
            installed_at: now_unix(),
            targets: normalized_targets.clone(),
        };
        self.db.upsert_skill_with_targets(skill.clone()).await?;

        // First push every requested target; then prune anything we previously
        // had but were just told to remove. sync_to_target is std::fs heavy
        // (recursive copy fallback) — wrap in spawn_blocking to keep the tokio
        // runtime responsive on slow disks.
        apply_targets_blocking(&dest, &install_name, &normalized_targets).await?;

        self.bus.publish(Event::SkillsChanged);
        Ok(skill)
    }

    pub async fn set_targets(
        &self,
        directory: &str,
        targets: Vec<String>,
    ) -> Result<(), SkillError> {
        let normalized = normalize_targets(targets);
        let dest = self.paths.skills_managed_dir().join(directory);
        if !tokio::fs::try_exists(&dest).await.unwrap_or(false) {
            return Err(SkillError::InvalidPath(directory.to_string()));
        }
        apply_targets_blocking(&dest, directory, &normalized).await?;
        self.db.set_skill_targets(directory, normalized).await?;
        self.bus.publish(Event::SkillsChanged);
        Ok(())
    }

    pub async fn uninstall(&self, directory: &str) -> Result<(), SkillError> {
        let directory_owned = directory.to_string();
        tokio::task::spawn_blocking(move || {
            for ide in SKILL_TARGET_IDES {
                sync::remove_from_target(&directory_owned, clients::ide_db_name(*ide));
            }
        })
        .await
        .map_err(|e| SkillError::InvalidPath(format!("join: {e}")))?;
        let dest = self.paths.skills_managed_dir().join(directory);
        let _ignored = tokio::fs::remove_dir_all(&dest).await;
        self.db.delete_skill(directory).await?;
        self.bus.publish(Event::SkillsChanged);
        Ok(())
    }

    /// Idempotently drops `<skills_root>/tmp` — half-finished installs that
    /// outlived a previous process. Cheap to call at startup.
    pub async fn purge_tmp(&self) {
        let tmp = self.paths.skills_root().join("tmp");
        let _ignored = tokio::fs::remove_dir_all(&tmp).await;
    }

    /// Register any subdirectory of `skills/managed/` that has a SKILL.md but
    /// isn't tracked in the DB yet. Lets the user drop a hand-crafted skill
    /// into the folder; the next Refresh picks it up with zero targets so
    /// they opt in per-IDE manually.
    pub async fn register_local_skills(&self) -> Result<usize, SkillError> {
        let managed = self.paths.skills_managed_dir();
        let known: std::collections::HashSet<String> = self
            .db
            .list_skills()
            .await?
            .into_iter()
            .map(|s| s.directory)
            .collect();
        let mut count: usize = 0;
        let mut reader = match tokio::fs::read_dir(&managed).await {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(SkillError::Io(e)),
        };
        while let Some(entry) = reader.next_entry().await? {
            let ft = entry.file_type().await?;
            if !ft.is_dir() && !ft.is_symlink() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if name.starts_with('.') || known.contains(&name) {
                continue;
            }
            let Some(skill_md) = first_existing_skill_md(&entry.path()).await else {
                continue;
            };
            let bytes = tokio::fs::read(&skill_md).await?;
            let meta =
                discover::read_skill_metadata(std::str::from_utf8(&bytes).unwrap_or(""), &name);
            let now = now_unix();
            let skill = Skill {
                directory: name.clone(),
                repo_owner: "local".to_string(),
                repo_name: "local".to_string(),
                repo_branch: "local".to_string(),
                source_directory: name.clone(),
                skill_name: meta.name,
                description: meta.description,
                readme_url: None,
                installed_at: now,
                targets: Vec::new(),
            };
            if let Err(e) = self.db.upsert_skill_with_targets(skill).await {
                tracing::warn!(error = %e, skill = %name, "register_local_skills failed");
                continue;
            }
            count += 1;
        }
        if count > 0 {
            self.bus.publish(Event::SkillsChanged);
        }
        Ok(count)
    }
}

async fn apply_targets_blocking(
    dest: &std::path::Path,
    install_name: &str,
    targets: &[String],
) -> Result<(), SkillError> {
    let dest = dest.to_path_buf();
    let install_name = install_name.to_string();
    let targets: Vec<String> = targets.to_vec();
    tokio::task::spawn_blocking(move || -> Result<(), SkillError> {
        for ide in SKILL_TARGET_IDES {
            let key = clients::ide_db_name(*ide);
            if targets.iter().any(|t| t == key) {
                sync::sync_to_target(&dest, key)?;
            } else {
                sync::remove_from_target(&install_name, key);
            }
        }
        Ok(())
    })
    .await
    .map_err(|e| SkillError::InvalidPath(format!("join: {e}")))?
}
