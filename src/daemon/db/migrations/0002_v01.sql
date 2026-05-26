-- 0002_v01.sql — v0.1 schema additions. Idempotent statements only.
-- ALTER TABLE ADD COLUMN work happens in Rust via ensure_column, not here.

INSERT OR IGNORE INTO schema_version (version) VALUES (2);

CREATE TABLE IF NOT EXISTS server_tools (
    server        TEXT NOT NULL,
    tool          TEXT NOT NULL,
    description   TEXT,
    input_schema  TEXT NOT NULL DEFAULT '{}',
    discovered_at INTEGER NOT NULL,
    PRIMARY KEY (server, tool),
    FOREIGN KEY (server) REFERENCES servers(name) ON UPDATE CASCADE ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_server_tools_server ON server_tools(server);

CREATE TABLE IF NOT EXISTS settings_kv (
    key   TEXT NOT NULL PRIMARY KEY,
    value TEXT NOT NULL
);
INSERT OR IGNORE INTO settings_kv (key, value) VALUES
  ('default_unknown_tool_policy', 'ask'),
  ('open_window_at_startup',      'false'),
  ('routing_paused',              'false'),
  ('has_completed_first_run',     'false'),
  ('shown_background_hint',       'false');
