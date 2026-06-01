# `src/` Restructure Plan

Status: **implemented on `db-rework-v2`** (uncommitted — staged renames + working-tree edits).
Verified green: `cargo check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `cargo fmt`.
Decisions below were made interactively; this document is the executable spec.

## 1. Goal & principles

Reshape `src/` so a newcomer opening the tree understands the system immediately, without
changing behaviour. Every move traces to one of:

- **Name what it is, not how it's wired** — kill insider/misleading names (`glue`, `gui`, `ui.rs`).
- **Group by the concept a reader looks for** — the GUI in one place, the two MCP directions
  named by direction, the static IDE catalog at top level.
- **Keep data next to the logic that owns it** — only the *pure static* IDE definitions float up;
  row types, errors, SQL, theme stay put.
- **No junk drawers, no speculative structure.** (`utils/` was explicitly rejected.)

Hard constraints preserved: two crates in one package (`lib.rs` library + `main.rs` binary),
thread-affinity contract, DB-actor pattern, data-driven clients, `build.rs` Slint compile.

## 2. Target tree

```
src/
  main.rs                      binary entry (unchanged location)
  lib.rs                       library root
  runtime.rs                   the tokio runtime/worker (foundational; stays at root)
  atomic_io.rs                 cross-platform atomic file write (shared; stays at root)

  slint/                       ← was ui/   (Slint markup; build.rs path updated)
    main.slint theme.slint structs.slint
    components/  tabs/

  window/                      ← binary GUI (was glue/ + ui.rs)
    mod.rs                     thin: declares submodules, re-exports MainWindow
    view.rs                    ← was src/ui.rs (include_modules!() + #![allow] + tab consts)
    refresh.rs  wizard.rs  skills.rs  util.rs
    callbacks/
      mod.rs                   ← was glue/callbacks.rs (install_callbacks fan-out)
      servers.rs ide.rs misc.rs log.rs   ← were glue/callbacks_*.rs (prefix dropped)

  os/                          ← was gui/  (OS/desktop integration)
    mod.rs tray.rs notifications.rs notification_router.rs hotkey.rs

  client/                      ← NEW top-level: the static IDE catalog (library)
    mod.rs
    claude_code.rs codex.rs cursor.rs opencode.rs openclaude.rs vscode_copilot.rs
    example.rs                 copy-template (#![cfg(any())])

  daemon/                      headless core (name kept)
    mod.rs
    downstream/                ← IDEs → us (we're the MCP server)
      mod.rs
      http_server.rs  proxy.rs (was mcp_proxy.rs)  approval.rs
    upstream/                  ← us → MCP servers (we're the MCP client) — contents unchanged
      mod.rs manager.rs probe.rs http.rs stdio.rs oauth.rs oauth_store.rs health.rs installer.rs
    config/                    ← our settings (config.toml)
      mod.rs  toml.rs (was config_toml.rs)  watcher.rs (was config_watcher.rs)
    client_config/             ← was clients/ engine: detect/resolve/render/write IDE configs
      mod.rs                   BUILTINS, builtin_by_id, raw read/write/validate; re-exports
      types.rs                 ← MERGED schema.rs + resolved.rs (+ ClientDef stays in mod.rs)
      paths.rs render.rs detect.rs project.rs io.rs error.rs
    db/
      mod.rs                   actor loop + Db handle + module decls
      dispatch.rs              ← NEW: the match DbCmd dispatch, extracted from mod.rs
      servers.rs types.rs skills.rs clients.rs commands.rs audit.rs
      migrations.rs migrations/0001_init.sql approvals.rs settings.rs oauth_drafts.rs
    skills/
      mod.rs                   thin: module decls + re-exports
      manager.rs               ← NEW: SkillManager impl, extracted from mod.rs
      discover.rs sync.rs
    paths.rs                   ← moved in from src/paths.rs (10/12 consumers are daemon)
    wizard_input.rs            ← MERGED paste_parser.rs + server_validators.rs
    cascade.rs ipc.rs events.rs errors.rs onboarding.rs retention.rs secrets.rs time_util.rs

  installer/                   unchanged
    mod.rs systemd_linux.rs launchd_macos.rs scheduled_task_windows.rs
```

Top level reads as a sentence: **slint** (markup) · **window** (GUI Rust) · **os** (desktop
integration) · **client** (IDE catalog) · **daemon** (core) · **installer** — plus the two crate
roots and two foundational files.

## 3. Move / rename map

| From | To | Notes |
|---|---|---|
| `src/ui/` | `src/slint/` | folder rename; update `build.rs` path |
| `src/ui.rs` | `src/window/view.rs` | the generated bindings + tab consts |
| `src/glue/` | `src/window/` | binary GUI controller |
| `src/glue/callbacks.rs` | `src/window/callbacks/mod.rs` | fan-out installer |
| `src/glue/callbacks_servers.rs` | `src/window/callbacks/servers.rs` | drop `callbacks_` prefix |
| `src/glue/callbacks_ide.rs` | `src/window/callbacks/ide.rs` | |
| `src/glue/callbacks_misc.rs` | `src/window/callbacks/misc.rs` | |
| `src/glue/callbacks_log.rs` | `src/window/callbacks/log.rs` | |
| `src/glue/{refresh,wizard,skills,util}.rs` | `src/window/{refresh,wizard,skills,util}.rs` | |
| `src/gui/` | `src/os/` | OS/desktop integration |
| `src/daemon/clients/{6 IDE files}.rs` | `src/client/*.rs` | the static defs |
| `src/daemon/clients/example.rs` | `src/client/example.rs` | copy-template |
| `src/daemon/clients/` (remainder) | `src/daemon/client_config/` | the engine |
| `src/daemon/clients/schema.rs` + `resolved.rs` | `src/daemon/client_config/types.rs` | merge |
| `src/daemon/http_server.rs` | `src/daemon/downstream/http_server.rs` | |
| `src/daemon/mcp_proxy.rs` | `src/daemon/downstream/proxy.rs` | drop `mcp_` prefix |
| `src/daemon/approval.rs` | `src/daemon/downstream/approval.rs` | |
| `src/daemon/config_toml.rs` | `src/daemon/config/toml.rs` | drop `config_` prefix |
| `src/daemon/config_watcher.rs` | `src/daemon/config/watcher.rs` | drop `config_` prefix |
| `src/paths.rs` | `src/daemon/paths.rs` | foundational → daemon |
| `src/daemon/paste_parser.rs` + `server_validators.rs` | `src/daemon/wizard_input.rs` | merge |

Use `git mv` for every move to preserve blame.

## 4. Code-level refactors (the "everything" extras)

- **SkillManager extraction.** `daemon/skills/mod.rs` (594 LOC) → keep module decls + `pub use`
  re-exports in `mod.rs`; move the `SkillManager` impl into `skills/manager.rs`. Mirrors
  `upstream/manager.rs`. Re-export `SkillManager` from `mod.rs` so external paths are unchanged.
- **DB dispatch split.** `daemon/db/mod.rs` (446 LOC) → keep `actor_loop`, the `Db` handle, and
  module declarations in `mod.rs`; move the `match DbCmd { … }` dispatch into `db/dispatch.rs`
  (a single `dispatch(conn, cmd)` fn called from `actor_loop`). Internal only — no consumer churn.
- **schema + resolved → types.** Merge `client_config/schema.rs` (`ConfigSpec`, `ConfigSchema`,
  `ConfigFormat`) and `resolved.rs` (`ResolvedClient`) into `client_config/types.rs`. Re-export
  `ConfigSpec`/`ConfigFormat`/`ConfigSchema`/`ResolvedClient` from `client_config/mod.rs` so the
  IDE def files and other consumers import from the crate-stable root.
- **paste_parser + server_validators → wizard_input.** Both are Add-Server wizard input handling
  (parse pasted config; validate name + clamp lifecycle). Combine into `daemon/wizard_input.rs`
  (~310 LOC, under the soft limit). *Judgment call — flagged; revert to two files if review prefers.*

## 5. Mechanical change checklist

**build.rs** — `slint_build::compile("src/ui/main.slint")` → `"src/slint/main.slint"`; update the
`//!` header path reference. (No other path in the repo references the markup dir.)

**Cargo.toml** — no change. `main.rs`/`lib.rs` stay at root; no `[[bin]]`/`[lib]` needed.

**`src/lib.rs`** module decls:
- `pub mod gui;` → `pub mod os;`
- remove `pub mod paths;` (now declared in `daemon/mod.rs`)
- add `pub mod client;`
- `atomic_io`, `daemon`, `installer`, `runtime` unchanged

**`src/main.rs`** module decls + imports:
- `mod ui;` removed; `pub(crate) mod glue;` → `pub(crate) mod window;`
- `use ui::MainWindow` → `use window::MainWindow` (re-exported from `window/mod.rs`)
- `conduit::gui::*` → `conduit::os::*`
- `glue::install_callbacks(...)` → `window::callbacks::install_callbacks(...)`
- `conduit::paths` → `conduit::daemon::paths`

**`src/daemon/mod.rs`** module decls:
- remove `http_server`, `mcp_proxy`, `approval` → add `pub mod downstream;`
- remove `config_toml`, `config_watcher` → add `pub mod config;`
- remove `clients` → add `pub mod client_config;`
- remove `paste_parser`, `server_validators` → add `pub mod wizard_input;`
- add `pub mod paths;`

**Import rewrites (crate-wide, mechanical):**

| Old path | New path |
|---|---|
| `crate::glue` / `glue::` | `crate::window` / `window::` |
| `glue::callbacks_servers` (etc.) | `window::callbacks::servers` (etc.) |
| `crate::ui::` / `ui::` (bindings) | `crate::window::view::` (or re-exported `window::`) |
| `conduit::gui` / `crate::gui` | `conduit::os` / `crate::os` |
| `daemon::clients::{render_mcp, resolve_client, BUILTINS, builtin_by_id, ClientDef, …}` | `daemon::client_config::…` |
| `daemon::clients::schema::*` / `resolved::*` | `daemon::client_config::types::*` |
| `daemon::http_server` | `daemon::downstream::http_server` |
| `daemon::mcp_proxy` | `daemon::downstream::proxy` |
| `daemon::approval` | `daemon::downstream::approval` |
| `daemon::config_toml` | `daemon::config::toml` |
| `daemon::config_watcher` | `daemon::config::watcher` |
| `crate::paths` / `conduit::paths` | `crate::daemon::paths` / `conduit::daemon::paths` |
| `daemon::paste_parser` / `daemon::server_validators` | `daemon::wizard_input` |

**The 6 IDE def files** (now in `src/client/`): `super::ClientDef` → `crate::daemon::client_config::ClientDef`;
`super::paths::PathSpec` → `crate::daemon::client_config::paths::PathSpec`;
`super::schema::{ConfigFormat, ConfigSpec}` → `crate::daemon::client_config::{ConfigFormat, ConfigSpec}`
(via the `mod.rs` re-exports). `BUILTINS` (in `client_config/mod.rs`) references `crate::client::*::DEF`
— this intra-crate cycle is allowed by Rust.

**Tests** to update (import paths only): `cascade.rs`, `client_detect.rs`, `client_writer.rs`,
`skills_paths.rs`, `notifications.rs`, `servers_crud.rs`, `policy_lookup.rs`, plus any `http_pause`,
`oauth_pkce` helpers referencing moved modules.

## 6. Execution order (each phase ends green: `cargo check`)

1. **slint/ rename** — `git mv src/ui src/slint`; update `build.rs`. `cargo check`.
2. **gui → os** — `git mv src/gui src/os`; update `lib.rs`, `main.rs`, internal `crate::gui`. `cargo check`.
3. **glue + ui.rs → window/** — create `window/`, `git mv` files in (callbacks → `callbacks/`,
   `ui.rs` → `view.rs`); write `window/mod.rs`; rewrite `main.rs` + internal refs. `cargo check`.
4. **client catalog lift** — `git mv` the 6 defs + `example.rs` to `src/client/`; rename
   `daemon/clients` → `daemon/client_config`; add `pub mod client` to `lib.rs`; fix def imports +
   `BUILTINS`. `cargo check`.
5. **daemon groupings** — create `downstream/`, `config/`; `git mv` files in (`mcp_proxy`→`proxy`,
   `config_toml`→`toml`, `config_watcher`→`watcher`); move `paths.rs` into `daemon/`. `cargo check`.
6. **code refactors** — SkillManager → `manager.rs`; DB dispatch → `dispatch.rs`; merge
   `schema`+`resolved`→`types.rs`; merge `paste_parser`+`server_validators`→`wizard_input.rs`. `cargo check`.
7. **tests** — update test imports. `cargo nextest run`.

Doing it phase-by-phase keeps each `cargo check` diff small and the failure surface local.

## 7. Verification (must all pass before claiming done)

- `cargo check`
- `cargo clippy --all-targets -- -D warnings`  (no new `#[allow]` without a `// reason:`)
- `cargo nextest run`  (or `cargo test`)
- `cargo fmt`
- `cargo run` — window opens, tabs render, tray appears (sanity that the Slint path + glue rewire work)

## 8. Risk & sequencing

- **7 live branches** besides `main` (`feat/approvals-popup`, `feat/rmcp-oauth`, `fix/oauth-bug-fixes`,
  `pr-13`, `refactor/quality-improvements`, `refactor/split-files-and-headers`, and current
  `db-rework-v2`). A mass `git mv` forces each to rebase across renamed paths and resets `git blame`
  on moved files. **Mitigation:** land this as a single, isolated refactor commit/PR on a branch cut
  from `main`, *after* the in-flight branches merge — ideally announced so others rebase once.
- **Current uncommitted edits** touch `clients/render.rs`, `clients/error.rs`, `glue/wizard.rs` —
  the exact files this plan moves. Commit or stash those first; don't tangle them with the move.
- **Slint path** is the one silent-failure point: if `build.rs` isn't updated in lockstep with the
  `ui/`→`slint/` rename, the build fails opaquely at codegen. It's step 1 for that reason.
- Prefer `git mv` everywhere (history) and keep moves and content edits in separate commits where
  practical (the code refactors in §4 are content edits; do them as their own commit after the moves).

## 9. Explicitly NOT changing

- `daemon/` keeps its name (no `core`/`engine` rename).
- No `app/` binary umbrella; `main.rs`/`lib.rs` stay at root.
- `db/types.rs`, `daemon/errors.rs`, `db/migrations/0001_init.sql`, `slint/theme.slint` stay where
  they are — they carry behaviour or are Slint-coupled; moving them harms cohesion.
- `db/clients.rs` keeps its name — it's the `client` *table* CRUD, unrelated to the `client_config`
  engine or the `client/` catalog (three distinct, correctly-named "client" things).
- `runtime.rs`, `atomic_io.rs` stay at root (cross-cutting; moving inverts deps).
```
