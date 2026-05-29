-- 0005_oauth_drafts.sql — adds the pending_oauth_drafts table.
-- Track in-flight OAuth draft sessions from the wizard.
-- Entries are deleted on wizard save (after promotion) or cleaned up at startup.
CREATE TABLE IF NOT EXISTS pending_oauth_drafts (
    draft_uuid TEXT PRIMARY KEY,
    created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
);
