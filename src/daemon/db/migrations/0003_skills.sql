-- 0003_skills.sql — agent skills (TokenTracker-style sync to per-IDE skill dirs).
-- Idempotent statements only.

INSERT OR IGNORE INTO schema_version (version) VALUES (3);

CREATE TABLE IF NOT EXISTS skill_repos (
    owner   TEXT NOT NULL,
    name    TEXT NOT NULL,
    branch  TEXT NOT NULL DEFAULT 'main',
    enabled INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (owner, name)
);

-- Default discovery sources — same first two TokenTracker ships with. Seeded
-- exactly once (sentinel in settings_kv): re-running this migration on an
-- existing DB must not undo user deletions of the seed rows.
INSERT INTO skill_repos (owner, name, branch, enabled)
SELECT 'anthropics',  'skills',                'main',   1
WHERE NOT EXISTS (SELECT 1 FROM settings_kv WHERE key = 'skill_repos_seeded');
INSERT INTO skill_repos (owner, name, branch, enabled)
SELECT 'mattpocock',  'skills',                'main',   1
WHERE NOT EXISTS (SELECT 1 FROM settings_kv WHERE key = 'skill_repos_seeded');
INSERT OR IGNORE INTO settings_kv (key, value) VALUES ('skill_repos_seeded', '1');

CREATE TABLE IF NOT EXISTS skills (
    directory        TEXT NOT NULL PRIMARY KEY,
    repo_owner       TEXT NOT NULL,
    repo_name        TEXT NOT NULL,
    repo_branch      TEXT NOT NULL,
    source_directory TEXT NOT NULL,
    skill_name       TEXT NOT NULL,
    description      TEXT NOT NULL DEFAULT '',
    readme_url       TEXT,
    installed_at     INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_skills_repo ON skills(repo_owner, repo_name);

CREATE TABLE IF NOT EXISTS skill_targets (
    directory TEXT NOT NULL,
    ide_name  TEXT NOT NULL,
    PRIMARY KEY (directory, ide_name),
    FOREIGN KEY (directory) REFERENCES skills(directory) ON UPDATE CASCADE ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_skill_targets_dir ON skill_targets(directory);
