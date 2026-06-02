# Phase 5b — Repo-local config (directory-scope) + scoped skills

Status: **planned, not started; reviewed (3-agent pass) and corrected.** Builds on Phases
1–4 (complete) and the src restructure. Sibling to the deferred custom-client work (Phase
5a, NOT in scope here). Supersedes the one-line "Phase 5" bullet in [db-reworkv2.md](db-reworkv2.md#L755).

> **Review corrections (must-fix, folded into the sections below):** (1) **Cascade** is a
> real rewrite — today it de-dups by `client_id` and discards scope, so repo files go stale
> on server delete (§6). (2) **Projection backfill** — `0002`'s NULL columns won't fill
> unless re-projection is forced; clear `client_def_version` in `0002` (§3c). (3) **Skills
> sync must go pure-DB** — drop the `builtin_by_id` reach, drive from
> `ResolvedClient::skills_target` (§7). (4) **Empty-repo litter** — delete ours-created
> file/dir on last removal (§5). (5) **Editors-tab TOML** — the raw editor is JSON-only;
> Codex repo edit breaks (§8b). (6) Validate paths on **add only**, on the worker via
> `spawn_blocking` (§3e/§8d).

## 0. The feature

Most IDEs read a **repo-local** config when opened inside a project directory, in
addition to the global/user config. Conduit should let a user push an MCP server (or a
skill) into **one specific repo** for **one specific editor**, without touching the
global install — writing/merging that editor's project-local file in the chosen folder.

Flow: in a Server's (or Skill's) detail, click an editor's icon → a **scope popover** →
toggle **Global** and/or any **repo**, add a new repo by **typing its path**, remove a
single scope (✕), or **Remove from all**. The **Editors tab** gains a scope selector to
view/edit each scope's actual config file like the global one.

## 1. Locked decisions (from the design grill)

| # | Decision |
|---|---|
| L1 | Scope of this phase = **directory-scope (B) + scoped skills sync (C)**. Custom clients (5a) deferred. |
| L2 | **Research gate cleared:** all 6 built-ins support repo-local MCP; 5/6 support repo-local skills (NOT VS Code Copilot). For every IDE the **repo config format == the global format** (same key_path + entry schema) — only the file *path* differs. |
| L3 | Repo-relative paths stored as **two projected NULL-able columns on `client`** (`project_config_rel`, `project_skills_rel`), filled from new `ClientDef` fields by `project_builtins`, version-stamped — same model as the existing identity columns. NOT code-only, NOT a JSON blob. |
| L4 | **No `project` registry table.** Targeting reuses the existing `managed_mcp.scope` / `managed_skill.scope` columns (`'global'` \| normalized absolute repo dir). A repo "exists" iff it has ≥1 managed row; `SELECT DISTINCT scope` enumerates them. |
| L5 | The renderer is **unchanged in substance** — `render_servers`/`purge_mcp` already take an explicit path. Only the target-path resolution is new (`global` → `config_path`; repo → `scope_dir/project_config_rel`). |
| L6 | **URL routing token stays per-client.** A repo-scoped config points at the same `localhost:<port>/<mcp_key>/<server>/mcp` as global. Directory-scope is purely *which file we write*, never routing. |
| L7 | **Skill sync mode is a global `config.toml` setting** (`symlink` \| `copy`), default `symlink`, applied to both global and repo sync. |
| L8 | **No native folder dialog / no new crate.** Repo path is a **typed text field**, validated (canonicalized, must be an existing directory). |
| L9 | Two UI surfaces: **scope popover** (add/manage, shared by Server + Skill detail) and **Editors-tab scope selector** (view/edit existing scopes' files). No separate "Projects" tab. Paths shown home-relative (`~/work/foo`). |
| L10 | One `normalize_scope_path()` helper is the single writer/reader of every scope string. |

## 2. Module remap (the original plan predates the restructure)

| Original plan path | Current path |
|---|---|
| `clients/mod.rs` (ClientDef, BUILTINS) | `daemon/client_config/mod.rs` |
| `clients/<ide>.rs` (the DEFs) | `client/<ide>.rs` |
| `clients/schema.rs` + `resolved.rs` | `daemon/client_config/types.rs` (merged) |
| `clients/render.rs` / `project.rs` / `paths.rs` | `daemon/client_config/{render,project,paths}.rs` |
| `db/mod.rs` dispatch arms | `daemon/db/dispatch.rs` (split out) |
| `config_toml.rs` | `daemon/config/toml.rs` |
| `glue/callbacks_ide.rs` | `window/callbacks/ide.rs` |
| `glue/{refresh,util,skills}.rs` | `window/{refresh,util,skills}.rs` |
| `glue/callbacks_servers.rs` | `window/callbacks/servers.rs` |
| `skills/mod.rs` (SkillManager) | `daemon/skills/manager.rs` |
| `ui/*.slint` | `slint/*.slint` |

## 3. Data layer

### 3a. Migration `0002` — two columns, nothing else
New file `daemon/db/migrations/0002_repo_scope.sql`; append `(2, INCLUDE_STR!)` to the
`MIGRATIONS` slice in `daemon/db/migrations.rs` (the forward seam kept in Phase 1). The
runner applies it after `0001` on existing DBs and `foreign_key_check`s.

```sql
-- 0002_repo_scope.sql — per-client project-relative config + skills paths.
ALTER TABLE client ADD COLUMN project_config_rel TEXT;  -- repo-root-relative; NULL = no repo MCP
ALTER TABLE client ADD COLUMN project_skills_rel TEXT;  -- repo-root-relative; NULL = no repo skills
```
`managed_mcp`/`managed_skill` need **no change** — `scope` is already in their PK.

### 3b. `ClientDef` + per-IDE values
Add to `ClientDef` (`daemon/client_config/mod.rs`):
```rust
pub project_config_rel: Option<&'static str>,
pub project_skills_rel: Option<&'static str>,
```
Fill each `client/<ide>.rs` `static DEF` (research-verified):

| IDE | `project_config_rel` | `project_skills_rel` |
|---|---|---|
| Claude Code | `.mcp.json` | `.claude/skills` |
| Cursor | `.cursor/mcp.json` | `.cursor/skills` |
| VS Code Copilot | `.vscode/mcp.json` | `None` |
| Codex | `.codex/config.toml` | `.codex/skills` |
| OpenCode | `.opencode/opencode.json` | `.opencode/skills` |
| OpenClaude | `.mcp.json` | `.openclaude/skills` |

### 3c. Projection (`daemon/client_config/project.rs`)
`project_builtins` writes the two new columns alongside the existing identity columns.
`query_project_builtin` (`db/clients.rs`) + its `DbCmd` `ProjectBuiltin` (`db/commands.rs`)
+ the `dispatch.rs` arm gain the two params.

**Backfill trap (must-fix).** `0002` adds the columns as NULL on already-projected rows;
`project_builtins` only re-projects when `client_def_version != CARGO_PKG_VERSION`. If 5b
ships **without** a version bump, the gate doesn't fire → columns stay NULL → every repo
write silently no-ops (`config_target(Repo)` → `None`). Guarantee the backfill: have
**`0002` clear `settings_kv['client_def_version']`** so the next startup projection is
forced (preferred — can't be forgotten), or make a version bump a documented release
invariant. Test: ALTER an old row's new columns to NULL, run startup, assert filled.

### 3d. `ResolvedClient` + target-path helpers (`daemon/client_config/types.rs`)
```rust
pub struct ResolvedClient {
    // ...existing...
    pub project_config_rel: Option<PathBuf>,   // relative
    pub project_skills_rel: Option<PathBuf>,   // relative
}
impl ResolvedClient {
    /// Absolute config file to write for a scope. None = client has no repo support.
    pub fn config_target(&self, scope: &Scope) -> Option<PathBuf> {
        match scope {
            Scope::Global => Some(self.config_path.clone()),
            Scope::Repo(dir) => self.project_config_rel.as_ref().map(|r| dir.join(r)),
        }
    }
    /// Absolute skills dir to mirror into for a scope.
    pub fn skills_target(&self, scope: &Scope) -> Option<PathBuf> { /* skills_dir | dir.join(project_skills_rel) */ }
}
```
`from_row` parses the two columns. A new tiny enum lives beside it:
```rust
pub enum Scope { Global, Repo(PathBuf) }   // serialized to/from the scope TEXT column
impl Scope {
    pub fn from_db(s: &str) -> Self;        // "global" => Global else Repo(PathBuf::from(s))
    pub fn as_db(&self) -> String;          // Global => "global"; Repo(p) => p.to_string()
}
```

### 3e. `normalize_scope_path` (L10) — validate on ADD only
One free fn (in `daemon/client_config/types.rs` or `daemon/paths.rs`): expand `~`,
canonicalize to an absolute path, strip trailing separator, native separators; `Err(invalid)`
if the path doesn't exist / isn't a dir. **Run it ONLY when the user types a new repo path
(add).** It stores the canonical string; thereafter toggle/remove/cascade operate on the
**stored string verbatim** (`Scope::from_db` does no fs I/O) — else a repo since deleted/moved
would fail `canonicalize` and could never be un-scoped. The popover's "is this item in this
repo" check compares stored-canonical to stored-canonical. (macOS `/var`→`/private/var`,
Windows `\\?\` — fine as long as you only ever key off the canonical form.)

## 4. DB queries + `DbCmd`s — thread `scope`

All in `daemon/db/{clients,skills}.rs` + a variant in `commands.rs` + an arm in
`dispatch.rs` + a `Db::` wrapper. **Drop the hardcoded `'global'`**, take a `scope: &str`:

- `query_list_managed_servers(conn, client_id, scope)` — servers for one (client, scope).
- `query_set_managed_servers(conn, client_id, scope, names)` — replace the set for one scope.
- `query_add_managed_server` / `query_remove_managed_server(conn, client_id, scope, server)` — single-row toggle (needed for the popover; cheaper than set-replace).
- `query_list_clients_managing_server(conn, server)` → **already** returns `(client_id, scope)` (Phase 1). Cascade consumes it as-is.
- Skills mirror: `add_skill_target` / `remove_skill_target` / `list_skill_targets` gain `scope`.
- **New:** `query_list_known_repos(conn) -> Vec<String>` = `SELECT DISTINCT scope FROM managed_mcp WHERE scope!='global' UNION SELECT DISTINCT scope FROM managed_skill WHERE scope!='global'`. Feeds the popover + Editors selector.
- **New:** `query_list_scopes_for_client(conn, client_id)` = scopes (incl. global) this client has any managed row in. Feeds the Editors-tab scope selector.
- **New:** `query_remove_server_all_scopes(conn, client_id, server)` and skill equivalent — backs "Remove from all".

Net: ~4 signatures gain a `scope` param, ~4 new queries. Each is the standard 3-site edit
(variant + dispatch arm + `Db::` wrapper).

## 5. Renderer (`daemon/client_config/render.rs`)

Add one thin scoped entry; the existing `render_servers`/`purge_mcp`/`purge_one` are
unchanged (they already take a path):
```rust
pub fn render_scope(client: &ResolvedClient, scope: &Scope, servers: &[String], port: u16)
    -> Result<(), ClientError> {
    let Some(path) = client.config_target(scope) else { return Ok(()); }; // client has no repo cfg
    purge_mcp(&client.schema, &path, &client.mcp_key, port)?;
    render_servers(&client.schema, &path, &client.mcp_key, servers, port)
}
```
Existing `render_mcp(client, servers, port)` becomes `render_scope(client, &Scope::Global, …)`.
**Re-derive rule:** after any managed_mcp change for `(client, scope)`, re-query that
scope's server set and `render_scope`.

**Empty-repo cleanup (differs from global!).** `atomic_write_*` does `create_dir_all(parent)`
then write — so `render_scope(repo, [])` after removing the last entry would *create*
`<repo>/.cursor/mcp.json` **and** the `.cursor/` dir, littering the user's repo. Global's
"leave the file" is only safe because the global file/dir pre-exist. For repo scope: after
purge, **if no entries remain (ours or the user's) AND Conduit created the file, delete it;
remove the parent dir if Conduit created it and it's now empty.** Capture an
"existed-before-first-write" signal to know what's ours. Never delete a file/dir that held
user content. `render_scope` also `debug_assert!`s/traces if `config_target` is `None` so a
future built-in with a missing rel-path isn't silently dropped.

## 6. Cascade (`daemon/cascade.rs`) — REAL REWRITE

**Today cascade de-dups affected clients by `client_id` and discards scope**
(`cascade.rs:~55` `seen.insert(client_id)`), then re-renders via the global-hardcoded
`list_managed_servers` + `render_mcp` → repo files are **never re-derived on server delete**
(a deleted server's entry persists in every repo file). The rewrite:
1. `list_clients_managing_server(doomed)` → `[(client_id, scope)]` (snapshot **before** the
   delete — already correct; the timing is fine, the scope-collapse is the bug).
2. **Drop the `client_id` de-dup**; iterate each `(client_id, scope)` *pair* (de-dup on the pair).
3. Per pair: `resolve_client(client_id)`, compute remaining via the **scope-aware**
   `list_managed_servers(client_id, scope)` (§4), then `render_scope(client,
   &Scope::from_db(&scope), remaining, port)`.
FK `ON DELETE CASCADE` still drops the rows; we re-derive each affected file (global + every
repo). Budget this as logic, not a swap. (Server delete does **not** touch skills.)

## 7. Skills — scoped sync + `sync_mode` (B+C) — PURE-DB REWRITE

- **Targeting:** `managed_skill` scope-aware via the §4 scoped add/remove-target queries.
  Today `set_targets`/`add`/`remove` take a flat `Vec<client_id>` (global only) — they change
  to `(client_id, scope)` pairs, not just gain a param.
- **Sync must become pure-DB (D5).** Today `sync::sync_to_target(source, client_id)` /
  `remove_from_target` resolve the destination internally via `builtin_by_id(client_id)` →
  `paths::resolve(...)`, and `apply_targets_blocking` iterates `skill_capable_builtins()` — an
  IDE-module reach **D5 forbids**. Rewrite so the **caller** resolves
  `ResolvedClient::skills_target(scope)` (global skills_dir, or `scope_dir/project_skills_rel`)
  and passes the absolute dir + `install_name` + `mode`; `sync.rs` no longer imports
  `client_config::builtin_by_id`/`skill_capable_builtins`. Drive the target list from the DB
  `(client_id, scope)` rows, exactly like the MCP cascade. Clients with `project_skills_rel ==
  None` (VS Code) are skipped for repo scope.
- **`sync_mode`:** read **fresh per sync** from the current `Settings` (config.toml is
  watched/reloaded — do NOT cache it at `SkillManager::new`, or edits won't apply until
  restart). `symlink` = try symlink, fall back to copy (today's behavior). `copy` = always
  recursive copy. Applies to global and repo alike.
- **Re-sync trigger:** toggling a skill for `(client, scope)` re-mirrors just that skill to
  that scope. **Uninstall REMOVES the skill from every `(client, scope)`** it's mirrored into
  (not "re-mirror" — re-mirroring a being-deleted skill would recreate files). Skill-update
  re-mirrors each existing scope.

## 8. UI

### 8a. Scope popover — `slint/components/scope_popover.slint` (shared)
Opened by clicking an editor icon in **Server detail** (`tabs/servers.slint`) and **Skill
detail** (`tabs/skills.slint`). Contents, driven by a Rust-owned model:
```
<Editor name>
  [✓] Global
  [✓] ~/work/foo        ✕
  [ ] ~/dev/bar         ✕
  ┌ add repo: [ type a path…            ] [Add]   ← validates on Add
  [ Remove from all ]
```
- Repo rows = `query_list_known_repos()`, each a toggle reflecting whether *this
  server/skill* is in that repo for *this editor*; ✕ removes that one scope.
- "Add" validates the typed path (`normalize_scope_path`); on success inserts the managed
  row at the new scope + re-renders. Invalid path → inline error, no row added.
- "Remove from all" → `remove_*_all_scopes(client, item)` + re-derive every affected file.
- New Slint structs: `ScopeRow { scope_label, scope_key, active: bool }`; popover
  in/out props (`popover_editor`, `popover_item_kind`, `popover_scopes: [ScopeRow]`,
  `popover_add_path`, `popover_add_error`).
- New callbacks (wired in `window/callbacks/servers.rs` + `window/skills.rs`):
  `toggle_scope(item, editor_id, scope_key, on)`, `add_repo_scope(item, editor_id, path)`,
  `remove_scope(item, editor_id, scope_key)`, `remove_all_scopes(item, editor_id)`.
- **Per-editor capability filter:** in the **skill** popover, hide all repo rows for an editor
  whose `project_skills_rel` is `None` (VS Code → Global only); in the **server** popover all 6
  have `project_config_rel`, so repos always show. Never show a repo toggle that can't be written.
- **Click-semantics change (intended):** today the editor icon one-click-toggles global
  (`servers.slint` `toggle-target`). The popover replaces that fast path — pre-select/focus
  **Global** so the common case stays ~one interaction.

### 8b. Editors tab — scope selector + file view/edit (`tabs/editors.slint`, `window/callbacks/ide.rs`)
The editor detail already shows settings + a raw config editor (read/validate/save/reload
on a path). Add a **scope selector** (`Global | ~/work/foo | …` = `list_scopes_for_client`).
Selecting a scope swaps the file path the existing raw editor reads/writes:
`client.config_target(scope)`. `read_raw`/reveal are format-agnostic; only the resolved path
changes. **No add here** (L9).
**Format trap:** `validate_json`/`write_raw` are JSON-only (`serde_json`), but Codex repo (and
global) config is **TOML** — selecting a TOML scope and saving fails validation. Either
format-switch validation on `client.schema.format` (validate TOML for Codex), or disable Save
and mark Codex raw-edit unsupported. (Global Codex has this latent bug today — fix or scope it
out explicitly.)
**Stale buffer:** a popover toggle re-renders the file the Editors tab may have open with
unsaved edits → mark the buffer dirty / prompt reload when its scope's file is rewritten
externally. New props: `editor_scopes: [ScopeOption]`, `selected_scope`; new callback
`select_editor_scope(scope_key)`.

### 8c. Painting (`window/refresh.rs`)
`refresh_server_detail` / skill detail populate `popover_scopes` when an editor icon is
clicked; the Editors detail populates `editor_scopes`. Home-relative display via a
`display_scope(path)` helper (`window/util.rs`): `Global` or `~`-collapsed path.

### 8d. Path entry + validation (L8)
No `rfd`. The "add repo" field is a Slint `LineEdit`. **Thread placement:**
`normalize_scope_path` does blocking `std::fs::canonicalize` — it must run inside the
callback's `rt.spawn(async move {…})` body, wrapped in `tokio::task::spawn_blocking` (never
on the Slint main thread), with the result/inline-error pushed back via
`upgrade_in_event_loop`. Reuses the existing inline-error pattern. Optional UX: warn
(non-blocking) if the chosen dir has no `.git` and isn't a repo root — IDEs walk up to the git
root, so a typed subdir yields a file the IDE won't read.

## 9. `config.toml` setting (`daemon/config/toml.rs`)

```toml
[skills]
sync_mode = "symlink"   # "symlink" | "copy"
```
```rust
#[derive(Deserialize, Default)] #[serde(rename_all = "snake_case")]
pub struct SkillsSettings { #[serde(default)] pub sync_mode: SyncMode }
#[derive(Deserialize, Default, Clone, Copy)] #[serde(rename_all = "lowercase")]
pub enum SyncMode { #[default] Symlink, Copy }
```
Add `skills: SkillsSettings` to `Settings` with `#[serde(default)]` (absent file still
valid). `SkillManager` reads it when mirroring. Surface read-only in the Settings tab's
config.toml mirror (it's `config.toml`-authoritative, not DB — matches existing pattern).

## 10. Phasing (each step ends `cargo check` green)

1. **Data** — `0002` migration + seam entry; `ClientDef` + 6 DEFs; `project_builtins` +
   `query_project_builtin`/`ProjectBuiltin`; `ResolvedClient` fields + `Scope` +
   `config_target`/`skills_target`; `normalize_scope_path`. *(verify: projection test asserts
   the columns; a `Scope` round-trip unit test.)*
2. **DB queries** — scope-thread the managed_mcp/managed_skill queries; new
   known-repos / scopes-for-client / remove-all queries; DbCmds + dispatch + wrappers.
   *(verify: `tests/clients_scope.rs` — set/list/remove at a repo scope; known-repos DISTINCT.)*
3. **Renderer + cascade** — `render_scope`; cascade re-derives per `(client, scope)`.
   *(verify: `tests/client_writer.rs` — repo path write merges + purges; cascade test covers a repo scope.)*
4. **Skills** — scope-aware targets + sync; `sync_mode` setting. *(verify: `tests/skills_scope.rs`
   — managed_skill at a repo scope; copy vs symlink honored; VS-Code-repo skipped.)*
5. **UI** — scope popover (server + skill), Editors scope selector, Slint structs +
   callbacks, path validation. *(verify: `cargo run`; manual e2e — add server to a repo for
   Claude, confirm `<repo>/.mcp.json` written + global untouched; remove-from-all clears all
   files; Editors tab edits the repo file.)*
6. **Green** — `cargo clippy --all-targets -- -D warnings`, `cargo fmt`, `cargo test`.

## 11. Tests (new / changed)

- **New:** `tests/clients_scope.rs` (scoped managed_mcp CRUD + known-repos DISTINCT/UNION),
  `tests/skills_scope.rs` (scoped managed_skill + `sync_mode=copy` vs `symlink` + VS-Code-repo
  skipped), `Scope` round-trip + `normalize_scope_path` units (`~`/trailing-slash/non-existent-errors).
- **Extend:** `tests/client_writer.rs` (repo write/merge/purge **for each format incl. Codex
  TOML** + **empty-repo cleanup**: last-removal deletes an ours-created file/dir),
  `tests/cascade.rs` (**red-first**: a doomed server scoped to global **+ a repo** re-derives
  **both** — fails against today's scope-collapsing cascade), `tests/config_toml.rs` (`[skills]
  sync_mode` default + parse), **projection backfill test** (NULL the new columns on an old row
  → startup fills them).
- **Unchanged:** routing/proxy (L6 — repo doesn't touch the URL), approvals, audit, probe.

## 12. Non-goals & accepted limitations (this phase)

- No `project` registry / labels / empty repos (L4) — revisit only if a repo-centric view is wanted.
- No native folder dialog (L8). No per-repo routing key (L6). No custom clients (5a).
- Codex project-`trust` handling and same-name merge precedence: out of scope (caveats dropped per decision).
- **Discoverability:** the only way to see where an item is scoped is its per-editor popover
  (no global "server X is in 3 repos" view). Accepted for v0; the item detail could later list
  all `(editor, scope)` for it.
- **Repo GC:** a repo leaves the known-repos list only when its last managed row anywhere is
  removed; there is no explicit "forget this repo" control. "Remove from all" is the GC.
