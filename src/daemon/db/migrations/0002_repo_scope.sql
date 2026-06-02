-- 0002_repo_scope.sql — per-client project-relative config + skills paths.
--
-- Phase 5b directory-scope: two NULL-able columns on `client` holding the
-- repo-root-relative MCP config / skills paths, projected from `ClientDef`.
-- `managed_mcp`/`managed_skill` already carry `scope` in their PK, so they
-- need no change. Clearing `client_def_version` forces the next startup
-- projection to re-fill these columns on rows that predate this migration —
-- otherwise they stay NULL and every repo write silently no-ops.
ALTER TABLE client ADD COLUMN project_config_rel TEXT;  -- repo-root-relative; NULL = no repo MCP
ALTER TABLE client ADD COLUMN project_skills_rel TEXT;  -- repo-root-relative; NULL = no repo skills
DELETE FROM settings_kv WHERE key = 'client_def_version';
