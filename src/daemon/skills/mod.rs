// Skills subsystem: discover SKILL.md packages on GitHub, stash a canonical
// copy under `<config>/conduit/skills/managed/<name>/`, and mirror that
// copy into each agentic IDE's skills dir (`~/.claude/skills`, etc.).

pub mod discover;
pub mod sync;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::daemon::clients::{self, SKILL_TARGET_IDES};
use crate::daemon::db::types::{Skill, SkillRepo};
use crate::daemon::db::Db;
use crate::daemon::errors::SkillError;
use crate::daemon::events::{Bus, Event};
use crate::paths::Paths;

const DISCOVER_CACHE_TTL: Duration = Duration::from_secs(60 * 60);
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

    /// Return the set of available (not installed) skills, fetching from GitHub
    /// when the in-memory cache is stale.
    pub async fn discover(&self, force: bool) -> Result<Vec<SkillCandidate>, SkillError> {
        let repos = self.db.list_skill_repos().await?;
        let enabled: Vec<SkillRepo> = repos.into_iter().filter(|r| r.enabled).collect();
        let fingerprint = discover::fingerprint(&enabled);

        if !force {
            let cached = self.discover_cache.lock().await.clone();
            if let Some(cache) = cached {
                if cache.fingerprint == fingerprint
                    && cache.generated_at.elapsed().unwrap_or(DISCOVER_CACHE_TTL)
                        < DISCOVER_CACHE_TTL
                {
                    return Ok(cache.skills);
                }
            }
        }

        let skills = discover::discover_skills(&self.http, &enabled).await?;
        *self.discover_cache.lock().await = Some(DiscoverCache {
            fingerprint,
            generated_at: SystemTime::now(),
            skills: skills.clone(),
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

    async fn invalidate_discover_cache(&self) {
        let mut guard = self.discover_cache.lock().await;
        *guard = None;
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

fn normalize_targets(targets: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = targets
        .into_iter()
        .filter(|t| {
            SKILL_TARGET_IDES
                .iter()
                .any(|ide| clients::ide_db_name(*ide) == t)
        })
        .collect();
    out.sort();
    out.dedup();
    out
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

async fn locate_skill_md(dir: &std::path::Path) -> Result<PathBuf, SkillError> {
    if let Some(p) = first_existing_skill_md(dir).await {
        return Ok(p);
    }
    Err(SkillError::MissingSkillFile {
        repo: dir.display().to_string(),
        dir: String::new(),
    })
}

async fn first_existing_skill_md(dir: &std::path::Path) -> Option<PathBuf> {
    for name in ["SKILL.md", "Skill.md", "skill.md"] {
        let candidate = dir.join(name);
        if tokio::fs::try_exists(&candidate).await.unwrap_or(false) {
            return Some(candidate);
        }
    }
    None
}

fn now_unix() -> i64 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    i64::try_from(secs).unwrap_or(i64::MAX)
}

fn now_unix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
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
    fn normalize_targets_keeps_known_ides_only() {
        let out = normalize_targets(vec![
            "ClaudeCode".into(),
            "Cursor".into(),
            "OpenCode".into(),
            "VsCodeCopilot".into(), // no global skills dir — filtered out
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
