//! Source-repo management and the cached discover query for `SkillManager`.
//!
//! CRUD over the user's list of GitHub skill repos (add/remove/enable) plus
//! `discover`, which serves the available-skills set from an in-memory cache
//! keyed by a fingerprint of the enabled repos and a TTL, refetching via the
//! `discover` module on miss. Mutations publish `Event::SkillsChanged`.

use std::time::{Duration, SystemTime};

use crate::daemon::db::types::SkillRepo;
use crate::daemon::errors::SkillError;
use crate::daemon::events::Event;

use super::{discover, DiscoverCache, SkillCandidate, SkillManager};

const DISCOVER_CACHE_TTL: Duration = Duration::from_secs(60 * 60);

impl SkillManager {
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
