//! `skill_repos` and `skills` table storage.
//!
//! `Db` methods and their handlers for source-repo rows and installed-skill
//! rows (with per-skill client targets via `managed_skill`). The `SkillManager`
//! calls these; mutations are surfaced to the GUI via `Event::SkillsChanged`.

use super::{Db, DbCmd};
use rusqlite::{params, Connection, OptionalExtension};

use crate::daemon::db::types::{Skill, SkillRepo};
use crate::daemon::errors::DbError;

pub(super) fn query_list_skill_repos(conn: &Connection) -> Result<Vec<SkillRepo>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT owner, name, branch, enabled FROM skill_repos ORDER BY owner, name",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(SkillRepo {
            owner: row.get(0)?,
            name: row.get(1)?,
            branch: row.get(2)?,
            enabled: row.get::<_, i64>(3)? != 0,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub(super) fn query_upsert_skill_repo(conn: &Connection, repo: &SkillRepo) -> Result<(), DbError> {
    conn.prepare_cached(
        "INSERT INTO skill_repos (owner, name, branch, enabled) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(owner, name) DO UPDATE SET
             branch = excluded.branch,
             enabled = excluded.enabled",
    )?
    .execute(params![
        &repo.owner,
        &repo.name,
        &repo.branch,
        i64::from(repo.enabled),
    ])?;
    Ok(())
}

pub(super) fn query_delete_skill_repo(
    conn: &Connection,
    owner: &str,
    name: &str,
) -> Result<(), DbError> {
    conn.prepare_cached("DELETE FROM skill_repos WHERE owner = ?1 AND name = ?2")?
        .execute(params![owner, name])?;
    Ok(())
}

pub(super) fn query_set_skill_repo_enabled(
    conn: &Connection,
    owner: &str,
    name: &str,
    enabled: bool,
) -> Result<(), DbError> {
    conn.prepare_cached("UPDATE skill_repos SET enabled = ?1 WHERE owner = ?2 AND name = ?3")?
        .execute(params![i64::from(enabled), owner, name])?;
    Ok(())
}

/// Upsert skill *metadata only* — never touches `managed_skill`.
///
/// The write path is deliberately split: a metadata re-sync (install refresh,
/// `register_local_skills`) must NOT wipe user-set client targets. Targets are
/// mutated only through `query_set_skill_targets` / `query_add_skill_target` /
/// `query_remove_skill_target`. (The old combined fn delete-then-reinserted
/// `managed_skill` here, so any refresh with a stale `targets` vec destroyed
/// user intent under `ON DELETE CASCADE`.)
pub(super) fn query_upsert_skill_metadata(conn: &Connection, skill: &Skill) -> Result<(), DbError> {
    conn.prepare_cached(
        "INSERT INTO skills
             (directory, repo_owner, repo_name, repo_branch, source_directory,
              skill_name, description, readme_url, installed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(directory) DO UPDATE SET
             repo_owner = excluded.repo_owner,
             repo_name = excluded.repo_name,
             repo_branch = excluded.repo_branch,
             source_directory = excluded.source_directory,
             skill_name = excluded.skill_name,
             description = excluded.description,
             readme_url = excluded.readme_url,
             installed_at = excluded.installed_at",
    )?
    .execute(params![
        &skill.directory,
        &skill.repo_owner,
        &skill.repo_name,
        &skill.repo_branch,
        &skill.source_directory,
        &skill.skill_name,
        &skill.description,
        &skill.readme_url,
        skill.installed_at,
    ])?;
    Ok(())
}

pub(super) fn query_add_skill_target(
    conn: &Connection,
    directory: &str,
    client_id: &str,
    scope: &str,
) -> Result<(), DbError> {
    conn.prepare_cached(
        "INSERT OR IGNORE INTO managed_skill (client_id, scope, skill_dir) VALUES (?1, ?2, ?3)",
    )?
    .execute(params![client_id, scope, directory])?;
    Ok(())
}

pub(super) fn query_remove_skill_target(
    conn: &Connection,
    directory: &str,
    client_id: &str,
    scope: &str,
) -> Result<(), DbError> {
    conn.prepare_cached(
        "DELETE FROM managed_skill WHERE skill_dir = ?1 AND client_id = ?2 AND scope = ?3",
    )?
    .execute(params![directory, client_id, scope])?;
    Ok(())
}

/// Remove a skill from every scope of one client ("Remove from all").
pub(super) fn query_remove_skill_all_scopes(
    conn: &Connection,
    directory: &str,
    client_id: &str,
) -> Result<(), DbError> {
    conn.prepare_cached("DELETE FROM managed_skill WHERE skill_dir = ?1 AND client_id = ?2")?
        .execute(params![directory, client_id])?;
    Ok(())
}

/// Replace the set of client targets for one `(skill, scope)`.
pub(super) fn query_set_skill_targets(
    conn: &Connection,
    directory: &str,
    scope: &str,
    targets: &[String],
) -> Result<(), DbError> {
    let tx = conn.unchecked_transaction()?;
    tx.prepare_cached("DELETE FROM managed_skill WHERE skill_dir = ?1 AND scope = ?2")?
        .execute(params![directory, scope])?;
    for client_id in targets {
        tx.prepare_cached(
            "INSERT OR IGNORE INTO managed_skill (client_id, scope, skill_dir) VALUES (?1, ?2, ?3)",
        )?
        .execute(params![client_id, scope, directory])?;
    }
    tx.commit()?;
    Ok(())
}

/// Every `(client_id, scope)` a skill is mirrored into — the skill analogue of
/// `query_list_clients_managing_server`, driving scope-aware sync (Phase 4).
pub(super) fn query_list_skill_targets_scoped(
    conn: &Connection,
    directory: &str,
) -> Result<Vec<(String, String)>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT client_id, scope FROM managed_skill WHERE skill_dir = ?1 ORDER BY client_id, scope",
    )?;
    let rows = stmt.query_map(params![directory], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub(super) fn query_delete_skill(conn: &Connection, directory: &str) -> Result<(), DbError> {
    conn.prepare_cached("DELETE FROM skills WHERE directory = ?1")?
        .execute(params![directory])?;
    Ok(())
}

/// Clients this skill is bound to in ANY scope (global or a repo) — drives the
/// Skills-tab per-editor icon's active state, which lights up if the skill is
/// synced anywhere for that editor. `DISTINCT` collapses a client present in
/// both global and a repo to one entry.
fn read_skill_targets(conn: &Connection, directory: &str) -> Result<Vec<String>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT DISTINCT client_id FROM managed_skill WHERE skill_dir = ?1 ORDER BY client_id",
    )?;
    let rows = stmt.query_map(params![directory], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn decode_skill_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Skill> {
    Ok(Skill {
        directory: row.get(0)?,
        repo_owner: row.get(1)?,
        repo_name: row.get(2)?,
        repo_branch: row.get(3)?,
        source_directory: row.get(4)?,
        skill_name: row.get(5)?,
        description: row.get(6)?,
        readme_url: row.get(7)?,
        installed_at: row.get(8)?,
        targets: Vec::new(),
    })
}

pub(super) fn query_get_skill(
    conn: &Connection,
    directory: &str,
) -> Result<Option<Skill>, DbError> {
    let row = conn
        .prepare_cached(
            "SELECT directory, repo_owner, repo_name, repo_branch, source_directory,
                    skill_name, description, readme_url, installed_at
             FROM skills WHERE directory = ?1",
        )?
        .query_row(params![directory], decode_skill_row)
        .optional()?;
    match row {
        Some(mut s) => {
            s.targets = read_skill_targets(conn, &s.directory)?;
            Ok(Some(s))
        }
        None => Ok(None),
    }
}

pub(super) fn query_list_skills(conn: &Connection) -> Result<Vec<Skill>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT directory, repo_owner, repo_name, repo_branch, source_directory,
                skill_name, description, readme_url, installed_at
         FROM skills ORDER BY skill_name",
    )?;
    let rows = stmt.query_map([], decode_skill_row)?;
    let mut out = Vec::new();
    for r in rows {
        let mut s = r?;
        s.targets = read_skill_targets(conn, &s.directory)?;
        out.push(s);
    }
    Ok(out)
}

impl Db {
    pub async fn list_skill_repos(&self) -> Result<Vec<SkillRepo>, DbError> {
        self.send_cmd(|reply| DbCmd::ListSkillRepos { reply }).await
    }

    pub async fn upsert_skill_repo(&self, repo: SkillRepo) -> Result<(), DbError> {
        self.send_cmd(|reply| DbCmd::UpsertSkillRepo { repo, reply })
            .await
    }

    pub async fn delete_skill_repo(&self, owner: &str, name: &str) -> Result<(), DbError> {
        self.send_cmd(|reply| DbCmd::DeleteSkillRepo {
            owner: owner.to_string(),
            name: name.to_string(),
            reply,
        })
        .await
    }

    pub async fn set_skill_repo_enabled(
        &self,
        owner: &str,
        name: &str,
        enabled: bool,
    ) -> Result<(), DbError> {
        self.send_cmd(|reply| DbCmd::SetSkillRepoEnabled {
            owner: owner.to_string(),
            name: name.to_string(),
            enabled,
            reply,
        })
        .await
    }

    /// Upsert skill metadata only — does not touch the client targets.
    pub async fn upsert_skill_metadata(&self, skill: Skill) -> Result<(), DbError> {
        self.send_cmd(|reply| DbCmd::UpsertSkillMetadata { skill, reply })
            .await
    }

    pub async fn add_skill_target(
        &self,
        directory: &str,
        client_id: &str,
        scope: &str,
    ) -> Result<(), DbError> {
        self.send_cmd(|reply| DbCmd::AddSkillTarget {
            directory: directory.to_string(),
            client_id: client_id.to_string(),
            scope: scope.to_string(),
            reply,
        })
        .await
    }

    pub async fn remove_skill_target(
        &self,
        directory: &str,
        client_id: &str,
        scope: &str,
    ) -> Result<(), DbError> {
        self.send_cmd(|reply| DbCmd::RemoveSkillTarget {
            directory: directory.to_string(),
            client_id: client_id.to_string(),
            scope: scope.to_string(),
            reply,
        })
        .await
    }

    pub async fn remove_skill_all_scopes(
        &self,
        directory: &str,
        client_id: &str,
    ) -> Result<(), DbError> {
        self.send_cmd(|reply| DbCmd::RemoveSkillAllScopes {
            directory: directory.to_string(),
            client_id: client_id.to_string(),
            reply,
        })
        .await
    }

    pub async fn set_skill_targets(
        &self,
        directory: &str,
        scope: &str,
        targets: Vec<String>,
    ) -> Result<(), DbError> {
        self.send_cmd(|reply| DbCmd::SetSkillTargets {
            directory: directory.to_string(),
            scope: scope.to_string(),
            targets,
            reply,
        })
        .await
    }

    pub async fn list_skill_targets_scoped(
        &self,
        directory: &str,
    ) -> Result<Vec<(String, String)>, DbError> {
        self.send_cmd(|reply| DbCmd::ListSkillTargetsScoped {
            directory: directory.to_string(),
            reply,
        })
        .await
    }

    pub async fn delete_skill(&self, directory: &str) -> Result<(), DbError> {
        self.send_cmd(|reply| DbCmd::DeleteSkill {
            directory: directory.to_string(),
            reply,
        })
        .await
    }

    pub async fn get_skill(&self, directory: &str) -> Result<Option<Skill>, DbError> {
        self.send_cmd(|reply| DbCmd::GetSkill {
            directory: directory.to_string(),
            reply,
        })
        .await
    }

    pub async fn list_skills(&self) -> Result<Vec<Skill>, DbError> {
        self.send_cmd(|reply| DbCmd::ListSkills { reply }).await
    }
}
