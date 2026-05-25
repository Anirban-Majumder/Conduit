-- v0 ships exactly one schema. This file grows as modules land in subsequent
-- stages. Every CREATE/INSERT must remain idempotent (`IF NOT EXISTS` / `OR
-- IGNORE`) — the actor re-runs this on every startup.

CREATE TABLE IF NOT EXISTS schema_version (
    version INTEGER NOT NULL PRIMARY KEY
);

INSERT OR IGNORE INTO schema_version (version) VALUES (1);
