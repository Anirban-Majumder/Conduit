//! Skills-tab Slint callbacks and model painting.
//!
//! Wires install/uninstall, per-IDE target toggles, repo add/remove/enable,
//! search filtering, and folder reveal, all delegating to `SkillManager` on the
//! tokio worker. The `paint_skills_*` / `*_to_rows` helpers merge installed
//! skills with discovered candidates into the Slint models. UI writes go through
//! `upgrade_in_event_loop`.

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use conduit::daemon::client_config;
use conduit::daemon::db::types::SkillRepo;
use conduit::daemon::skills::{SkillCandidate, SkillManager};

use crate::window::refresh::ide_icon;
use crate::window::view::{IdeTarget, MainWindow, SkillRepoRow, SkillRow};
use crate::window::CallbackContext;

#[expect(
    clippy::too_many_lines,
    reason = "Skills tab wires eight callbacks at once"
)]
pub(crate) fn install_skills_callbacks(window: &MainWindow, ctx: &CallbackContext) {
    let rt = &ctx.rt;
    let skills = &ctx.skills;

    let weak = window.as_weak();
    let rt_for_refresh = rt.clone();
    let skills_for_refresh = skills.clone();
    window.on_skills_refresh_clicked(move || {
        let skills = skills_for_refresh.clone();
        let weak = weak.clone();
        weak.upgrade_in_event_loop(|ui| {
            ui.set_skills_busy(true);
            ui.set_skills_status_message(SharedString::from("Refreshing…"));
        })
        .ok();
        rt_for_refresh.spawn(async move {
            match skills.discover(true).await {
                Ok(_) => {
                    refresh_skills(&weak, &skills).await;
                    weak.upgrade_in_event_loop(|ui| {
                        ui.set_skills_busy(false);
                        ui.set_skills_status_message(SharedString::from(""));
                    })
                    .ok();
                }
                Err(e) => {
                    let msg = format!("Refresh failed: {e}");
                    tracing::warn!(error = %e, "skill discover failed");
                    weak.upgrade_in_event_loop(move |ui| {
                        ui.set_skills_busy(false);
                        ui.set_skills_status_message(SharedString::from(msg));
                    })
                    .ok();
                }
            }
        });
    });

    let weak = window.as_weak();
    let rt_for_install = rt.clone();
    let skills_for_install = skills.clone();
    window.on_skills_install_clicked(move |owner, name, branch, dir, readme, candidate_name| {
        let skills = skills_for_install.clone();
        let weak = weak.clone();
        let candidate = SkillCandidate {
            key: format!("{owner}/{name}:{dir}"),
            name: candidate_name.to_string(),
            description: String::new(),
            directory: dir.to_string(),
            readme_url: readme.to_string(),
            repo_owner: owner.to_string(),
            repo_name: name.to_string(),
            repo_branch: branch.to_string(),
        };
        weak.upgrade_in_event_loop(|ui| {
            ui.set_skills_busy(true);
            ui.set_skills_status_message(SharedString::from("Installing…"));
        })
        .ok();
        rt_for_install.spawn(async move {
            // No targets by default — user opts in per-IDE after install.
            let result = skills.install(candidate, Vec::new()).await;
            match result {
                Ok(skill) => {
                    let msg = format!("Installed {}", skill.skill_name);
                    weak.upgrade_in_event_loop(move |ui| {
                        ui.set_skills_busy(false);
                        ui.set_skills_status_message(SharedString::from(msg));
                    })
                    .ok();
                }
                Err(e) => {
                    let msg = format!("Install failed: {e}");
                    tracing::warn!(error = %e, "skill install failed");
                    weak.upgrade_in_event_loop(move |ui| {
                        ui.set_skills_busy(false);
                        ui.set_skills_status_message(SharedString::from(msg));
                    })
                    .ok();
                }
            }
        });
    });

    let weak = window.as_weak();
    let rt_for_uninstall = rt.clone();
    let skills_for_uninstall = skills.clone();
    window.on_skills_uninstall_clicked(move |directory| {
        let skills = skills_for_uninstall.clone();
        let weak = weak.clone();
        let directory = directory.to_string();
        rt_for_uninstall.spawn(async move {
            if let Err(e) = skills.uninstall(&directory).await {
                let msg = format!("Uninstall failed: {e}");
                tracing::warn!(error = %e, directory = %directory, "skill uninstall failed");
                weak.upgrade_in_event_loop(move |ui| {
                    ui.set_skills_status_message(SharedString::from(msg));
                })
                .ok();
            }
        });
    });

    let rt_for_add_repo = rt.clone();
    let skills_for_add_repo = skills.clone();
    window.on_skills_add_repo(move |owner, name, branch| {
        let skills = skills_for_add_repo.clone();
        let repo = SkillRepo {
            owner: owner.to_string(),
            name: name.to_string(),
            branch: branch.to_string(),
            enabled: true,
        };
        rt_for_add_repo.spawn(async move {
            if let Err(e) = skills.add_repo(repo).await {
                tracing::warn!(error = %e, "skill add_repo failed");
            }
        });
    });

    let rt_for_remove_repo = rt.clone();
    let skills_for_remove_repo = skills.clone();
    window.on_skills_remove_repo(move |owner, name| {
        let skills = skills_for_remove_repo.clone();
        let owner = owner.to_string();
        let name = name.to_string();
        rt_for_remove_repo.spawn(async move {
            if let Err(e) = skills.remove_repo(&owner, &name).await {
                tracing::warn!(error = %e, "skill remove_repo failed");
            }
        });
    });

    let rt_for_toggle_repo = rt.clone();
    let skills_for_toggle_repo = skills.clone();
    window.on_skills_toggle_repo(move |owner, name, enabled| {
        let skills = skills_for_toggle_repo.clone();
        let owner = owner.to_string();
        let name = name.to_string();
        rt_for_toggle_repo.spawn(async move {
            if let Err(e) = skills.set_repo_enabled(&owner, &name, enabled).await {
                tracing::warn!(error = %e, "skill set_repo_enabled failed");
            }
        });
    });

    let weak = window.as_weak();
    window.on_skills_search_changed(move |query| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let all: Vec<SkillRow> = ui.get_skills_all().iter().collect();
        let filtered = filter_skill_rows(&all, query.as_str());
        ui.set_skills(ModelRc::new(VecModel::from(filtered)));
    });

    let rt_for_open_folder = rt.clone();
    let skills_for_open_folder = skills.clone();
    let weak = window.as_weak();
    window.on_skills_open_folder_clicked(move || {
        let skills = skills_for_open_folder.clone();
        let weak = weak.clone();
        rt_for_open_folder.spawn(async move {
            let dir = skills.paths().skills_managed_dir();
            if let Err(e) = tokio::fs::create_dir_all(&dir).await {
                tracing::warn!(error = %e, "create skills dir failed");
            }
            let dir_for_open = dir.clone();
            let open_result = tokio::task::spawn_blocking(move || open::that(&dir_for_open)).await;
            match open_result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    let msg = format!("Couldn't open folder: {e}");
                    tracing::warn!(error = %e, path = %dir.display(), "open folder failed");
                    weak.upgrade_in_event_loop(move |ui| {
                        ui.set_skills_status_message(SharedString::from(msg));
                    })
                    .ok();
                }
                Err(e) => tracing::warn!(error = %e, "open folder join failed"),
            }
        });
    });
}

pub(crate) async fn refresh_skills(weak: &slint::Weak<MainWindow>, skills: &SkillManager) {
    if let Err(e) = skills.register_local_skills().await {
        tracing::warn!(error = %e, "register_local_skills failed");
    }
    let active_client_ids = skills.active_skill_client_ids().await;
    paint_skills_from_db(weak, skills, &active_client_ids).await;
    match skills.discover(false).await {
        Ok(candidates) => {
            paint_skills_with_discovery(weak, skills, candidates, &active_client_ids).await;
        }
        Err(e) => tracing::warn!(error = %e, "skill discover failed during refresh"),
    }
}

pub(crate) async fn paint_skills_from_db(
    weak: &slint::Weak<MainWindow>,
    skills: &SkillManager,
    active_client_ids: &[String],
) {
    let installed = skills.list_installed().await.unwrap_or_default();
    let repos = skills.list_repos().await.unwrap_or_default();
    let repo_rows = repos_to_rows(repos);
    let active_client_ids = active_client_ids.to_vec();
    // Rows carry brand-icon `Image`s (!Send), so build them on the UI thread.
    let _ignored = weak.upgrade_in_event_loop(move |ui| {
        let rows = installed_skills_to_rows(&installed, &active_client_ids);
        let filtered = filter_skill_rows(&rows, ui.get_skills_search_query().as_str());
        ui.set_skills(ModelRc::new(VecModel::from(filtered)));
        ui.set_skills_all(ModelRc::new(VecModel::from(rows)));
        ui.set_skill_repos(ModelRc::new(VecModel::from(repo_rows)));
    });
}

pub(crate) fn filter_skill_rows(rows: &[SkillRow], query: &str) -> Vec<SkillRow> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return rows.to_vec();
    }
    rows.iter()
        .filter(|r| {
            r.name.to_lowercase().contains(q.as_str())
                || r.description.to_lowercase().contains(q.as_str())
                || r.repo_label.to_lowercase().contains(q.as_str())
        })
        .cloned()
        .collect()
}

pub(crate) async fn paint_skills_with_discovery(
    weak: &slint::Weak<MainWindow>,
    skills: &SkillManager,
    candidates: Vec<SkillCandidate>,
    active_client_ids: &[String],
) {
    let installed = skills.list_installed().await.unwrap_or_default();
    let repos = skills.list_repos().await.unwrap_or_default();
    let repo_rows = repos_to_rows(repos);
    // SK-3: normalise to lowercase for case-insensitive FS collision detection.
    let installed_dirs_lower: std::collections::HashSet<String> = installed
        .iter()
        .map(|s| s.directory.to_ascii_lowercase())
        .collect();
    let active_client_ids = active_client_ids.to_vec();

    // Rows carry brand-icon `Image`s (!Send), so build them on the UI thread.
    let _ignored = weak.upgrade_in_event_loop(move |ui| {
        let mut rows = installed_skills_to_rows(&installed, &active_client_ids);
        for candidate in candidates {
            // SK-2: root-level sentinel means repo name is the install dir.
            let install_name = if candidate.directory.is_empty() {
                candidate.repo_name.clone()
            } else {
                candidate
                    .directory
                    .rsplit('/')
                    .next()
                    .unwrap_or(&candidate.directory)
                    .to_string()
            };
            if installed_dirs_lower.contains(&install_name.to_ascii_lowercase()) {
                continue;
            }
            rows.push(SkillRow {
                key: SharedString::from(candidate.key.clone()),
                directory: SharedString::from(install_name),
                name: SharedString::from(candidate.name),
                description: SharedString::from(candidate.description),
                repo_label: SharedString::from(format!(
                    "{}/{} · {}",
                    candidate.repo_owner, candidate.repo_name, candidate.repo_branch
                )),
                repo_owner: SharedString::from(candidate.repo_owner),
                repo_name: SharedString::from(candidate.repo_name),
                repo_branch: SharedString::from(candidate.repo_branch),
                source_directory: SharedString::from(candidate.directory),
                readme_url: SharedString::from(candidate.readme_url),
                installed: false,
                // Not-installed rows hide their toggles, so no targets needed.
                targets: empty_targets(),
            });
        }
        rows.sort_by(|a, b| match (a.installed, b.installed) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        });
        let filtered = filter_skill_rows(&rows, ui.get_skills_search_query().as_str());
        ui.set_skills(ModelRc::new(VecModel::from(filtered)));
        ui.set_skills_all(ModelRc::new(VecModel::from(rows)));
        ui.set_skill_repos(ModelRc::new(VecModel::from(repo_rows)));
    });
}

/// Build the per-skill IDE toggle model: one `IdeTarget` per detected
/// skill-capable IDE, `active` reflecting whether the skill is synced there.
/// Must run on the UI thread — `IdeTarget.icon` is a !Send `slint::Image`.
fn skill_targets(enabled: &[String], active_client_ids: &[String]) -> ModelRc<IdeTarget> {
    let targets: Vec<IdeTarget> = active_client_ids
        .iter()
        .filter_map(|client_id| {
            let def = client_config::builtin_by_id(client_id)?;
            Some(IdeTarget {
                ide_kind: SharedString::from(def.id),
                label: SharedString::from(def.display_name),
                icon: ide_icon(def.id),
                active: enabled.iter().any(|t| t == def.id),
            })
        })
        .collect();
    ModelRc::new(VecModel::from(targets))
}

fn empty_targets() -> ModelRc<IdeTarget> {
    ModelRc::new(VecModel::<IdeTarget>::default())
}

pub(crate) fn installed_skills_to_rows(
    installed: &[conduit::daemon::db::types::Skill],
    active_client_ids: &[String],
) -> Vec<SkillRow> {
    installed
        .iter()
        .map(|skill| SkillRow {
            key: SharedString::from(format!(
                "{}/{}:{}",
                skill.repo_owner, skill.repo_name, skill.directory
            )),
            directory: SharedString::from(skill.directory.clone()),
            name: SharedString::from(skill.skill_name.clone()),
            description: SharedString::from(skill.description.clone()),
            repo_label: SharedString::from(format!(
                "{}/{} · {}",
                skill.repo_owner, skill.repo_name, skill.repo_branch
            )),
            repo_owner: SharedString::from(skill.repo_owner.clone()),
            repo_name: SharedString::from(skill.repo_name.clone()),
            repo_branch: SharedString::from(skill.repo_branch.clone()),
            source_directory: SharedString::from(skill.source_directory.clone()),
            readme_url: SharedString::from(skill.readme_url.clone().unwrap_or_default()),
            installed: true,
            targets: skill_targets(&skill.targets, active_client_ids),
        })
        .collect()
}

pub(crate) fn repos_to_rows(repos: Vec<SkillRepo>) -> Vec<SkillRepoRow> {
    repos
        .into_iter()
        .map(|r| SkillRepoRow {
            owner: SharedString::from(r.owner),
            name: SharedString::from(r.name),
            branch: SharedString::from(r.branch),
            enabled: r.enabled,
        })
        .collect()
}
