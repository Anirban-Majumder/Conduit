// GitHub-side IO for the skills subsystem: walk a repo's tree to find every
// SKILL.md, pull frontmatter (name/description), and fetch raw file bytes.

use serde::Deserialize;

use crate::daemon::db::types::SkillRepo;
use crate::daemon::errors::SkillError;

use super::SkillCandidate;

const MAX_SKILLS_PER_REPO: usize = 200;
const DISCOVER_CONCURRENCY: usize = 4;

#[derive(Debug, PartialEq, Eq)]
pub enum EntryKind {
    Blob,
    Tree,
    Other,
}

#[derive(Debug)]
pub struct TreeEntry {
    pub path: String,
    pub kind: EntryKind,
}

#[derive(Debug)]
pub struct RepoTree {
    pub branch: String,
    pub entries: Vec<TreeEntry>,
}

#[derive(Deserialize)]
struct ApiTreeResponse {
    tree: Vec<ApiTreeEntry>,
}

#[derive(Deserialize)]
struct ApiTreeEntry {
    path: String,
    #[serde(rename = "type")]
    kind: String,
}

pub fn fingerprint(repos: &[SkillRepo]) -> String {
    let mut parts: Vec<String> = repos
        .iter()
        .map(|r| format!("{}/{}@{}", r.owner, r.name, r.branch))
        .collect();
    parts.sort();
    parts.join("|")
}

pub async fn resolve_repo_tree(
    http: &reqwest::Client,
    owner: &str,
    name: &str,
    branch: &str,
) -> Result<RepoTree, SkillError> {
    let mut candidates: Vec<String> = Vec::new();
    if !branch.is_empty() && !branch.eq_ignore_ascii_case("head") {
        candidates.push(branch.to_string());
    }
    for fallback in ["main", "master"] {
        if !candidates.iter().any(|b| b == fallback) {
            candidates.push(fallback.to_string());
        }
    }

    let mut last_err: Option<SkillError> = None;
    for branch in &candidates {
        match fetch_tree_for_branch(http, owner, name, branch).await {
            Ok(entries) => {
                return Ok(RepoTree {
                    branch: branch.clone(),
                    entries,
                });
            }
            Err(SkillError::RateLimited(code)) => return Err(SkillError::RateLimited(code)),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| SkillError::RepoTree(format!("{owner}/{name}"))))
}

async fn fetch_tree_for_branch(
    http: &reqwest::Client,
    owner: &str,
    name: &str,
    branch: &str,
) -> Result<Vec<TreeEntry>, SkillError> {
    let url = format!(
        "https://api.github.com/repos/{owner}/{name}/git/trees/{}?recursive=1",
        url_segment(branch)
    );
    let resp = http
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?;
    let status = resp.status();
    if status.as_u16() == 429 || status.as_u16() == 403 {
        return Err(SkillError::RateLimited(status.as_u16()));
    }
    if !status.is_success() {
        return Err(SkillError::HttpStatus(status.as_u16()));
    }
    let body: ApiTreeResponse = resp.json().await?;
    Ok(body
        .tree
        .into_iter()
        .map(|e| TreeEntry {
            path: e.path.replace('\\', "/"),
            kind: match e.kind.as_str() {
                "blob" => EntryKind::Blob,
                "tree" => EntryKind::Tree,
                _ => EntryKind::Other,
            },
        })
        .collect())
}

pub async fn fetch_raw(
    http: &reqwest::Client,
    owner: &str,
    name: &str,
    branch: &str,
    path: &str,
) -> Result<Vec<u8>, SkillError> {
    let encoded = path
        .split('/')
        .map(url_segment)
        .collect::<Vec<_>>()
        .join("/");
    let url = format!("https://raw.githubusercontent.com/{owner}/{name}/{branch}/{encoded}");
    let resp = http.get(&url).send().await?;
    let status = resp.status();
    if status.as_u16() == 429 || status.as_u16() == 403 {
        return Err(SkillError::RateLimited(status.as_u16()));
    }
    if !status.is_success() {
        return Err(SkillError::HttpStatus(status.as_u16()));
    }
    let bytes = resp.bytes().await?;
    Ok(bytes.to_vec())
}

pub async fn discover_skills(
    http: &reqwest::Client,
    repos: &[SkillRepo],
) -> Result<Vec<SkillCandidate>, SkillError> {
    // Spawn count is bounded by `repos.len()` (user-managed list, typically
    // <10); HTTP concurrency is bounded by `DISCOVER_CONCURRENCY` via the
    // semaphore each task acquires before issuing a request.
    let mut joinset: tokio::task::JoinSet<Result<Vec<SkillCandidate>, SkillError>> =
        tokio::task::JoinSet::new();
    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(DISCOVER_CONCURRENCY));

    for repo in repos {
        let http = http.clone();
        let repo = repo.clone();
        let sem = sem.clone();
        joinset.spawn(async move {
            // Bounded concurrency: at most DISCOVER_CONCURRENCY repos resolve at once.
            let Ok(_permit) = sem.acquire_owned().await else {
                return Ok(Vec::new());
            };
            discover_one(&http, &repo).await
        });
    }

    let mut out = Vec::<SkillCandidate>::new();
    let mut rate_limited: Option<SkillError> = None;
    while let Some(res) = joinset.join_next().await {
        match res {
            Ok(Ok(mut list)) => out.append(&mut list),
            Ok(Err(SkillError::RateLimited(code))) => {
                rate_limited = Some(SkillError::RateLimited(code));
            }
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "skill discovery for one repo failed");
            }
            Err(e) => tracing::warn!(error = %e, "skill discovery task panicked"),
        }
    }

    if out.is_empty() {
        if let Some(e) = rate_limited {
            return Err(e);
        }
    }

    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out.dedup_by(|a, b| a.key.eq_ignore_ascii_case(&b.key));
    Ok(out)
}

async fn discover_one(
    http: &reqwest::Client,
    repo: &SkillRepo,
) -> Result<Vec<SkillCandidate>, SkillError> {
    let tree = resolve_repo_tree(http, &repo.owner, &repo.name, &repo.branch).await?;
    let skill_files: Vec<TreeEntry> = tree
        .entries
        .into_iter()
        .filter(|e| e.kind == EntryKind::Blob && path_is_skill_md(&e.path))
        .take(MAX_SKILLS_PER_REPO)
        .collect();

    // Fan-out SKILL.md fetches in parallel; serial-per-entry was the main
    // contributor to "feels blocked" on the Refresh button.
    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(SKILL_MD_FETCH_CONCURRENCY));
    let mut joinset: tokio::task::JoinSet<Option<SkillCandidate>> = tokio::task::JoinSet::new();
    for entry in skill_files {
        let http = http.clone();
        let owner = repo.owner.clone();
        let name = repo.name.clone();
        let branch = tree.branch.clone();
        let sem = sem.clone();
        joinset.spawn(async move {
            let _permit = sem.acquire_owned().await.ok()?;
            let directory = entry
                .path
                .strip_suffix("/SKILL.md")
                .or_else(|| entry.path.strip_suffix("/Skill.md"))
                .or_else(|| entry.path.strip_suffix("/skill.md"))
                .map_or_else(|| name.clone(), ToString::to_string);

            let body = fetch_raw(&http, &owner, &name, &branch, &entry.path)
                .await
                .ok();
            let metadata = body.as_ref().map_or_else(
                || SkillMetadata {
                    name: directory.clone(),
                    description: String::new(),
                },
                |b| read_skill_metadata(std::str::from_utf8(b).unwrap_or(""), &directory),
            );

            Some(SkillCandidate {
                key: format!("{owner}/{name}:{directory}"),
                name: metadata.name,
                description: metadata.description,
                directory: directory.clone(),
                readme_url: format!(
                    "https://github.com/{owner}/{name}/blob/{branch}/{}",
                    entry
                        .path
                        .split('/')
                        .map(url_segment)
                        .collect::<Vec<_>>()
                        .join("/"),
                ),
                repo_owner: owner,
                repo_name: name,
                repo_branch: branch,
            })
        });
    }

    let mut out = Vec::new();
    while let Some(res) = joinset.join_next().await {
        if let Ok(Some(c)) = res {
            out.push(c);
        }
    }
    Ok(out)
}

const SKILL_MD_FETCH_CONCURRENCY: usize = 8;

fn path_is_skill_md(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower == "skill.md" || lower.ends_with("/skill.md")
}

#[derive(Debug)]
pub struct SkillMetadata {
    pub name: String,
    pub description: String,
}

pub fn read_skill_metadata(markdown: &str, fallback_name: &str) -> SkillMetadata {
    let frontmatter_block = extract_frontmatter(markdown);
    let source = frontmatter_block.unwrap_or(markdown);
    let name = extract_field(source, "name").unwrap_or_else(|| fallback_name.to_string());
    let description = extract_field(source, "description").unwrap_or_default();
    SkillMetadata { name, description }
}

fn extract_frontmatter(markdown: &str) -> Option<&str> {
    let s = markdown.trim_start_matches('\u{feff}');
    let s = s.strip_prefix("---")?;
    let s = s.strip_prefix('\n').or_else(|| s.strip_prefix("\r\n"))?;
    let end = s.find("\n---")?;
    s.get(..end)
}

fn extract_field(source: &str, field: &str) -> Option<String> {
    for line in source.lines() {
        let trimmed = line.trim_start();
        let head = format!("{field}:");
        if let Some(rest) = trimmed.strip_prefix(&head) {
            let value = rest.trim().trim_matches(|c| c == '"' || c == '\'');
            if value.is_empty() {
                return None;
            }
            return Some(value.to_string());
        }
    }
    None
}

fn url_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                use std::fmt::Write as _;
                // write! into a String never fails — explicit discard for lints.
                let _ignored = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "test ergonomics")]
    use super::*;

    #[test]
    fn parses_frontmatter_name_and_description() {
        let md = "---\nname: My Skill\ndescription: \"Does the thing.\"\n---\n# Body\n";
        let m = read_skill_metadata(md, "fallback");
        assert_eq!(m.name, "My Skill");
        assert_eq!(m.description, "Does the thing.");
    }

    #[test]
    fn falls_back_when_no_frontmatter() {
        let m = read_skill_metadata("just some markdown", "my-skill");
        assert_eq!(m.name, "my-skill");
        assert_eq!(m.description, "");
    }

    #[test]
    fn ignores_single_quoted_values_around_field() {
        let md = "---\nname: 'wrapped'\ndescription: no quotes\n---\n";
        let m = read_skill_metadata(md, "fallback");
        assert_eq!(m.name, "wrapped");
        assert_eq!(m.description, "no quotes");
    }

    #[test]
    fn url_segment_percent_encodes_specials() {
        assert_eq!(url_segment("hello world"), "hello%20world");
        assert_eq!(url_segment("a/b"), "a%2Fb");
    }

    #[test]
    fn fingerprint_is_stable_across_input_order() {
        let a = vec![
            SkillRepo {
                owner: "anthropics".into(),
                name: "skills".into(),
                branch: "main".into(),
                enabled: true,
            },
            SkillRepo {
                owner: "ComposioHQ".into(),
                name: "awesome-claude-skills".into(),
                branch: "master".into(),
                enabled: true,
            },
        ];
        let mut b = a.clone();
        b.reverse();
        assert_eq!(fingerprint(&a), fingerprint(&b));
    }

    #[test]
    fn detects_skill_md_anywhere() {
        assert!(path_is_skill_md("SKILL.md"));
        assert!(path_is_skill_md("foo/SKILL.md"));
        assert!(path_is_skill_md("foo/bar/Skill.md"));
        assert!(!path_is_skill_md("foo/SKILL.markdown"));
        assert!(!path_is_skill_md("readme.md"));
    }
}
