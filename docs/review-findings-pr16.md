# Review findings — PR #16 (data-driven client + repo-scope refactor)

Multi-agent review of branch `phase5b-repo-scope` vs `main`, 2026-06-02. Every
finding below was double-verified by adversarial skeptics; severities are the
post-verification (impact-adjusted) values. This file records what was **fixed**,
what is **deferred**, and what is **accepted** so the decisions aren't re-litigated.

## Fixed in this branch

- **A2 — projection now reconciles removed/renamed built-ins.** `project_builtins`
  gains `reconcile_orphaned_builtins`: a built-in `client` row whose `id` is no
  longer in `BUILTINS` (a def removed/renamed across an upgrade) is **untracked**
  (not deleted — preserves `mcp_key` + `managed_*` for a future rename-migration).
  Stops orphaned rows showing "active" and being live cascade/render targets.
  `src/daemon/client_config/project.rs`.
- **A3 — idempotent startup config reconcile.** New `reconcile_configs(db, port)`
  re-derives every tracked built-in's on-disk MCP config from current `managed_mcp`
  (global + each repo scope) at startup, repairing DB↔file divergence left by an
  interrupted cascade/toggle. Called from `push_initial_models` after detection.
  Made non-destructive by a **no-op-write guard** in `client_config/io.rs`
  (`write_atomic` skips byte-identical files) — so a consistent tree writes nothing
  and we don't churn/strip comments every boot. `project.rs`, `io.rs`, `refresh.rs`.
- **Optimizations (all confirmed N+1 / cosmetic):**
  - `query_list_skills` — one batched `SELECT DISTINCT skill_dir, client_id` +
    bucket, replacing one targets query per skill. `db/skills.rs`.
  - `resolve_scope_targets` — `list_clients()` once into a map, local lookup,
    replacing a `get_client` actor roundtrip per row. `skills/manager.rs`.
  - `refresh_policies` / `refresh_servers` — new batched `Db::list_all_tools()`
    replaces the per-server `list_server_tools` loop. `db/servers.rs`,
    `db/commands.rs`, `db/dispatch.rs`, `window/refresh.rs`.
  - `display_scope` — strip the Windows verbatim `\\?\` / `\\?\UNC\` prefix before
    matching `home_dir()`, so scope labels collapse to `~/…` on Windows.
    `window/util.rs`.

## Accepted risks (won't fix — deliberate)

- **Migration version collision (was "must fix").** New breaking schema is folded
  into `0001_init.sql` but kept at `MIGRATIONS` version 1; an existing on-disk
  `state.db` already records schema_version ≥ 1, so the new `INIT_SQL` is skipped
  on in-place upgrade → runtime `no such table: mcp_server`. **Accepted:** only two
  collaborators, who will delete their local `state.db` manually. Fresh installs are
  unaffected. `src/daemon/db/migrations.rs:27,100-108`.
- **A1 — repo-scope is write-time only.** Request routing + policy collapse all
  scopes into one per-client `mcp_key` (`/<key>/<server>/mcp`); the daemon can't tell
  which repo a call came from, and `resolve_policy(server, tool)` has no scope input.
  So scopes can't differ in policy/availability — "scope" is config-file placement,
  not an enforcement boundary. **Accepted** for v0. If scope ever needs to be an
  enforcement boundary, key routing/policy by `(client, scope)`.
  `src/daemon/downstream/http_server.rs`.
- **A4 — scope key = raw canonicalized path string.** A repo scope is identified by
  its canonical absolute path captured at add-time, used verbatim. Moving/renaming a
  scoped repo orphans its scope (re-add creates a second scope; the old config file at
  the dead path is unreachable for cleanup). **Accepted** for v0; revisit with a
  `repo` table (id + mutable path) if repos become first-class.
  `src/daemon/client_config/types.rs`.
- **A5 — `decision_source` provenance dropped.** Merging `policies` into
  `mcp_tool.decision` loses per-tool source; `refresh_policies` emits an empty
  `source`. Recoverable historically from the audit log; derivable live
  (`source = if decision IS NOT NULL then 'tool' else 'default'`). **Accepted.**
  `src/daemon/db/migrations/0001_init.sql`, `window/refresh.rs`.

## Deferred bugs (real, low/medium impact — single-user/localhost softens them)

Not fixed per scope decision. Listed so they're tracked, not lost.

| # | Sev | Bug | Location |
|---|-----|-----|----------|
| 1 | medium | Cascade server-delete never kills the running upstream; persistent (`-1`) child + grandchildren leak until daemon shutdown. `CallbackContext` has no `UpstreamManager` handle. Re-add same name reuses the stale entry. | `src/daemon/cascade.rs:24-74` |
| 2 | medium | Blocking keyring fetch on the async tool-call hot path — `server_to_config` calls sync `secrets::fetch` inline in `proxy.rs` (every `tools/list` + `tools/call`), no `spawn_blocking`, no cache. `oauth_store.rs` already does it right. | `src/daemon/downstream/http_server.rs:205-230` |
| 3 | low | `.mcp.json` collision: ClaudeCode + OpenClaude share the repo file, key entries by bare server name with different `mcp_key` → silent overwrite + audit mis-attribution if the same server is repo-scoped to both. | `src/client/{claude_code,openclaude}.rs:17` |
| 4 | low | JSONC comment loss: VS Code `mcp.json` re-emitted comment-less on render/purge. (The A3 no-op-write guard removes the *gratuitous* no-op rewrites; genuine content changes still drop comments.) | `src/daemon/client_config/render.rs` |
| 5 | low | Approval hold doesn't `select!` on the shared `CancellationToken` → ~5s shutdown delay + forced task abort instead of clean cancel. | `src/daemon/downstream/approval.rs:117-145` |
| 6 | low | Approval resolve-vs-timeout race: last-instant Approve can land while the IDE is told Timeout (audit/event mismatch). Safe direction — tool is **not** executed. `resolve_pending_approval` UPDATE lacks a `WHERE status='pending'` CAS guard. | `src/daemon/downstream/approval.rs:117-189` |
| 7 | low | Skills-tab `toggle-target` chain is dead code (IconToggle calls `open-scope` now); if ever re-wired it seeds from all-scope `targets` but writes `"global"` only → would promote a repo-only client to a spurious global row. `Db::set_skill_targets` still used by `install()`. | `src/window/skills.rs:129-156` |

Rejected on verification (non-issues): "global skill reconcile mirrors into untracked
editors"; "atomic_rename has no fsync".
