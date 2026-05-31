# DB + Client/Editor Rework — Implementation Plan

Status: **LOCKED — ready for Phase 1.** Schema + types are the lockable contract; the build
proceeds in phases against this fixed target. Conduit is pre-v0 with **no real user data**, so
this is a clean reset: local `state.db` is wiped on first run of the new schema, and there is
**no back-compat / migration-shim code** for the prior schema.

## Locked decisions

| # | Decision |
|---|---|
| D1 | Build the NEW features now (directory `scope` + custom clients), not deferred. |
| D2 | TDD the three logic rewrites (cascade, policy resolver, skill resync) against the new schema; they gate the Phase-4 green — a compile alone proves none of them correct. |
| D3 | `mcp_tool` is delete-on-absent; a tool that vanishes drops its per-tool decision. Accepted. |
| D4 | Collapse migrations to one `0001_init.sql` **and keep a thin forward seam** (`schema_version` + a runner over a `MIGRATIONS` slice) so the first post-v0 change is an additive `0002`, not a wipe. `#[serde(default)]` on every `ConfigSchema` field. |
| D5 | **No `Client` trait. Pure-data `ClientDef` per file.** Each built-in is a single `static DEF: ClientDef` — zero functions, zero trait impls. Paths are declarative via a `PathSpec` enum (4 variants cover all 6 IDEs). Detection data (`binaries`, `mac_app`, `win_paths`) is co-located in the same struct. At startup, detected built-ins are **projected into the `client` DB table** (version-stamped on `CARGO_PKG_VERSION`). After projection, **the DB is the absolute truth** — no downstream code (UI, proxy, cascade, renderer) imports any IDE module or matches on an IDE name. Built-in and custom clients are indistinguishable to consumers; both are `ResolvedClient` from the DB. **One merged `client` table.** One free-fn renderer keyed on `ConfigSchema` (the runtime/DB-owned form of `ConfigSpec`). |
| D6 | Custom-client authoring = declare-directly (format picker + `key_path` + tokenized `entry_template` + live preview). No diff/inference engine. |
| D7 | `mcp_server` trimmed of 6 dead columns; **no per-server `default_tool_policy`** — the default tool policy is a single **global** setting → 2-tier resolution. |

---

## Guiding principles

- **Code is the source of truth for identity; the DB is its uniform *projection*.** Built-in
  identity (display name, config path, schema, skills paths, icon) lives in code and is
  **painted into `client` rows at detect**, refreshed when the app version changes. Readers
  never branch on built-in-vs-custom for identity — they read the row.
- **The DB stores instance state + the identity projection**: routing keys, user intent
  (`tracked`), discovered tools, decisions, managed (client × scope × server/skill) relations,
  audit, and the projected/authored client identity.
- **Three identity concepts stay separate**: `id` (stable identity + code link), `mcp_key`
  (rotatable URL token), display `name` (projected for built-ins / authored for custom).
- **Status is computed, never stored** — `(detected?, tracked, has-managed-rows)`.
- **One baseline migration + a thin forward seam**; idempotent CREATEs, no legacy fixups.
- **Collapse on contact.** This rework makes most current indirection redundant; delete dead
  code and inline pointless wrappers as you touch each site (§10).

---

## 1. The single compiled migration (+ thin seam)

Replaces all of `src/daemon/db/migrations/0001..0005.sql`. New file: **`0001_init.sql`**, the
only migration today. PRAGMAs (`journal_mode=WAL`, `busy_timeout=5000`, `foreign_keys=ON`) stay
set in `init_conn` before the batch.

**Seam.** Keep, do not delete: the `schema_version` table + `query_schema_version`; and a runner
walking `const MIGRATIONS: &[(i64, &str)] = &[(1, INIT_SQL)]`, applying any whose version > the
stored `schema_version` and bumping after each. Today the slice holds only `(1, INIT_SQL)`; the
first post-v0 change appends `(2, …)`. ~20 lines; buys an upgrade path for the custom-client
data v0 introduces. **Gut everything else:** `migrate_legacy_ide_tables`,
`legacy_ide_name_to_kind/display`, `ensure_column`, `drop_column_if_exists`, `ensure_policies_fk`,
and every ad-hoc fixup ([migrations.rs:122-282](../src/daemon/db/migrations.rs#L122-L282)).

```sql
-- 0001_init.sql — Conduit complete schema (single compiled baseline).

CREATE TABLE schema_version (version INTEGER NOT NULL PRIMARY KEY);
INSERT OR IGNORE INTO schema_version (version) VALUES (1);

-- ── MCP servers & tools ───────────────────────────────────────────────
CREATE TABLE mcp_server (
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
-- DROPPED as dead (verified no effective reader/writer; clean reset → free, re-add w/ a real
-- writer when a feature needs it): last_init_duration_ms, package_version, mcp_self_version
-- (probe never produces these — ProbeResult yields only reported_name/reported_version);
-- created_at (written 0, never read); last_auth_error_at (write-only). ALSO default_tool_policy:
-- per-server default was seed-only ('ask'/probe), never user-written, and its seed row SHADOWED
-- settings_kv.default_unknown_tool_policy (tier 2 always hit → global default unreachable; a
-- latent bug). Default tool policy is a GLOBAL setting only (§2). Struct/upsert/select shrink.

-- merges server_tools (metadata) + policies(scope='tool') (decision)
CREATE TABLE mcp_tool (
    mcp_server      TEXT NOT NULL REFERENCES mcp_server(name) ON UPDATE CASCADE ON DELETE CASCADE,
    name            TEXT NOT NULL,
    description     TEXT,
    input_schema    TEXT NOT NULL DEFAULT '{}',
    discovered_at   INTEGER NOT NULL,               -- every row is a currently-advertised tool (delete-on-absent)
    decision        TEXT,                           -- NULL = inherit global; else 'allow'|'deny'|'ask'
    PRIMARY KEY (mcp_server, name)
);

-- ── Approvals & audit ─────────────────────────────────────────────────
CREATE TABLE pending_approvals (
    id          TEXT NOT NULL PRIMARY KEY,
    mcp_server  TEXT NOT NULL,
    tool        TEXT NOT NULL,
    args_json   TEXT NOT NULL,
    client_key  TEXT NOT NULL,                      -- was ide_key (= client.mcp_key, denormalized; no FK)
    created_at  INTEGER NOT NULL,
    status      TEXT NOT NULL                       -- 'pending'|'approved'|'denied'|'timeout'|'cancelled'
);
CREATE INDEX idx_pending_approvals_status ON pending_approvals(status);

CREATE TABLE audit_log (
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
CREATE INDEX idx_audit_log_ts ON audit_log(ts DESC);
CREATE INDEX idx_audit_log_server_tool ON audit_log(mcp_server, tool);
CREATE INDEX idx_audit_log_client_key ON audit_log(client_key);

-- ── Settings (created before the skill_repos seed reads its sentinel) ──
CREATE TABLE settings_kv (key TEXT NOT NULL PRIMARY KEY, value TEXT NOT NULL);
INSERT OR IGNORE INTO settings_kv (key, value) VALUES
  ('default_unknown_tool_policy', 'ask'),
  ('open_window_at_startup',      'false'),
  ('routing_paused',              'false'),
  ('has_completed_first_run',     'false'),
  ('shown_background_hint',       'false');
-- NOTE: 'client_def_version' is deliberately NOT seeded — absent ≠ CARGO_PKG_VERSION, so the
-- first detect pass projects all built-ins. Written by the detect-snapshot code (§3 project_builtins).

-- ── Skills (skills table precedes managed_skill's FK target) ──────────
CREATE TABLE skill_repos (
    owner   TEXT NOT NULL,
    name    TEXT NOT NULL,
    branch  TEXT NOT NULL DEFAULT 'main',
    enabled INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (owner, name)
);

CREATE TABLE skills (
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
CREATE INDEX idx_skills_repo ON skills(repo_owner, repo_name);

INSERT INTO skill_repos (owner, name, branch, enabled)
SELECT 'anthropics', 'skills', 'main', 1
WHERE NOT EXISTS (SELECT 1 FROM settings_kv WHERE key = 'skill_repos_seeded');
INSERT INTO skill_repos (owner, name, branch, enabled)
SELECT 'mattpocock', 'skills', 'main', 1
WHERE NOT EXISTS (SELECT 1 FROM settings_kv WHERE key = 'skill_repos_seeded');
INSERT OR IGNORE INTO settings_kv (key, value) VALUES ('skill_repos_seeded', '1');

-- ── Clients (editors) — single merged table ───────────────────────────
-- Built-in rows: identity columns are a CODE-OWNED CACHE, projected at detect (§1a/§3),
--   NEVER user-edited. Custom rows: identity columns are user-authored once at creation.
CREATE TABLE client (
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
    -- NOTE: project_config_rel / skills_project_rel are Phase 5 additions (directory-scope).
    -- Not present in this baseline; added as a migration when scope UI lands.
);

CREATE TABLE managed_mcp (
    client_id   TEXT NOT NULL REFERENCES client(id) ON DELETE CASCADE,
    scope       TEXT NOT NULL DEFAULT 'global',       -- 'global' | absolute project dir
    -- ON UPDATE CASCADE is load-bearing: server rename relies on FK cascade (query_rename_server
    -- does a bare UPDATE mcp_server SET name and trusts the FK to fix managed_mcp).
    server_name TEXT NOT NULL REFERENCES mcp_server(name) ON UPDATE CASCADE ON DELETE CASCADE,
    PRIMARY KEY (client_id, scope, server_name)
);
CREATE INDEX idx_managed_mcp_server ON managed_mcp(server_name);

CREATE TABLE managed_skill (                          -- replaces skill_targets; scope-aware
    client_id TEXT NOT NULL REFERENCES client(id) ON DELETE CASCADE,
    scope     TEXT NOT NULL DEFAULT 'global',
    skill_dir TEXT NOT NULL REFERENCES skills(directory) ON UPDATE CASCADE ON DELETE CASCADE,
    PRIMARY KEY (client_id, scope, skill_dir)
);
CREATE INDEX idx_managed_skill_dir ON managed_skill(skill_dir);

CREATE TABLE pending_oauth_drafts (
    draft_uuid TEXT PRIMARY KEY,
    created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
);
```

Emitted order is copy-paste-correct: `settings_kv` precedes the `skill_repos` seed that reads its
sentinel; `skills` precedes `managed_skill`'s FK target; `client`/`mcp_server` precede
`managed_mcp`/`managed_skill`.

### 1a. Identity projection — the load-bearing model

Built-in identity (name / config_path / schema / skills paths) is **code**, painted into `client`
rows by the detect pass so all readers are uniform. None of those fields depend on state that
changes *between detects on one machine* — they change only on a **Conduit code update** (a const
changed) or the **first time a client is seen**. So:

```
// startup, after detect_installed():
let coded = env!("CARGO_PKG_VERSION");
if settings_kv["client_def_version"] != coded {           // ≈ once per upgrade — free
    for kind in detected built-ins:
        upsert client row: overwrite name/config_path/schema_json/skills_* from code consts
    settings_kv["client_def_version"] = coded
}
for kind in detected built-ins where row absent:           // first-detect insert
    insert row, mint mcp_key, project identity, is_builtin=1
// always: bump last_seen_at; `detected` status is computed, not stored
```

**Invariants (violate → the drift bug returns):**
1. **Re-project on version change, unconditionally overwriting the identity columns.** Guard with
   a test: project → mutate a code const → bump version → re-detect → assert row updated.
2. **The detect-snapshot runs at startup before any consumer reads** `client` (UI / proxy).
3. **Identity columns of built-in rows are code-owned cache** — nothing but the snapshot writes
   them; no UI edit path for a built-in's name/schema/path.
4. **Detect only snapshots built-ins.** Custom rows (`is_builtin = 0`) are user-authored at
   creation; the snapshot must not touch them.
5. **Full catalog for the UI** ("installable but not yet detected" built-ins) = `ClientKind::ALL`
   (code) ∪ `client` rows. Every *other* read is pure DB.

**Accepted edge:** if a user changes `XDG_CONFIG_HOME`/`APPDATA` between runs *without* upgrading,
the stored `mcp_config_path` goes stale. Documented as **"moving your config dir requires a Conduit
restart."** Not engineered around.

---

## 2. Combine tools + policies — semantics

A single **`mcp_tool`** row holds *both* discovered metadata and the per-tool decision. There is
**no per-server fallback** — the only default is the global `settings_kv.default_unknown_tool_policy`
(user-writable via the Settings tab). No magic empty-tool rows, no per-server policy column.

**Resolution (two tiers, single query path):**
```
mcp_tool.decision  (if non-NULL)
  ↳ else settings_kv.default_unknown_tool_policy  (global; fallback 'ask')
```
The per-server middle tier is **gone**: it was seed-only and its `'ask'` seed *shadowed* the global
setting — today tier 2 always hit, so a user's global default was unreachable (latent bug, fixed by
removal). Two tiers also kill today's redundant round-trip:
[mcp_proxy.rs:85-91](../src/daemon/mcp_proxy.rs#L85-L91) calls `get_policy()`, then on `None` calls
`resolve_policy()` which **re-runs `get_policy()` internally** — a 3-trip miss path. The rewritten
resolver is a **single path**: one `mcp_tool` read, then settings. Read enums via
`from_db_str → Option` (surface `DbError` on unknown strings); no `_ =>` catch-all.

**Probe / re-probe (delete-on-absent):** every `mcp_tool` row is a *currently-advertised* tool. On
re-probe: upsert metadata + `discovered_at = now` for present tools (leaving `decision` intact);
**DELETE rows for tools no longer advertised**, dropping their decision too. `discovered_at` stays
`NOT NULL`; `McpTool` has no invalid state to model.

> **Accepted cost:** a tool that transiently vanishes and returns loses its user `deny`/`allow` and
> re-resolves to the global default. Accepted for schema simplicity.

**Seed detail:** a freshly-probed tool starts `decision = NULL` (inherit). Do **not** port today's
`decision='ask', source='probe'` seed ([servers.rs:147-151](../src/daemon/db/servers.rs#L147-L151))
— `NULL` is what lets resolution fall through to the global default.

---

## 3. Rust type & module changes

### `db/types.rs`
| Today | After |
|---|---|
| `struct Server { …, server_name, server_version, last_init_duration_ms, package_version, mcp_self_version, created_at, last_auth_error_at, … }` | `struct McpServer { …, reported_name, reported_version, … }` — 6 dead cols dropped (§1) |
| `struct Policy { id, server, tool, decision, decision_source, created_at }` | **removed** — folded into `McpTool.decision`; no per-server default |
| `struct ToolMeta { name, description, input_schema_json, discovered_at }` | `struct McpTool { server, name, description, input_schema_json, discovered_at: i64, decision: Option<Decision> }` (`None` = inherit; `decision_source` dropped — provenance is in `audit_log`) |
| `struct PendingApproval { …, server, ide_key, … }` | fields `mcp_server`, `client_key` |
| `struct AuditEntry { …, server, ide_key, ide_name, … }` + `struct AuditFilter { …, ide_key, server, … }` | fields `mcp_server`, `client_key`, `client_name` (both structs) |
| `struct IdeKey`, `IdeRecord`, `ManagedIdeConfig`, `enum IdeStatus` | **removed** → `struct ClientRow` (raw `client` row) + `struct ResolvedClient` (§3 below) |
| `fresh_ide_key()` | `fresh_mcp_key()` (unchanged 8-char alnum) |
| `enum Decision`, `DecisionSource`, `ApprovalStatus`, `AuditDecision` | **unchanged** (`DecisionSource` now only used by `audit_log`) |

### `clients/` — pure-data `ClientDef` per file, DB is absolute truth

A "client" has **no behavior**, only data — custom clients can never carry code, so a trait buys
nothing. PR #14's `ClientSpec` trait is replaced: every trait method was a one-liner forwarding to
a const or free fn — the trait was a vtable over data, not behavior. Its `&'static str` return
types also structurally prevent custom clients without memory leaks.

**Architecture rule:** after detect→project at startup, **no downstream code imports any IDE
module or matches on an IDE name**. The DB is the single source of truth. Built-in and custom
clients are indistinguishable to consumers — both are `ResolvedClient` from the `client` table.

#### `clients/paths.rs` — declarative path resolution

```rust
/// 4 variants cover all 6 current IDEs. No per-file path functions needed.
pub enum PathSpec {
    /// home_dir().join(rel) — ClaudeCode, OpenClaude
    Home(&'static str),
    /// config_dir().join(rel) on ALL platforms — VsCodeCopilot
    ConfigDir(&'static str),
    /// home_dir().join(unix) normally, config_dir().join(win) on Windows — Cursor, OpenCode
    PlatformSplit { unix: &'static str, windows: &'static str },
    /// $ENV/suffix, fallback to home_dir().join(fallback) — Codex
    EnvOrHome { env: &'static str, suffix: &'static str, fallback: &'static str },
}

pub fn resolve(spec: &PathSpec) -> Option<PathBuf>;           // OS-resolved absolute path
pub fn resolve_under(spec: &PathSpec, home: &Path) -> PathBuf; // test seam: resolve against fake home
```

#### `clients/<ide>.rs` — one `static DEF` per file, zero functions

Each file is a single struct literal. Identity, paths, config spec, and detection data
are co-located. No functions, no trait impls, no boilerplate.

```rust
/// The full static definition of a built-in client.
/// Replaces ClientSpec trait + IdeSpec + IdeKind enum methods.
pub struct ClientDef {
    pub id:           &'static str,
    pub display_name: &'static str,
    pub icon_svg:     &'static [u8],
    // paths (declarative — resolved by paths::resolve)
    pub config:       PathSpec,
    pub skills:       Option<PathSpec>,          // None = no skills support (VsCodeCopilot)
    // MCP config writing spec
    pub config_spec:  ConfigSpec,
    // detection
    pub binaries:     &'static [&'static str],
    #[cfg(target_os = "macos")]
    pub mac_app:      Option<&'static str>,
    #[cfg(target_os = "windows")]
    pub win_paths:    &'static [&'static str],
}
// NOTE: project_config_rel / project_skills_rel are Phase 5 features (directory-scope).
// Not present on ClientDef until then — no phantom fields.
```

Example — Claude Code (entire file):
```rust
// clients/claude_code.rs
use super::paths::PathSpec;
use super::schema::{ConfigFormat, ConfigSpec};

pub static DEF: super::ClientDef = super::ClientDef {
    id:           "ClaudeCode",
    display_name: "Claude Code",
    icon_svg:     include_bytes!("../../../assets/client/claude-code.svg"),
    config:       PathSpec::Home(".claude.json"),
    skills:       Some(PathSpec::Home(".claude/skills")),
    config_spec: ConfigSpec {
        format:         ConfigFormat::Json,
        key_path:       &["mcpServers"],
        entry_template: r#"{"type":"http","url":"${url}"}"#,
    },
    binaries:     &["claude"],
    #[cfg(target_os = "macos")]
    mac_app:      Some("Claude.app"),
    #[cfg(target_os = "windows")]
    win_paths:    &["$APPDATA\\npm\\claude.cmd"],
};
```

Example — Codex (the env-var + TOML outlier, still zero functions):
```rust
// clients/codex.rs
pub static DEF: super::ClientDef = super::ClientDef {
    id:           "Codex",
    display_name: "Codex",
    icon_svg:     include_bytes!("../../../assets/client/codex.svg"),
    config:       PathSpec::EnvOrHome {
                      env: "CODEX_HOME", suffix: "config.toml",
                      fallback: ".codex/config.toml",
                  },
    skills:       Some(PathSpec::EnvOrHome {
                      env: "CODEX_HOME", suffix: "skills",
                      fallback: ".codex/skills",
                  }),
    config_spec: ConfigSpec {
        format:         ConfigFormat::Toml,
        key_path:       &["mcp_servers"],
        entry_template: r#"{"url":"${url}"}"#,
    },
    binaries:     &["codex"],
    #[cfg(target_os = "macos")]
    mac_app:      None,
    #[cfg(target_os = "windows")]
    win_paths:    &["$APPDATA\\npm\\codex.cmd"],
};
```

#### `clients/schema.rs` — config spec (static) + config schema (runtime)

```rust
// Static — lives in ClientDef, compile-time
#[derive(Debug, Clone, Copy)]
pub enum ConfigFormat { Json, Jsonc, Toml }

#[derive(Debug, Clone)]
pub struct ConfigSpec {
    pub format:         ConfigFormat,
    pub key_path:       &'static [&'static str],
    pub entry_template: &'static str,              // JSON literal, parsed at render time
}

// Runtime — serialized into client.schema_json in the DB, deserialized by consumers
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct ConfigSchema {
    #[serde(default)] pub format: ConfigFormat,
    #[serde(default)] pub key_path: Vec<String>,
    #[serde(default)] pub entry_template: serde_json::Value,
}
// ConfigSpec → ConfigSchema conversion used by project_builtins
impl From<&ConfigSpec> for ConfigSchema { /* ... */ }
```
// Placeholder vocab (fixed, small): ${name} ${url} ${key} ${port}. Substituted RECURSIVELY over
// the whole entry_template (a token may live inside a nested array — see Zed below).

#### `clients/mod.rs` — collects defs, no dispatch logic

```rust
pub mod claude_code;
pub mod codex;
pub mod cursor;
pub mod openclaude;
pub mod opencode;
pub mod vscode_copilot;

mod paths;
mod schema;
mod render;
mod project;
mod detect;

pub use schema::{ConfigFormat, ConfigSpec, ConfigSchema};
pub use render::{render_mcp, purge_mcp, purge_one};

/// All built-in client definitions. Adding an IDE = one new file + one line here.
pub static BUILTINS: &[&ClientDef] = &[
    &claude_code::DEF, &codex::DEF, &cursor::DEF,
    &openclaude::DEF,  &opencode::DEF, &vscode_copilot::DEF,
];

pub fn builtin_by_id(id: &str) -> Option<&'static ClientDef> {
    BUILTINS.iter().copied().find(|b| b.id == id)
}
```

`IdeKind` enum, `ClientSpec` trait, `spec_for()`, `SKILL_TARGET_IDES`, `JsonSchema`,
`json_schema_for`, `config_path_for`, `skills_dir_for`, `ide_db_name`, `ide_name`,
`ide_kind_from_db_name` — **all deleted**. "Supports skills" = `client.skills_dir IS NOT NULL`.

#### `clients/resolved.rs` — the ONE type every consumer sees

```rust
pub struct ResolvedClient {
    pub id: String,
    pub display_name: String,
    pub is_builtin: bool,
    pub mcp_key: String,
    pub config_path: PathBuf,             // absolute, already resolved
    pub skills_dir: Option<PathBuf>,      // None = no skills support
    pub schema: ConfigSchema,             // deserialized from schema_json
}

// Built from a DB row. No reference to any ClientDef or IDE name.
impl ResolvedClient {
    pub fn from_row(row: ClientRow) -> Self;
}

pub async fn resolve_client(db: &Db, id: &str) -> Result<Option<ResolvedClient>, DbError>;
```

#### `clients/render.rs` — ONE renderer, format-switched internally

```rust
// Replaces write_uniform_mcp_servers + write_toml_mcp_servers + all purge variants.
// Does NOT know which IDE it's writing for — reads schema from ResolvedClient.
pub fn render_mcp(client: &ResolvedClient, servers: &[String], port: u16) -> Result<(), ClientError>;
pub fn purge_mcp(schema: &ConfigSchema, path: &Path, key: &str, port: u16) -> Result<(), ClientError>;
pub fn purge_one(schema: &ConfigSchema, path: &Path, key: &str, server: &str, port: u16) -> Result<(), ClientError>;
```

**Four renderer rules the real editor schemas force (pin these):**
1. **Recursive token substitution** — walk the entire `entry_template` tree; replace `${…}` in any
   string at any depth. (Lets a stdio-only client be targeted via `["mcp-remote","${url}"]`.)
2. **`Jsonc` needs a comment-tolerant parser** (gemini/zed/VS Code settings.json carry comments +
   trailing commas; `serde_json` chokes). Use a json5/strip-comments read path for `Jsonc`.
3. **Walk-and-create `key_path`, merge — never clobber.** Descend (creating intermediate objects),
   insert our entries beside the user's siblings, re-serialize.
4. **Purge scans the whole entry for the endpoint**, not a fixed `url` field — with variable field
   names + url-in-args it must match `…/<port>/<key>/<server>/mcp` anywhere in the serialized entry.

**Boundary (documented limit):** the template cannot express *logic* (keyring-fetched auth headers,
computed values). "Needs logic → must be a built-in." Keep the placeholder vocab fixed at the four
above.

#### `clients/detect.rs` — detection reads from `BUILTINS`

```rust
pub enum DetectionSource { Path(PathBuf), MacApp(PathBuf), WinApp(PathBuf) }
pub struct DetectedClient { pub def: &'static ClientDef, pub source: DetectionSource }
pub fn detect_installed() -> Vec<DetectedClient>;
```

Walks `BUILTINS`, checks `def.binaries` / `def.mac_app` / `def.win_paths`. No separate
`IDE_SPECS` / `IdeSpec` — detection data lives in the same `ClientDef` as everything else.

#### `clients/project.rs` — the detect→DB projection (§1a)

```rust
/// Runs ONCE at startup, BEFORE any consumer reads the client table.
/// After this, ClientDef is never read again until next startup.
pub async fn project_builtins(db: &Db, detected: &[DetectedClient]) -> Result<(), DbError>;
```

Resolves `PathSpec` → absolute paths, converts `ConfigSpec` → `ConfigSchema` JSON, upserts into
`client` rows. Version-gated on `CARGO_PKG_VERSION` — overwrites identity columns only on upgrade.

#### Data flow (the architectural boundary)

```
  BUILTINS ──detect──→ project_builtins() ──→ DB client table
                                                    │
  UI ──────────────→ db.list_clients() ────→ ResolvedClient
  proxy ───────────→ db.lookup_by_key() ───→ ResolvedClient
  cascade ─────────→ db.resolve_client() ──→ ResolvedClient
  render ──────────→ client.schema ────────→ format-switched write
                                    │
           every consumer reads ONE type from ONE source
           no consumer imports clients/claude_code.rs
```

**Built-in → template mapping (proves subsumption; renderer must reproduce today's exact entry):**

| Built-in | `config` PathSpec | `key_path` | `entry_template` | `format` |
|---|---|---|---|---|
| ClaudeCode | `Home(".claude.json")` | `["mcpServers"]` | `{"type":"http","url":"${url}"}` | Json |
| Cursor | `PlatformSplit { unix: ".cursor/mcp.json", windows: "Cursor/mcp.json" }` | `["mcpServers"]` | `{"url":"${url}"}` | Json |
| VsCodeCopilot | `ConfigDir("Code/User/mcp.json")` | `["servers"]` | `{"type":"http","url":"${url}"}` | Jsonc |
| OpenCode | `PlatformSplit { unix: ".config/opencode/opencode.json", windows: "opencode/opencode.json" }` | `["mcp"]` | `{"type":"remote","url":"${url}"}` | Json |
| OpenClaude | `Home(".openclaude.json")` | `["mcpServers"]` | `{"type":"http","url":"${url}"}` | Json |
| Codex | `EnvOrHome { env: "CODEX_HOME", ... }` | `["mcp_servers"]` | `{"url":"${url}"}` | Toml |

**Un-implemented editors the template *also* handles** (differing URL field names a fixed-`url` enum
could not express): Antigravity `["mcpServers"]`/`{"serverUrl":"${url}"}`; Gemini CLI
`["mcpServers"]`/`{"httpUrl":"${url}"}` (jsonc); Zed
`["context_servers"]`/`{"source":"custom","command":"npx","args":["mcp-remote","${url}"]}` (jsonc).

### Adding a built-in editor afterward = one new `<ide>.rs` file (a single `static DEF: ClientDef`) + one line in `BUILTINS` + one SVG icon. No trait, no functions, no impl block, no match arms.

---

## 4. DB layer module changes (`src/daemon/db/`)

- **`migrations.rs` — gutted, seam kept (§1).** `init_conn`: PRAGMAs → run the `MIGRATIONS` slice
  (today just `(1, INIT_SQL)`) → `PRAGMA foreign_key_check`. Keep `query_schema_version`. Delete
  `migrate_legacy_ide_tables`, `legacy_ide_name_to_*`, `ensure_column`, `drop_column_if_exists`,
  `ensure_policies_fk`. `query_list_tables`/`query_list_columns` keep **only** if the rewritten
  `tests/db_actor.rs` asserts the new table set.
- **`servers.rs` → `mcp_server.rs`.** Rename `servers`→`mcp_server` SQL; drop the 6 dead columns
  from `query_upsert_server`/`query_get_server`/select lists. Merge the tool-write path:
  `upsert_server_with_tools` writes `mcp_server` + upserts `mcp_tool` (metadata + `discovered_at`,
  preserving `decision`), applying the §2 re-probe rule. `query_list_server_tools` →
  `query_list_mcp_tools` returning `McpTool`. **Delete the seed `scope='server'` policy insert.**
- **`policy.rs` — folded into `mcp_tool.rs` (or merged into `mcp_server.rs`).** `set_policy` →
  `UPDATE mcp_tool SET decision`; `resolve_policy` → **two-tier single-query path** (§2). Delete
  `get_policy_scoped`, `query_get_policy_scoped`, `query_list_all_policies`/`list_all_policies`
  becomes "list `mcp_tool` rows with non-NULL decision" (feeds the Policies tab), the `scope` logic,
  and the per-server fallback entirely.
- **`ides.rs` → `clients.rs`.** Queries over the single `client` table + `managed_mcp` /
  `managed_skill`: the **detect projection upsert** (§1a `project_builtins`), `mcp_key` mint on first
  insert, `lookup_client_by_key` (was `lookup_ide_key`), scoped managed-server add/remove,
  custom-client CRUD, and the reverse `list_clients_managing_server(server_name) -> Vec<(client_id,
  scope)>` (consumer of `idx_managed_mcp_server`; cascade needs it — **deliver in Phase 1**). The
  10 `*ide*` queries collapse: `upsert_detected_ide`→`project_builtins`, `list_ides`/`get_ide_by_kind`
  → `client` reads, `mark_ides_stale`/`mark_ide_not_tracked`/`retrack_ide` → `tracked` flag + computed
  status, `update_ide_managed_entries`/`list_all_ide_configs` → **deleted** (cascade derives, §9).
- **`skills.rs`.** `skill_targets` → `managed_skill` (scope-aware). **Split the write path:**
  skill-metadata upsert touches **only** `skills`; `managed_skill` gets separate add/remove DbCmds
  keyed on `(client_id, scope, skill_dir)`. Never delete-then-reinsert targets — today's
  `query_upsert_skill_with_targets` does ([skills.rs:102-109](../src/daemon/db/skills.rs#L102-L109)),
  which with `ON DELETE CASCADE` would wipe user intent on every metadata sync. FK ordering: the
  `client` row must exist before a `managed_skill` insert.
- **`commands.rs` / `mod.rs`.** Rename/merge `DbCmd` variants + dispatch (≈44 variants; see §5).
- **`audit.rs` / `approvals.rs`.** Field renames only (`server`→`mcp_server`, `ide_key`→`client_key`,
  `ide_name`→`client_name`) in `AuditEntry`/`AuditFilter`/`PendingApproval` + their SQL.

---

## 5. Full blast radius (grep-verified) → checked, updated, reworked

Rename map applied **everywhere** the symbols below appear:

| Today | After |
|---|---|
| `IdeKind` · `as_db_kind` · `from_db_kind` · `display_name` · `ALL` | **deleted** → `BUILTINS: &[&ClientDef]` + `builtin_by_id()` |
| `ClientSpec` trait · `spec_for()` · per-IDE `Spec` structs | **deleted** → `ClientDef` static data per file |
| `IDE_SPECS` · `IdeSpec` · `ide_detect.rs` | detection fields merged into `ClientDef`; `clients/detect.rs` walks `BUILTINS` |
| `IdeKey`/`IdeRecord`/`ManagedIdeConfig`/`IdeStatus` · `DetectedIde` | `ClientRow`/`ResolvedClient` (removed others) · `DetectedClient` |
| `ide_key` · `ide_name` · `server`(col) | `client_key` · `client_name` · `mcp_server` |
| `JsonSchema`/`json_schema_for`/`SKILL_TARGET_IDES`/`ide_db_name`/`ide_name`/`ide_kind_from_db_name` | **deleted** → `ConfigSpec` (static) / `ConfigSchema` (runtime DB) + `skills_dir IS NOT NULL` |
| `config_path_for`/`skills_dir_for`/per-file path fns | **deleted** → `PathSpec` enum + `paths::resolve()` |
| `fresh_ide_key` · `servers`/`server_tools`/`policies`/`ides`/`skill_targets` (SQL) | `fresh_mcp_key` · `mcp_server`/`mcp_tool`/`client`/`managed_skill` |

### DB layer (`src/daemon/db/`)
- **`commands.rs` — ≈44 `DbCmd` variants**, each a 3-site edit (variant decl + `mod.rs` dispatch +
  `Db::` async wrapper). Groups: servers/tools 9, **policies 6 → fold to ~2** (`SetToolDecision`,
  resolver is internal), IDE/client 10 → ~6 (`project_builtins`, lookup, list, tracked toggles,
  managed add/remove, custom CRUD, `ListClientsManagingServer`), approvals 5, audit 3, skills 8
  (split targets out), settings 3, OAuth 3, cascade 1 (`CommitCascadeDelete` rewritten, §9),
  infra/introspection (`Ping`/`GetSchemaVersion`/`ListTables`/`ListColumns`). Net variant count
  **drops** after the fold.
- **`types.rs`** — §3 table.
- **migration SQL files** `0001_init.sql`..`0005_oauth_drafts.sql` → replaced by one `0001_init.sql`.

### daemon (`src/daemon/`, non-db)
- **`mcp_proxy.rs` (LOGIC REWRITE):** [:85-91](../src/daemon/mcp_proxy.rs#L85-L91) `get_policy`→`resolve_policy`
  3-trip → single 2-tier read. Decision routing [:94-189] uses `mcp_server`/`client_key`. Auth-error
  edge helpers `maybe_record/clear_auth_error` [:256-282] keep (write `last_auth_error_msg`; drop the
  `last_auth_error_at` write).
- **`cascade.rs` (LOGIC REWRITE — §9):** `plan_targets`/`delete_server_with_cascade`/`TargetIde`/
  `CascadeReport`; drop the stored `config_path`/`managed_entries` blob reads.
- **`http_server.rs`:** route `/{key}/{server}/mcp` [:59]; `lookup_ide_key`→`lookup_client_by_key`
  [:105]; `get_server` [:120]; `route_tool_call(&ide.key, &ide.ide_name, …)` → `client_key`/`client_name`;
  `server_to_config` [:205-261] reads only live `mcp_server` cols.
- **`approval.rs`:** `ApprovalRequest{server, …, ide_key}` → `mcp_server`/`client_key`; `PendingApproval`
  build [:90]; `Event::ApprovalPending` payload [:117].
- **`events.rs`:** `Event::ApprovalPending { id, server, tool, ide_key }` → `mcp_server`/`client_key`;
  `AuthFailure{server,…}`/`AuthCleared{server}`/`ServerHealthChanged{name}` → `mcp_server`.
- **`upstream/probe.rs`:** `ProbeResult{server_name, server_version, tools, oauth_session}` feeds
  `reported_name`/`reported_version` + `mcp_tool` rows. `ToolMeta`→`McpTool` mapping.
- **`server_validators.rs` / `upstream/manager.rs` / `upstream/lifecycle.rs`:** `lifecycle_seconds`
  semantics (0/-1/N) unchanged; rename only.

### glue (`src/glue/`)
- **`refresh.rs` (heavy):** row constructors — `ApprovalRow`[:153-168] (`ide_name`→from
  `lookup_client_names_for_keys`, `server`→`mcp_server`), `IdeTarget`→`EditorTarget`[:308-319]
  (`ide_kind`→`id`, `as_db_kind`→`id`), `IdeRow`→`EditorRow`[:404-422] (`kind`/`key`/`status`/`icon`
  built from `client` rows + `from_id`), `PolicyRow`[:432-440] (**drop `source`** — `mcp_tool` has no
  `decision_source`; reads `db` "tools with decision"), `AuditRow`[:453-465] (`ide_name`→`client_name`,
  `server`→`mcp_server`), `DashboardStats.editor_count`[:517-525], **`ServerDetail`[:258-286] — remove
  the version/latency reads of dropped cols** (`server.package_version`[:254-256],
  `server.last_init_duration_ms`[:282]). Helper `lookup_ide_names_for_keys`[:535-548]→`client_name`.
  Event arms [:89-126]. Policy display `get_policy`[:228] + global default kv[:66-73].
- **`callbacks_ide.rs` → `callbacks_client.rs`:** `refresh_selected_editor`[:28], `get_ide_by_kind`
  [:120]→`client` read, `select-ide`/`remove-ide`/`retrack-ide`/`edit/save/reload` handlers
  [:72,196,202,217,223], `write_raw`/`read_raw` calls.
- **`util.rs`:** `from_db_kind`[:83,103]→`from_id`, `clients::purge`[:106]→`purge_mcp`,
  `detect_installed`[:115]→`project_builtins` path, `upsert_detected_ide`[:124] removed,
  `resolve_ide_config_path`[:76-88] → read `client.mcp_config_path` (collapse, §10).
- **`callbacks_servers.rs`:** `on_toggle_server_target`[:224] (`ide_kind`→client id), server-field
  reads [:149-153].
- **`callbacks_misc.rs`:** global `default_unknown_tool_policy` write[:38-42] keep; `set_policy`
  notification path[:58].
- **`wizard.rs`:** `Server` initializer [:176-186] — remove the dropped fields (`last_init_duration_ms`,
  `package_version`, `mcp_self_version`, `created_at`), keep `reported_name`/`reported_version` set
  from probe [:243-244]; `lifecycle_seconds` path unchanged.
- **`skills.rs`:** `active_ide_kinds`→client kinds[:246], `Skill.targets`→`managed_skill` relation,
  `set_targets`[:149], `skill_targets`/`empty_targets` helpers[:350-365], `IdeTarget`→`EditorTarget`.

### gui (`src/gui/`)
- **`notifications.rs`:** `Event::ApprovalPending{…ide_key…}`[:52] → `client_key`;
  `lookup_ide_key`→`lookup_client_by_key`[:53]; `.ide_name`→`.client_name`[:54].
- **`notification_router.rs`:** `set_policy(…Notification)`[:71] → `SetToolDecision`.
- **`tray.rs`:** `last_auth_error_msg` filter[:314] keep; approval/auth event arms[:287-295].

### UI (`src/ui/`, Slint)
- **`structs.slint`:** `IdeTarget{ide_kind,label,icon,active}`→`EditorTarget`;
  `IdeRow{kind,display_name,key,status,config_path,icon}`→`EditorRow`;
  `ApprovalRow{ide_name,server,…}`/`AuditRow{ts_label,ide_name,server,…}`→`client_name`/`mcp_server`;
  `PolicyRow{server,tool,decision,source}`→ `mcp_server`/`tool`/`decision` (**drop `source`**);
  `DashboardStats.editor_count` keep; `ServerDetail{version,…,latency_label}` — version/latency now
  always-empty for the dropped cols, **remove those fields/bindings**; `SkillRow.targets:[IdeTarget]`
  →`[EditorTarget]`. Add `supports_skills: bool` to `EditorRow`; scope fields later (Phase 5).
- **`main.slint`:** properties `ides`→`editors` [:43]; callbacks `remove-ide-clicked`/
  `retrack-ide-clicked`/`refresh-ides-clicked`/`select-ide`/`reveal-ide-config`/`edit-ide-config`/
  `save-ide-config`/`reload-ide-config` [:111-140] → `editor` names; `toggle-server-target` [:124].
  Tab label "Editors" stays.
- **`tabs/editors.slint`** (rows/detail/status), **`tabs/servers.slint`** (`TargetToggle`, "CONNECTED
  EDITORS"[:337], `[IdeTarget] targets`[:143]), **`tabs/skills.slint`** (`[IdeTarget]` toggles[:305-316]),
  **`tabs/dashboard.slint`** (`editor_count`[:200-201], `a.ide_name`[:411]), **`tabs/log.slint`**
  (`ide_name`[:42]), **`components/approvals_popup.slint`**(`ide_name`[:45]),
  **`components/approval_modal.slint`** ("REQUESTER" `ide_name`[:146]) → label/field renames to
  editor/`client_name`/`mcp_server`.

### tests (`tests/`)
- **Rewrite/DELETE FIRST (assert dead schema):** `migrations.rs` (`schema_version==4`,
  `ides`/`ide_keys`/`managed_ide_configs` presence) — mostly delete; `db_actor.rs` (`schema_version==4`,
  `policies`, `ides`, legacy-table absence, `upsert_detected_ide`/`update_ide_managed_entries`).
- **Rewrite vs new schema (the D2 TDD gate):** `policy_lookup.rs` (2-tier resolver), `servers_crud.rs`
  (rename-cascade via `managed_mcp`), **new `managed_skill` resync test** (metadata refresh must NOT
  wipe targets), `cascade.rs` (`ides_trimmed`→derived report).
- **Rename:** `ide_detect.rs`→`client_detect.rs`, `ide_writer.rs`→`client_writer.rs` (now drives the
  one renderer — assert built-in output unchanged + the 3 new editors), `skills_db.rs`,
  `skills_paths.rs` (`SKILL_TARGET_IDES`/`ide_kind_from_db_name` gone).
- Untouched: `approval`, `health`, `probe`, `config_*`, `installer`, `ipc`, `oauth_pkce`,
  `paste_parser`, `paths`, `runtime`, `secrets`, `notifications`, `events`, `headless`.

### main / paths / installer / assets
- **`main.rs`:** `Db::spawn_at`[:107], `push_initial_models`[:213], event bridge[:220] — ensure the
  detect→`project_builtins` pass runs at startup **before** model paint (invariant §1a.2).
- **`paths.rs` / `installer/`:** no client/server table refs — untouched.
- **`assets/`:** add `assets/client/default.svg` (custom-client icon). `antigravity.svg`/`gemini.svg`
  are orphans today — wire them when those built-ins land; do not delete.

---

## 6. Phased build (against the locked schema)

- **Phase 0 — Lock.** Sign off. *(verify: agreement)*
- **Phase 1 — DB core + the D2 tests.** **First** rewrite/delete `tests/db_actor.rs` +
  `tests/migrations.rs`. Then `0001_init.sql` + seam, gut `migrations.rs`, new `types.rs`,
  `mcp_server.rs` (+merged tools), folded policy (2-tier), `clients.rs` (incl. `project_builtins` +
  `list_clients_managing_server`), `managed_skill`, `DbCmd` rewire. **Write now** (red until logic
  lands, green = the Phase-4 gate): `policy_lookup.rs`, cascade test in `servers_crud.rs`, the new
  `managed_skill` resync test.
- **Phase 2 — Client catalog + renderer + projection.** `ClientKind` catalog, `CLIENT_SPECS`,
  `ConfigSchema`, the one `render_mcp`/`purge_mcp`/`purge_one`, `ResolvedClient`, `resolve_client`,
  `project_builtins` (version-stamped §1a). *(verify: renderer unit tests + `client_writer.rs`
  reproduce today's exact built-in output; projection test = invariant §1a.1)*
- **Phase 3 — Daemon wiring.** proxy/approval/upstream/http use `mcp_server`, 2-tier resolution,
  `mcp_key` routing, scoped managed writes. **`cascade.rs` LOGIC REWRITE** (§9).
- **Phase 4 — Glue + UI rename.** §5 rename map, refresh/callbacks read `client` uniformly, Slint
  structs/labels. *(verify: tree goes GREEN; `cargo run` boots, Editors tab renders; the 3 Phase-1
  tests pass)*
- **Phase 5 — New features.** directory-scope UI + per-client project paths; **custom-client
  declare-directly add/edit UI** (format picker + `key_path` + tokenized `entry_template` + live
  preview); scoped skills sync. *(verify: manual e2e per feature)*
- **Phase 6 — Green.** `cargo clippy --all-targets -- -D warnings`, `cargo fmt`, `cargo nextest run`.

**Compile strategy — atomic red landing.** No transitional aliases. `Server`/`Policy`/`IdeRecord`/
`ToolMeta`/`IdeKey`/`ManagedIdeConfig` are removed in Phase 1; the tree stays red through Phases 2–3
while consumers migrate; it goes green at the **end of Phase 4**. `clients/mod.rs` imports
`IdeKey`/`ManagedIdeConfig` ([clients/mod.rs:17](../src/daemon/clients/mod.rs#L17)) that vanish in
Phase 1, so **Phases 1+2 are one indivisible compile unit** (the `ide_detect.rs`→`clients/detect.rs`
move happens in it). `render_mcp`/`purge_*` **lose the old `ManagedIdeConfig` return** — the caller
persists `managed_mcp` rows directly. The 3 D2 tests are the *behavioral* gate at the Phase-4 green.

---

## 7. Resolved decisions (cosmetic defaults)

- `reported_name`/`reported_version` rename: **yes** (clarity inside `mcp_server`).
- Table naming: `mcp_server`/`mcp_tool`/`client` singular; `skills`/`skill_repos`/`pending_approvals`/
  `settings_kv` plural. Mild mix accepted.
- Custom-client icons: single default icon now, no per-custom upload.
- `managed_skill` replaces `skill_targets` now (scope-aware this pass).
- `ConfigSchema` = `{format, key_path, entry_template}`, declare-directly authoring.
- One merged `client` table; identity projected at detect, version-stamped on `CARGO_PKG_VERSION`.

---

## 8. Non-goals

- No legacy fixups / no version-by-version history of the prior schema. (A thin forward seam IS kept
  — §1.)
- No per-custom-client icon upload; no per-directory routing-key granularity (the URL token stays
  per-client — a dir segment can be added later without a schema change).
- No template *logic* (auth-header injection, computed values) — needs-logic clients must be built-ins.
- No changes to `config.toml`, keyring/secrets, or the OAuth wire beyond column renames.
- Not engineering around mid-session `XDG_CONFIG_HOME`/`APPDATA` relocation (§1a accepted edge).

---

## 9. `cascade.rs` — LOGIC REWRITE (not a rename)

Today `plan_targets` reads a denormalized `managed_entries` blob + a stored `config_path` and filters
the blob for "remaining" ([cascade.rs:135-156](../src/daemon/cascade.rs#L135-L156)). `managed_mcp`
stores neither. New flow:

(a) `list_clients_managing_server(doomed)` → affected `(client_id, scope)` (Phase-1 query);
(b) per client, `resolve_client(id)` → `config_path` / project path + `client.mcp_key`;
(c) recompute "remaining" by re-querying `managed_mcp` for that client's *other* servers.

`TargetIde`/`CascadeTrim` lose the stored `config_path`; `CommitCascadeDelete` /
`record_ide_config_write` lose the blob column (today `commit_cascade_delete` updates
`ides.managed_entries_json` — that path is gone; cascade now deletes `managed_mcp` rows via FK
`ON DELETE CASCADE` and re-derives files). Sequence cascade **after** the registry (Phase 2) and the
Phase-1 by-server query exist.

---

## 10. Dead-code & wrapper-collapse directive (apply while implementing)

This rework makes most current indirection redundant. **As you touch each site, delete dead code and
inline single-use wrappers** — don't port them forward. Concrete candidates the audit surfaced:

- **`ClientSpec` trait + per-IDE modules → `ClientDef` static data.** The trait, all 6 `Spec`
  structs, all per-file path functions (`config_path()`, `config_path_under()`, `skills_dir()`,
  `skills_dir_under()`) — **deleted entirely**, replaced by `PathSpec` enum variants in each
  file's `static DEF`. `spec_for()`, `SKILL_TARGET_IDES`, `JsonSchema` struct,
  `write_uniform_mcp_servers` + `write_toml_mcp_servers` + `purge_*_toml` + `purge_one_server_for_ide*`,
  `ide_db_name`/`ide_name`/`ide_kind_from_db_name`, `config_path_for`, `skills_dir_for`
  — all collapse into `ConfigSpec`/`ConfigSchema` + `render_mcp`/`purge_mcp`/`purge_one`.
  `merger.rs` dual JSON/TOML paths fold into one format-switched renderer.
- **`IdeKind` enum + `ide_detect.rs`** — deleted. Detection data (`binaries`, `mac_app`,
  `win_paths`) moves into each file's `ClientDef`. `detect_installed()` walks `BUILTINS`.
  `IdeKind::ALL`, `as_db_kind`, `from_db_kind`, `display_name` — all gone (data on `ClientDef`).
- **Dead `Server` columns + their UI.** Drop the 6 columns and the now-empty `ServerDetail`
  version/latency bindings ([refresh.rs:254-256,282](../src/glue/refresh.rs#L254-L256)).
- **`resolve_ide_config_path`** ([util.rs:76-88](../src/glue/util.rs#L76-L88)) → a direct
  `client.mcp_config_path` read from the DB; the optional-record path-recompute logic is obsolete
  once identity is projected.
- **Per-client lookup helpers** — `lookup_ide_names_for_keys`
  ([refresh.rs:535-548](../src/glue/refresh.rs#L535-L548)) and `empty_targets`
  ([skills.rs:363-365](../src/glue/skills.rs#L363-L365)) are single-use; inline if the call site
  simplifies.
- **Duplicated `now_unix()`** — ✅ **already done** by PR #14 via `time_util.rs`.
- **Atomic writes** — ✅ **already done** by PR #14 via `atomic_io.rs` + `clients/io.rs`.
- **Migration machinery** — `ensure_column`, `drop_column_if_exists`, `ensure_policies_fk`,
  `migrate_legacy_ide_tables`, `legacy_ide_name_to_*` all delete.
- **Policy double-query** — the `get_policy`→`resolve_policy`(→`get_policy`) miss path
  ([mcp_proxy.rs:85-91](../src/daemon/mcp_proxy.rs#L85-L91)) becomes one read.
- **Introspection DbCmds** — `ListTables`/`ListColumns`/`GetSchemaVersion` survive only if a rewritten
  test still asserts on them; otherwise drop.
- **`PolicyRow.source`** — no `decision_source` on `mcp_tool`; remove the column rather than fake it.

Rule: every wrapper you keep must earn it (multiple callers, or a real boundary). If the new
data-driven path makes a function a one-line pass-through with a single caller, inline it. Match
CLAUDE.md: "no abstractions for single-use code."

---

## Sources (un-implemented editor schemas)

- Antigravity MCP — https://medium.com/google-developer-experts/google-antigravity-custom-mcp-server-integration-to-improve-vibe-coding-f92ddbc1c22d
- Gemini CLI MCP — https://geminicli.com/docs/tools/mcp-server/
- Zed MCP — https://zed.dev/docs/ai/mcp
