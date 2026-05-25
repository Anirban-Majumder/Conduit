-- v0 ships exactly one schema. Idempotent: every CREATE/INSERT uses
-- IF NOT EXISTS / OR IGNORE so the actor re-runs this on every startup.
-- Schema grows in-place across stages until v0.5+ introduces a migration runner.

CREATE TABLE IF NOT EXISTS schema_version (
    version INTEGER NOT NULL PRIMARY KEY
);
INSERT OR IGNORE INTO schema_version (version) VALUES (1);

CREATE TABLE IF NOT EXISTS ide_keys (
    key        TEXT NOT NULL PRIMARY KEY,
    ide_name   TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_ide_keys_ide_name ON ide_keys(ide_name);

CREATE TABLE IF NOT EXISTS servers (
    name                  TEXT NOT NULL PRIMARY KEY,
    transport             TEXT NOT NULL,        -- 'stdio' | 'http'
    command               TEXT,
    args_json             TEXT NOT NULL DEFAULT '[]',
    env_json              TEXT NOT NULL DEFAULT '{}',
    url                   TEXT,
    lifecycle_seconds     INTEGER NOT NULL DEFAULT 300,
    last_init_duration_ms INTEGER,
    package_version       TEXT,
    mcp_self_version      TEXT,
    created_at            INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS policies (
    id              INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    server          TEXT NOT NULL,
    tool            TEXT NOT NULL,
    decision        TEXT NOT NULL,        -- 'allow' | 'deny' | 'ask'
    decision_source TEXT NOT NULL,        -- 'default' | 'user' | 'tool_list_changed'
    created_at      INTEGER NOT NULL,
    UNIQUE(server, tool)
);
CREATE INDEX IF NOT EXISTS idx_policies_server ON policies(server);

CREATE TABLE IF NOT EXISTS pending_approvals (
    id         TEXT NOT NULL PRIMARY KEY,
    server     TEXT NOT NULL,
    tool       TEXT NOT NULL,
    args_json  TEXT NOT NULL,
    ide_key    TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    status     TEXT NOT NULL              -- 'pending' | 'approved' | 'denied' | 'timeout'
);
CREATE INDEX IF NOT EXISTS idx_pending_approvals_status ON pending_approvals(status);

CREATE TABLE IF NOT EXISTS audit_log (
    id              INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    ts              INTEGER NOT NULL,
    ide_key         TEXT,
    ide_name        TEXT,
    server          TEXT NOT NULL,
    tool            TEXT,
    decision        TEXT NOT NULL,        -- allow/deny/ask-approved/ask-denied/ask-timeout/error/cancelled
    decision_source TEXT,
    latency_ms      INTEGER,
    request_args    TEXT,
    response_status TEXT,
    response_length INTEGER,
    error_code      TEXT
);
CREATE INDEX IF NOT EXISTS idx_audit_log_ts ON audit_log(ts DESC);
CREATE INDEX IF NOT EXISTS idx_audit_log_server_tool ON audit_log(server, tool);
CREATE INDEX IF NOT EXISTS idx_audit_log_ide_key ON audit_log(ide_key);

CREATE TABLE IF NOT EXISTS managed_ide_configs (
    ide_name             TEXT NOT NULL,
    ide_key              TEXT NOT NULL,
    config_path          TEXT NOT NULL,
    managed_entries_json TEXT NOT NULL DEFAULT '[]',
    written_at           INTEGER NOT NULL,
    PRIMARY KEY (ide_name, ide_key)
);
