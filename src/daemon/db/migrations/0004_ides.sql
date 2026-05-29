-- 0004_ides.sql — adds the `ides` table: detected IDEs, their managed MCP
-- entries, and tracking status. Supersedes the older managed_ide_configs flow.
-- Idempotent statements only.

CREATE TABLE IF NOT EXISTS ides (
    ide_kind             TEXT NOT NULL PRIMARY KEY,
    display_name         TEXT NOT NULL,
    ide_key              TEXT NOT NULL UNIQUE,
    status               TEXT NOT NULL DEFAULT 'active',
    detection_source     TEXT,
    config_path          TEXT,
    managed_entries_json TEXT NOT NULL DEFAULT '[]',
    first_seen_at        INTEGER NOT NULL,
    last_seen_at         INTEGER NOT NULL,
    not_tracked_at       INTEGER
);
CREATE INDEX IF NOT EXISTS idx_ides_status ON ides(status);
CREATE INDEX IF NOT EXISTS idx_ides_ide_key ON ides(ide_key);

INSERT OR IGNORE INTO schema_version (version) VALUES (4);
