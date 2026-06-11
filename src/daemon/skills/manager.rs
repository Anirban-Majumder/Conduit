//! `SkillManager` — the skills coordinator: discover → install → mirror.
//!
//! Owns the in-memory discover cache and orchestrates `discover` (GitHub IO)
//! and `sync` (mirror managed skills into each IDE's skills dir). The parent
//! `skills` module holds the module wiring + re-exports.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use super::{discover, sync};

use crate::daemon::client_config::{self, ClientDef, ResolvedClient, Scope, BUILTINS};
use crate::daemon::config::toml::Settings;
use crate::daemon::db::types::{Skill, SkillRepo};
use crate::daemon::db::Db;
use crate::daemon::errors::SkillError;
use crate::daemon::events::{Bus, Event};
use crate::daemon::paths::Paths;
use crate::daemon::time_util::{now_unix_nanos, now_unix_secs};

const DISCOVER_CACHE_TTL: Duration = Duration::from_secs(60 * 60);
/// SK-5: when any repo errors during discovery, use a much shorter TTL so the
/// next refresh retries rather than serving the partial list for a full hour.
const DISCOVER_CACHE_TTL_PARTIAL: Duration = Duration::from_secs(60);
const FETCH_TIMEOUT: Duration = Duration::from_secs(20);
const USER_AGENT: &str = "conduit-skills";
const DOWNLOAD_FILES_PER_SKILL_LIMIT: usize = 200;
const FILE_FETCH_CONCURRENCY: usize = 6;

/// Returns the subset of BUILTINS that support skills.
fn skill_capable_builtins() -> impl Iterator<Item = &'static ClientDef> {
    BUILTINS.iter().copied().filter(|b| b.skills.is_some())
}

/// Sentinel used in `SkillCandidate.directory` for skills whose `SKILL.md` is at
/// the repository root (not inside a subdirectory).
pub const ROOT_SKILL_SENTINEL: &str = "";

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
    /// Ensures at most one network discovery runs at a time. The second caller
    /// awaits the first; once the first finishes the cache is fresh, so the
    /// second hits the cache immediately after acquiring the lock.
    discover_lock: Arc<Mutex<()>>,
}

#[derive(Debug, Clone)]
struct DiscoverCache {
    fingerprint: String,
    generated_at: SystemTime,
    skills: Vec<SkillCandidate>,
    /// SK-5: full TTL normally; short TTL when discovery was partial.
    ttl: Duration,
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
            discover_lock: Arc::new(Mutex::new(())),
        })
    }

    pub const fn paths(&self) -> &Paths {
        &self.paths
    }

    /// IDs of tracked clients that can receive skills. Drives the Skills-tab
    /// per-skill icon toggles; clients without a skills dir are excluded so
    /// we never offer a toggle whose write would silently drop.
    pub async fn active_skill_client_ids(&self) -> Vec<String> {
        let tracked = self.db.list_tracked_clients().await.unwrap_or_default();
        tracked
            .into_iter()
            .filter(|c| client_config::builtin_by_id(&c.id).is_some_and(|def| def.skills.is_some()))
            .map(|c| c.id)
            .collect()
    }

    /// Return the set of available (not installed) skills, fetching from GitHub
    /// when the in-memory cache is stale.
    ///
    /// Concurrent callers are serialised by `discover_lock`: the second caller
    /// waits for the first to finish, then hits the freshly-populated cache
    /// without issuing a second round of network requests.
    pub async fn discover(&self, force: bool) -> Result<Vec<SkillCandidate>, SkillError> {
        let repos = self.db.list_skill_repos().await?;
        let enabled: Vec<SkillRepo> = repos.into_iter().filter(|r| r.enabled).collect();
        let fingerprint = discover::fingerprint(&enabled);

        // Fast path: cache hit before acquiring the network lock.
        if !force {
            let cached = self.discover_cache.lock().await.clone();
            if let Some(cache) = &cached {
                if cache.fingerprint == fingerprint
                    && cache.generated_at.elapsed().unwrap_or(cache.ttl) < cache.ttl
                {
                    return Ok(cache.skills.clone());
                }
            }
        }

        // Serialise network work: hold the lock only while fetching + writing
        // the cache. The lock guard must not be held across any UI call.
        let _lock = self.discover_lock.lock().await;

        // Re-check after acquiring the lock — a concurrent caller may have
        // already populated the cache while we waited.
        if !force {
            let cached = self.discover_cache.lock().await.clone();
            if let Some(cache) = &cached {
                if cache.fingerprint == fingerprint
                    && cache.generated_at.elapsed().unwrap_or(cache.ttl) < cache.ttl
                {
                    return Ok(cache.skills.clone());
                }
            }
        }

        let (skills, any_errored) = discover::discover_skills(&self.http, &enabled).await?;
        // SK-5: use a short TTL when any repo errored so the next refresh retries.
        let ttl = if any_errored {
            DISCOVER_CACHE_TTL_PARTIAL
        } else {
            DISCOVER_CACHE_TTL
        };
        *self.discover_cache.lock().await = Some(DiscoverCache {
            fingerprint,
            generated_at: SystemTime::now(),
            skills: skills.clone(),
            ttl,
        });
        Ok(skills)
    }

    pub async fn list_repos(&self) -> Result<Vec<SkillRepo>, SkillError> {
        Ok(self.db.list_skill_repos().await?)
    }

    pub async fn add_repo(&self, repo: SkillRepo) -> Result<(), SkillError> {
        validate_repo_input(&repo)?;
        self.db.upsert_skill_repo(repo).await?;
        self.invalidate_discover_cache().await;
        self.bus.publish(Event::SkillsChanged);
        Ok(())
    }

    pub async fn remove_repo(&self, owner: &str, name: &str) -> Result<(), SkillError> {
        self.db.delete_skill_repo(owner, name).await?;
        self.invalidate_discover_cache().await;
        self.bus.publish(Event::SkillsChanged);
        Ok(())
    }

    pub async fn set_repo_enabled(
        &self,
        owner: &str,
        name: &str,
        enabled: bool,
    ) -> Result<(), SkillError> {
        self.db.set_skill_repo_enabled(owner, name, enabled).await?;
        // Intentionally NOT invalidating the discover cache: the cache
        // fingerprint already reflects the enabled-set, so the next discover
        // call sees a fingerprint mismatch and refetches naturally.
        self.bus.publish(Event::SkillsChanged);
        Ok(())
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
        // SK-2: root-level skill uses the repo name as install directory.
        let is_root = candidate.directory == ROOT_SKILL_SENTINEL;
        let install_name = if is_root {
            sanitize_segment(&candidate.repo_name)
                .ok_or_else(|| SkillError::InvalidPath(candidate.repo_name.clone()))?
        } else {
            sanitize_segment(last_segment(&candidate.directory))
                .ok_or_else(|| SkillError::InvalidPath(candidate.directory.clone()))?
        };

        // SK-3: case-insensitive conflict check against all installed skills.
        let all_installed = self.db.list_skills().await?;
        for existing in &all_installed {
            if existing.directory.eq_ignore_ascii_case(&install_name) {
                // Same directory (case-insensitively) — allow only if same repo.
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
                        owner: existing.repo_owner.clone(),
                        repo: existing.repo_name.clone(),
                    });
                }
            }
        }

        let tree = discover::resolve_repo_tree(
            &self.http,
            &candidate.repo_owner,
            &candidate.repo_name,
            &candidate.repo_branch,
        )
        .await?;

        // SK-2: root-level skill — all blob entries are included directly,
        // with their tree path used as the relative install path.
        let (files, source_dir) = if is_root {
            // Root install: the SKILL.md at the repo root implies the whole repo
            // is the skill. Still cap downloads — a hostile/huge repo would
            // otherwise pull its entire tree (only in-flight concurrency, not the
            // file count, is bounded).
            let all_blobs = tree
                .entries
                .into_iter()
                .filter(|e| e.kind == discover::EntryKind::Blob)
                .collect::<Vec<_>>();
            if all_blobs.len() > DOWNLOAD_FILES_PER_SKILL_LIMIT {
                tracing::warn!(
                    total = all_blobs.len(),
                    limit = DOWNLOAD_FILES_PER_SKILL_LIMIT,
                    "root skill has more files than the per-skill download limit; some files will be skipped"
                );
            }
            let filtered = all_blobs
                .into_iter()
                .take(DOWNLOAD_FILES_PER_SKILL_LIMIT)
                .collect::<Vec<_>>();
            (filtered, String::new())
        } else {
            let source_dir = sanitize_relative(&candidate.directory)
                .ok_or_else(|| SkillError::InvalidPath(candidate.directory.clone()))?;
            let all_matching = tree
                .entries
                .into_iter()
                .filter(|e| {
                    e.kind == discover::EntryKind::Blob
                        && (e.path == source_dir || e.path.starts_with(&format!("{source_dir}/")))
                })
                .collect::<Vec<_>>();
            if all_matching.len() > DOWNLOAD_FILES_PER_SKILL_LIMIT {
                tracing::warn!(
                    directory = %source_dir,
                    total = all_matching.len(),
                    limit = DOWNLOAD_FILES_PER_SKILL_LIMIT,
                    "skill has more files than the per-skill download limit; some files will be skipped"
                );
            }
            let filtered = all_matching
                .into_iter()
                .take(DOWNLOAD_FILES_PER_SKILL_LIMIT)
                .collect::<Vec<_>>();
            (filtered, source_dir)
        };

        if !files.iter().any(|e| {
            let rel = if is_root {
                e.path.clone()
            } else {
                relative_to_source(&e.path, &source_dir)
            };
            !rel.contains('/') && discover::is_skill_md_path(&rel)
        }) {
            return Err(SkillError::MissingSkillFile {
                repo: format!("{}/{}", candidate.repo_owner, candidate.repo_name),
                dir: source_dir.clone(),
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
                // SK-2: root-level skill — the tree path is already the relative
                // install path; for subdirectory skills strip the source prefix.
                let relative = if is_root {
                    entry.path.clone()
                } else {
                    relative_to_source(&entry.path, &source_dir)
                };
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

        // SK-9: delete-last swap — rename old dest aside first so that if the
        // rename of tmp fails we still have the original content. Only delete
        // the old dir after the new content is in place.
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut moved_aside: Option<PathBuf> = None;
        if tokio::fs::try_exists(&dest).await.unwrap_or(false) {
            if let Some(parent) = dest.parent() {
                let aside = parent.join(format!(".{install_name}.old-{}", now_unix_nanos()));
                if tokio::fs::rename(&dest, &aside).await.is_ok() {
                    moved_aside = Some(aside);
                }
            }
        }
        if let Err(e) = tokio::fs::rename(&tmp, &dest).await {
            // Rename failed: restore the previous install from the aside so
            // the skill's managed source dir (which the DB row still points
            // at) doesn't vanish, then clean up tmp.
            if let Some(ref aside) = moved_aside {
                if let Err(restore_err) = tokio::fs::rename(aside, &dest).await {
                    tracing::warn!(
                        error = %restore_err,
                        aside = %aside.display(),
                        "failed to restore previous skill content after swap failure"
                    );
                }
            }
            let _ignored = tokio::fs::remove_dir_all(&tmp).await;
            return Err(SkillError::Io(e));
        }
        // New content is live — safe to drop the old dir.
        if let Some(aside) = moved_aside {
            let _ignored = tokio::fs::remove_dir_all(&aside).await;
        }

        let metadata = load_skill_metadata_from_dir(&dest, &install_name).await?;

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
            installed_at: now_unix_secs(),
            targets: normalized_targets.clone(),
        };
        // Metadata and targets are written separately: a re-install refresh
        // must not clobber targets the user later changed. Here the caller's
        // `targets` IS the intended set, so set them explicitly after metadata.
        self.db.upsert_skill_metadata(skill.clone()).await?;

        // SK-1: reconcile BEFORE writing the new managed_skill rows so that
        // `we_manage` reflects pre-install state. A first-time install has no
        // rows yet → we_manage=false → TargetExists guard fires for user-owned
        // dirs. A re-install already has rows → we_manage=true → overwrite ok.
        // (mirrors the ordering in `set_targets`)
        let failed = self
            .reconcile_global_targets(&install_name, &normalized_targets)
            .await?;

        // Record rows for the targets that actually synced. Conflicted targets
        // (TargetExists: a user-authored dir we refused to overwrite) are
        // excluded — claiming them would let a later reconcile delete the
        // user's dir. Other failures keep their row: the dir is ours (or
        // absent), and the next sync self-heals.
        let recorded = exclude_conflicted(&normalized_targets, &failed);
        self.db
            .set_skill_targets(&install_name, "global", recorded)
            .await?;
        self.remirror_repo_scopes(&install_name).await?;

        self.bus.publish(Event::SkillsChanged);
        if let Some((_, e)) = failed.into_iter().next() {
            return Err(e);
        }
        Ok(skill)
    }

    /// Replace a skill's GLOBAL editor targets (Skills-tab toggles) and reconcile
    /// each global skills dir to match. Repo scopes are managed via `set_skill_scope`.
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
        let failed = self
            .reconcile_global_targets(directory, &normalized)
            .await?;
        let recorded = exclude_conflicted(&normalized, &failed);
        self.db
            .set_skill_targets(directory, "global", recorded)
            .await?;
        self.bus.publish(Event::SkillsChanged);
        if let Some((_, e)) = failed.into_iter().next() {
            return Err(e);
        }
        Ok(())
    }

    /// Toggle one skill for one `(client, scope)`: mirror or unmirror that scope
    /// and reflect it in `managed_skill`. A client that can't host skills at the
    /// scope (e.g. VS Code at a repo) is skipped entirely — no row, no file.
    pub async fn set_skill_scope(
        &self,
        directory: &str,
        client_id: &str,
        scope: &str,
        enabled: bool,
    ) -> Result<(), SkillError> {
        let Some(client) = client_config::resolve_client(&self.db, client_id).await? else {
            return Err(SkillError::UnknownTarget(client_id.to_string()));
        };
        let Some(dest_dir) = client.skills_target(&Scope::from_db(scope)) else {
            return Ok(()); // no skills support at this scope — skip
        };
        let source = self.paths.skills_managed_dir().join(directory);
        let install_name = directory.to_string();
        let cfg = self.paths.config_toml();
        // SK-1 applies here too: a destination dir Conduit didn't write (no
        // pre-existing managed_skill row) must not be overwritten or removed.
        let we_manage = self
            .db
            .list_skill_targets_scoped(directory)
            .await?
            .iter()
            .any(|(c, s)| c == client_id && s == scope);
        if enabled {
            // Sync FIRST, row after — recording the row before a TargetExists
            // conflict would claim a user-authored dir for later removal.
            let dest_for_rollback = dest_dir.clone();
            let name_for_rollback = install_name.clone();
            tokio::task::spawn_blocking(move || -> Result<(), SkillError> {
                let mode = Settings::load_or_default(&cfg).skills.sync_mode;
                sync::sync_to_target(&source, &dest_dir, &install_name, mode, we_manage)
            })
            .await
            .map_err(|e| SkillError::InvalidPath(format!("join: {e}")))??;
            if let Err(e) = self.db.add_skill_target(directory, client_id, scope).await {
                // The dir is provably ours (sync just succeeded) but no row
                // records that; leaving it would brick every retry with
                // TargetExists. Best-effort roll the sync back.
                if !we_manage {
                    let _ignored = tokio::task::spawn_blocking(move || {
                        sync::remove_from_target(&dest_for_rollback, &name_for_rollback);
                    })
                    .await;
                }
                return Err(e.into());
            }
        } else {
            self.db
                .remove_skill_target(directory, client_id, scope)
                .await?;
            if we_manage {
                tokio::task::spawn_blocking(move || {
                    sync::remove_from_target(&dest_dir, &install_name);
                })
                .await
                .map_err(|e| SkillError::InvalidPath(format!("join: {e}")))?;
            }
        }
        self.bus.publish(Event::SkillsChanged);
        Ok(())
    }

    pub async fn uninstall(&self, directory: &str) -> Result<(), SkillError> {
        // Remove the mirror from every (client, scope) it lives in, driven from
        // the DB (not the IDE catalog), then drop the managed dir + DB row.
        let dests: Vec<PathBuf> = self
            .resolve_scope_targets(directory)
            .await?
            .into_iter()
            .filter_map(|(client, scope)| client.skills_target(&scope))
            .collect();
        let install_name = directory.to_string();
        tokio::task::spawn_blocking(move || {
            for dest_dir in dests {
                sync::remove_from_target(&dest_dir, &install_name);
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
        // SK-3: compare case-insensitively to avoid double-registration on
        // case-insensitive filesystems (macOS/Windows).
        let known_lower: std::collections::HashSet<String> = self
            .db
            .list_skills()
            .await?
            .into_iter()
            .map(|s| s.directory.to_ascii_lowercase())
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
            if name.starts_with('.') || known_lower.contains(&name.to_ascii_lowercase()) {
                continue;
            }
            let Ok(meta) = load_skill_metadata_from_dir(&entry.path(), &name).await else {
                continue;
            };
            let now = now_unix_secs();
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
            // Metadata-only: a locally-discovered skill starts with zero
            // targets and must never disturb existing managed_skill rows.
            if let Err(e) = self.db.upsert_skill_metadata(skill).await {
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

    async fn invalidate_discover_cache(&self) {
        let mut guard = self.discover_cache.lock().await;
        *guard = None;
    }

    /// Resolve every `(client, scope)` this skill targets into `ResolvedClient`
    /// + `Scope` pairs — the skill analogue of the MCP cascade's affected list.
    async fn resolve_scope_targets(
        &self,
        directory: &str,
    ) -> Result<Vec<(ResolvedClient, Scope)>, SkillError> {
        let scoped = self.db.list_skill_targets_scoped(directory).await?;
        if scoped.is_empty() {
            return Ok(Vec::new());
        }
        // Resolve all client rows once and look up locally — the per-row
        // `resolve_client` (a `get_client` actor roundtrip each) was an N+1.
        let clients: std::collections::HashMap<String, ResolvedClient> = self
            .db
            .list_clients()
            .await?
            .into_iter()
            .map(|row| (row.id.clone(), ResolvedClient::from_row(row)))
            .collect();
        let mut out = Vec::new();
        for (client_id, scope) in scoped {
            if let Some(client) = clients.get(&client_id) {
                out.push((client.clone(), Scope::from_db(&scope)));
            }
        }
        Ok(out)
    }

    /// Reconcile every built-in's GLOBAL skills dir to match `new_targets`:
    /// mirror the chosen ones, remove the rest. Removals are DB-gated: a dir is
    /// only removed when Conduit has a `managed_skill` row for it (SK-1 fix).
    ///
    /// A per-target failure does NOT abort the rest of the plan — aborting
    /// mid-plan leaves already-synced dirs with no `managed_skill` row, which
    /// permanently bricks retries (`we_manage=false` → `TargetExists` on a dir
    /// Conduit itself wrote). Failures are returned as `(client_id, error)` so
    /// the caller can exclude conflicted targets from the rows it records.
    async fn reconcile_global_targets(
        &self,
        directory: &str,
        new_targets: &[String],
    ) -> Result<Vec<(String, SkillError)>, SkillError> {
        let source = self.paths.skills_managed_dir().join(directory);
        let want: std::collections::HashSet<&str> =
            new_targets.iter().map(String::as_str).collect();

        // Build the set of client IDs that currently have a managed_skill row
        // in the global scope — only these are ours to remove.
        let currently_managed_global: std::collections::HashSet<String> = self
            .db
            .list_skill_targets_scoped(directory)
            .await?
            .into_iter()
            .filter(|(_, scope)| scope == "global")
            .map(|(client_id, _)| client_id)
            .collect();

        // (client_id, dir, want_it, we_manage_it)
        let mut plan: Vec<(String, PathBuf, bool, bool)> = Vec::new();
        for row in self.db.list_clients().await? {
            let client = ResolvedClient::from_row(row);
            if !client.is_builtin {
                continue;
            }
            if let Some(dir) = client.skills_target(&Scope::Global) {
                let want_it = want.contains(client.id.as_str());
                let we_manage = currently_managed_global.contains(&client.id);
                plan.push((client.id, dir, want_it, we_manage));
            }
        }
        let install_name = directory.to_string();
        let cfg = self.paths.config_toml();
        tokio::task::spawn_blocking(move || -> Vec<(String, SkillError)> {
            let mode = Settings::load_or_default(&cfg).skills.sync_mode;
            let mut failed = Vec::new();
            for (client_id, dest_dir, want_it, we_manage) in plan {
                if want_it {
                    if let Err(e) =
                        sync::sync_to_target(&source, &dest_dir, &install_name, mode, we_manage)
                    {
                        tracing::warn!(
                            error = %e,
                            client = %client_id,
                            skill = %install_name,
                            "skill sync failed; continuing with remaining targets"
                        );
                        failed.push((client_id, e));
                    }
                } else if we_manage {
                    // Only remove what Conduit wrote.
                    sync::remove_from_target(&dest_dir, &install_name);
                }
                // Not wanted, not managed → skip; don't touch the user's dir.
            }
            failed
        })
        .await
        .map_err(|e| SkillError::InvalidPath(format!("join: {e}")))
    }

    /// Re-mirror the skill into every REPO scope it's already in — a re-install
    /// refresh propagates updated content to repos (global is handled separately).
    async fn remirror_repo_scopes(&self, directory: &str) -> Result<(), SkillError> {
        let source = self.paths.skills_managed_dir().join(directory);
        let dests: Vec<PathBuf> = self
            .resolve_scope_targets(directory)
            .await?
            .into_iter()
            .filter(|(_, scope)| matches!(scope, Scope::Repo(_)))
            .filter_map(|(client, scope)| client.skills_target(&scope))
            .collect();
        if dests.is_empty() {
            return Ok(());
        }
        let install_name = directory.to_string();
        let cfg = self.paths.config_toml();
        tokio::task::spawn_blocking(move || -> Result<(), SkillError> {
            let mode = Settings::load_or_default(&cfg).skills.sync_mode;
            for dest_dir in dests {
                // we_manage=true: these dests came from the managed_skill DB table.
                sync::sync_to_target(&source, &dest_dir, &install_name, mode, true)?;
            }
            Ok(())
        })
        .await
        .map_err(|e| SkillError::InvalidPath(format!("join: {e}")))?
    }
}

fn validate_repo_input(repo: &SkillRepo) -> Result<(), SkillError> {
    fn ok(value: &str) -> bool {
        // GitHub login/repo grammar: ASCII alphanumeric + . _ -; reject all-dot
        // strings so "." and ".." can't sneak into URLs we construct later.
        !value.is_empty()
            && !value.chars().all(|c| c == '.')
            && value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
    }
    if !ok(&repo.owner) || !ok(&repo.name) || !ok(&repo.branch) {
        return Err(SkillError::InvalidPath(format!(
            "{}/{}@{}",
            repo.owner, repo.name, repo.branch
        )));
    }
    Ok(())
}

fn normalize_targets(targets: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = targets
        .into_iter()
        .filter(|t| skill_capable_builtins().any(|def| def.id == t))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Drop targets whose sync failed with `TargetExists` (user-owned dir we must
/// not claim). Targets that failed for other reasons keep their row — the dir
/// is ours or absent, and the next sync self-heals.
fn exclude_conflicted(targets: &[String], failed: &[(String, SkillError)]) -> Vec<String> {
    targets
        .iter()
        .filter(|t| {
            !failed
                .iter()
                .any(|(id, e)| id == *t && matches!(e, SkillError::TargetExists(_)))
        })
        .cloned()
        .collect()
}

fn sanitize_segment(segment: &str) -> Option<String> {
    let s = segment.trim();
    if s.is_empty() || s == "." || s == ".." {
        return None;
    }
    if s.contains('/') || s.contains('\\') || s.contains('\0') {
        return None;
    }
    Some(s.to_string())
}

fn sanitize_relative(value: &str) -> Option<String> {
    let raw = value.replace('\\', "/");
    let raw = raw.trim();
    if raw.is_empty() || raw.starts_with('/') || raw.contains('\0') {
        return None;
    }
    let parts: Vec<&str> = raw.split('/').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() || parts.iter().any(|p| *p == "." || *p == "..") {
        return None;
    }
    Some(parts.join("/"))
}

fn last_segment(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn relative_to_source(path: &str, source_dir: &str) -> String {
    if path == source_dir {
        last_segment(path).to_string()
    } else {
        let prefix = format!("{source_dir}/");
        path.strip_prefix(&prefix).unwrap_or(path).to_string()
    }
}

async fn load_skill_metadata_from_dir(
    dir: &std::path::Path,
    fallback: &str,
) -> Result<discover::SkillMetadata, SkillError> {
    let skill_md = locate_skill_md(dir).await?;
    let bytes = tokio::fs::read(&skill_md).await?;
    Ok(discover::read_skill_metadata(
        std::str::from_utf8(&bytes).unwrap_or(""),
        fallback,
    ))
}

async fn locate_skill_md(dir: &std::path::Path) -> Result<PathBuf, SkillError> {
    if let Some(p) = first_existing_skill_md(dir).await {
        return Ok(p);
    }
    Err(SkillError::MissingSkillFile {
        repo: dir.display().to_string(),
        dir: String::new(),
    })
}

// SK-14: use read_dir with a case-insensitive filename compare so that a repo
// with e.g. "SKILL.MD" is found on case-sensitive filesystems (Linux).
async fn first_existing_skill_md(dir: &std::path::Path) -> Option<PathBuf> {
    let mut reader = tokio::fs::read_dir(dir).await.ok()?;
    while let Ok(Some(entry)) = reader.next_entry().await {
        let fname = entry.file_name();
        let lower = fname.to_string_lossy().to_ascii_lowercase();
        if lower == "skill.md" {
            return Some(entry.path());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "test ergonomics")]
    use super::*;

    #[test]
    fn sanitize_rejects_traversal() {
        assert!(sanitize_relative("../etc/passwd").is_none());
        assert!(sanitize_relative("/abs/path").is_none());
        assert!(sanitize_relative("a/./b").is_none());
        assert_eq!(sanitize_relative("a/b/c").unwrap(), "a/b/c");
        assert_eq!(sanitize_relative("a\\b").unwrap(), "a/b");
    }

    #[test]
    fn segment_strips_whitespace_and_separators() {
        assert_eq!(sanitize_segment(" my-skill ").unwrap(), "my-skill");
        assert!(sanitize_segment("..").is_none());
        assert!(sanitize_segment("a/b").is_none());
    }

    #[test]
    fn relative_to_source_strips_prefix() {
        assert_eq!(relative_to_source("dir/SKILL.md", "dir"), "SKILL.md");
        assert_eq!(relative_to_source("dir/a/b.md", "dir"), "a/b.md");
        // root-level file (path == source_dir) → use file_name
        assert_eq!(relative_to_source("SKILL.md", "SKILL.md"), "SKILL.md");
    }

    #[test]
    fn normalize_targets_keeps_known_clients_only() {
        let out = normalize_targets(vec![
            "ClaudeCode".into(),
            "Cursor".into(),
            "OpenCode".into(),
            "VsCodeCopilot".into(), // no skills dir — filtered out
            "Bogus".into(),
            "ClaudeCode".into(),
        ]);
        assert_eq!(
            out,
            vec![
                "ClaudeCode".to_string(),
                "Cursor".to_string(),
                "OpenCode".to_string(),
            ]
        );
    }
}
