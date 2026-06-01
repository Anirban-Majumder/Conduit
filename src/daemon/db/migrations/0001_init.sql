-- 0001_init.sql — Conduit complete schema (single compiled baseline).
-- `schema_version` and its bookkeeping are owned by the migration runner
-- (migrations.rs); this file only defines the v1 tables. Every statement is
-- idempotent (IF NOT EXISTS / INSERT OR IGNORE) so re-running is safe.

-- ── MCP servers & tools ───────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS mcp_server (
    name                  TEXT NOT NULL PRIMARY KEY,
    transport             TEXT NOT NULL,            -- 'stdio' | 'http'
    command               TEXT,
    args_json             TEXT NOT NULL DEFAULT '[]',
    env_json              TEXT NOT NULL DEFAULT '{}',
    url                   TEXT,
    lifecycle_seconds     INTEGER NOT NULL DEFAULT 300,
    reported_name         TEXT,                     -- upstream self-report (was servers.server_name)
    reported_version      TEXT,                     -- was servers.server_version
    last_probed_at        INTEGER,
    last_probe_error      TEXT,
    last_auth_error_msg   TEXT
);

-- merges server_tools (metadata) + policies(scope='tool') (decision)
CREATE TABLE IF NOT EXISTS mcp_tool (
    mcp_server      TEXT NOT NULL REFERENCES mcp_server(name) ON UPDATE CASCADE ON DELETE CASCADE,
    name            TEXT NOT NULL,
    description     TEXT,
    input_schema    TEXT NOT NULL DEFAULT '{}',
    discovered_at   INTEGER NOT NULL,               -- every row is a currently-advertised tool (delete-on-absent)
    decision        TEXT,                           -- NULL = inherit global; else 'allow'|'deny'|'ask'
    PRIMARY KEY (mcp_server, name)
);

-- ── Approvals & audit ─────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS pending_approvals (
    id          TEXT NOT NULL PRIMARY KEY,
    mcp_server  TEXT NOT NULL,
    tool        TEXT NOT NULL,
    args_json   TEXT NOT NULL,
    client_key  TEXT NOT NULL,                      -- was ide_key (= client.mcp_key, denormalized; no FK)
    created_at  INTEGER NOT NULL,
    status      TEXT NOT NULL                       -- 'pending'|'approved'|'denied'|'timeout'|'cancelled'
);
CREATE INDEX IF NOT EXISTS idx_pending_approvals_status ON pending_approvals(status);

CREATE TABLE IF NOT EXISTS audit_log (
    id              INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    ts              INTEGER NOT NULL,
    client_key      TEXT,                            -- was ide_key
    client_name     TEXT,                            -- denormalized point-in-time label (was ide_name)
    mcp_server      TEXT NOT NULL,                   -- was server
    tool            TEXT,
    decision        TEXT NOT NULL,
    decision_source TEXT,
    latency_ms      INTEGER,
    request_args    TEXT,
    response_status TEXT,
    response_length INTEGER,
    error_code      TEXT
);
CREATE INDEX IF NOT EXISTS idx_audit_log_ts ON audit_log(ts DESC);
CREATE INDEX IF NOT EXISTS idx_audit_log_server_tool ON audit_log(mcp_server, tool);
CREATE INDEX IF NOT EXISTS idx_audit_log_client_key ON audit_log(client_key);

-- ── Settings (created before the skill_repos seed reads its sentinel) ──
CREATE TABLE IF NOT EXISTS settings_kv (key TEXT NOT NULL PRIMARY KEY, value TEXT NOT NULL);
INSERT OR IGNORE INTO settings_kv (key, value) VALUES
  ('default_unknown_tool_policy', 'ask'),
  ('open_window_at_startup',      'false'),
  ('routing_paused',              'false'),
  ('has_completed_first_run',     'false'),
  ('shown_background_hint',       'false');

-- ── Skills (skills table precedes managed_skill's FK target) ──────────
CREATE TABLE IF NOT EXISTS skill_repos (
    owner   TEXT NOT NULL,
    name    TEXT NOT NULL,
    branch  TEXT NOT NULL DEFAULT 'main',
    enabled INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (owner, name)
);

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

INSERT INTO skill_repos (owner, name, branch, enabled)
SELECT 'anthropics', 'skills', 'main', 1
WHERE NOT EXISTS (SELECT 1 FROM settings_kv WHERE key = 'skill_repos_seeded');
INSERT INTO skill_repos (owner, name, branch, enabled)
SELECT 'mattpocock', 'skills', 'main', 1
WHERE NOT EXISTS (SELECT 1 FROM settings_kv WHERE key = 'skill_repos_seeded');
INSERT OR IGNORE INTO settings_kv (key, value) VALUES ('skill_repos_seeded', '1');

-- ── Clients (editors) — single merged table ───────────────────────────
CREATE TABLE IF NOT EXISTS client (
    id                 TEXT NOT NULL PRIMARY KEY,    -- built-in: code id ("Cursor"); custom: random uid
    mcp_key            TEXT NOT NULL UNIQUE,         -- URL routing token  …/<mcp_key>/<server>/mcp
    tracked            INTEGER NOT NULL DEFAULT 1,   -- user intent; 0 = "never manage this editor"
    is_builtin         INTEGER NOT NULL,             -- discriminant: 1 = projected from code, 0 = custom
    first_seen_at      INTEGER NOT NULL,
    last_seen_at       INTEGER NOT NULL,
    -- identity (projected for built-ins / authored for custom) ─────────
    name               TEXT NOT NULL,                -- display label
    mcp_config_path    TEXT NOT NULL,                -- absolute global config path (OS-resolved)
    skills_dir         TEXT,                          -- absolute global skills dir (NULL = no skills support)
    schema_json        TEXT NOT NULL                  -- serialized ConfigSchema {format,key_path,entry_template}
);

CREATE TABLE IF NOT EXISTS managed_mcp (
    client_id   TEXT NOT NULL REFERENCES client(id) ON DELETE CASCADE,
    scope       TEXT NOT NULL DEFAULT 'global',       -- 'global' | absolute project dir
    server_name TEXT NOT NULL REFERENCES mcp_server(name) ON UPDATE CASCADE ON DELETE CASCADE,
    PRIMARY KEY (client_id, scope, server_name)
);
CREATE INDEX IF NOT EXISTS idx_managed_mcp_server ON managed_mcp(server_name);

CREATE TABLE IF NOT EXISTS managed_skill (                          -- replaces skill_targets; scope-aware
    client_id TEXT NOT NULL REFERENCES client(id) ON DELETE CASCADE,
    scope     TEXT NOT NULL DEFAULT 'global',
    skill_dir TEXT NOT NULL REFERENCES skills(directory) ON UPDATE CASCADE ON DELETE CASCADE,
    PRIMARY KEY (client_id, scope, skill_dir)
);
CREATE INDEX IF NOT EXISTS idx_managed_skill_dir ON managed_skill(skill_dir);

CREATE TABLE IF NOT EXISTS pending_oauth_drafts (
    draft_uuid TEXT PRIMARY KEY,
    created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
);
