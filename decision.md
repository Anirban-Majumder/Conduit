# Decision Log

Running record of architectural decisions for the rmcp-OAuth migration, file-split, and file-header work. Each entry: the question, the options weighed, and the chosen answer.

---

## Phase 1 — rmcp OAuth migration

### D1 — Migration scope
**Decision: (A) Full migration.**

Delete the hand-rolled OAuth (discovery, DCR, `www-authenticate` parsing, authorize-URL building, token exchange, `refresh_tokens`, `RefreshCoordinator`) and drive everything through `rmcp::transport::auth::AuthorizationManager` (interactive login) + `AuthClient` (runtime connect + auto-refresh). Keyring persistence via a `CredentialStore` adapter.

Rationale: the goal is to delete the ~1,000-line bug surface (source of the double-`Bearer` and latent duplicate-`resource` bugs). A partial/runtime-only migration would keep the discovery/DCR footguns and force two parallel auth paths. Pre-release on a dedicated branch → clean cut is acceptable. The Cloudflare/Auth0 quirk-preservation risk is handled as a verification task, not a reason to keep code.

### D2 — Interactive login flow structure
**Decision: (A) Keep our loopback + browser-open + Slint progress glue; swap only the protocol internals to `AuthorizationManager`.** Additionally: **use codex's structure/system as a reference and adopt any optimization/improvement worth taking.**

Consequence: rmcp's `oauth2`-based manager owns PKCE + CSRF `state` internally (verifier stashed in a `StateStore` keyed by CSRF token; `exchange_code_for_token` looks it up). So `wait_for_callback` stops generating/validating its own `state` — it returns the raw `code` + `state` and hands them to rmcp, which validates. `oauth::fresh_state` / `state_matches` are removed; the listener, browser-open, 5-min timeout, and HTML response stay.

### D2a — Codex reference findings + structural adoption
Studied codex's `rmcp-client` (`perform_oauth_login.rs`, `rmcp_client.rs`). Patterns:

- **Login** uses the high-level **`OAuthState`** wrapper, not raw `AuthorizationManager`: `OAuthState::new(url, Some(http_client))` → `start_authorization(scopes, redirect_uri, Some("Conduit"))` → `get_authorization_url()` → `handle_callback(code, state)` (exchange + state validation) → `get_credentials()` → `(client_id, token_response)`.
- **Pre-registered client_id**: when a `client_id` is provided, codex skips DCR via `OAuthClientConfig::new(client_id, redirect_uri).with_scopes(...)`.
- **Runtime**: `OAuthState::new` → `set_credentials(client_id, token_response)` → extract `AuthorizationManager` → `AuthClient::new(adapter, manager)` → `StreamableHttpClientTransport::with_client(auth_client, cfg)`. AuthClient auto-attaches+refreshes the token inline on every request.
- **Persistence**: codex does **NOT** implement rmcp's `CredentialStore` trait. It keeps its own store and bridges with `set_credentials`/`get_credentials`, plus an `OAuthPersistor.refresh_if_needed()` wrapper that persists refreshed tokens. This avoids async-trait-over-blocking-keyring friction.
- **Resource param**: rmcp's `get_authorization_url` already adds `resource=<base_url>`; codex only manually appends a `resource` when an explicit override is configured.

**Adopted (recommendation, low-controversy): drive the high-level `OAuthState` wrapper rather than raw `AuthorizationManager`.** Simpler, fewer call sites, proven by codex against Cloudflare-style servers.

Items that become explicit decisions below: pre-registered client_id (D7), persistence pattern (D4), scope-upgrade handling (deferred unless raised).

### D4 — Runtime keyring persistence
**Decision: (X) Implement a keyring-backed `CredentialStore` with a small in-memory cache.**

`load()` reads the keyring once → caches; `save()` writes cache + keyring; both via `tokio::task::spawn_blocking` (thread-affinity rule). Wired with `manager.set_credential_store(store)`. rmcp then auto-loads, auto-refreshes (serialized by the manager's internal `Mutex`), and auto-persists (confirmed in rmcp source: `get_access_token` loads at auth.rs:1239, `refresh_token` saves at auth.rs:1328).

Deletes: `RefreshCoordinator`, `ensure_fresh_access_token`, and all pre-op refresh calls in `manager.rs`. Rejected (Y) codex's seed+persistor pattern — proven but carries a wrapper + manual refresh calls we don't need.

### D5 — Keyring schema + draft lifecycle (consequence of D4)
**Decision: one JSON blob per account, replacing the 4-entry scheme.**

- Storage: serialize rmcp's `StoredCredentials { client_id, token_response, granted_scopes, token_received_at }` to a single keyring entry (e.g. `conduit-oauth / <account>:cred`). Removes today's `:access` / `:refresh` / `:client_secret` / `:meta` entries.
- Login (draft): `OAuthState` runs in-memory; `get_credentials()` → store one blob under the **draft UUID** account (our keyring write, via `spawn_blocking`).
- Save: `promote_oauth_tokens` copies the one blob `<draft>:cred` → `<server>:cred`, then deletes the draft entry.
- Runtime: `KeyringCredentialStore { account: <server> }` reads/writes the `<server>:cred` blob.

### D6 — Existing old-format tokens
**Decision: (A) Force re-login.** No migration code. Add a one-time cleanup that deletes stale old-format entries (`:access`/`:refresh`/`:client_secret`/`:meta`); `NeedsAuth` state prompts re-auth. Justified: pre-release, ~one user, format is days old.

### D7 — Pre-registered client_id (non-DCR servers)
**Decision: (A) Defer — but document the limitation explicitly.**

Stay DCR-only for now; hard-fail with a clear message on servers lacking a `registration_endpoint`. Requirements:
- **Code:** an explicit comment at the hard-fail site noting that `OAuthState` / `OAuthClientConfig::new(client_id, redirect_uri)` supports pre-registered clients and this is a deliberate deferral, not an oversight.
- **README:** note that HTTP OAuth MCP servers must support Dynamic Client Registration; a manual/pre-registered `client_id` is not yet supported.

No architectural lock-in (the `OAuthState` path already branches on an optional `client_id`).

### D8 — `servers.oauth_*` columns
**Decision: (A) Drop `oauth_authority`, `oauth_client_id`, `oauth_scopes`.**

Confirmed write-only (no behavioral/UI reader; `client_id` now lives in the keyring `:cred` blob). Remove from `db/types.rs`, `ProbeResult` (probe.rs), `main.rs` staging/save, and `ALTER TABLE servers DROP COLUMN` (bundled SQLite supports it). **Keep** `last_auth_error_msg`/`_at` — load-bearing for `NeedsAuth` ([health.rs:49](src/daemon/upstream/health.rs#L49)), the tray badge, and wizard display. Granted scopes, if ever needed for display, come from `StoredCredentials.granted_scopes`.

### D9 — Acceptance gate for the migration
**Decision: hard gate = (1) static checks + (2) automated tests.**

1. `cargo clippy --all-targets -- -D warnings` + `cargo nextest run` green.
2. Automated tests: keyring `CredentialStore` round-trip (load/save/clear + cache), callback-URL parsing, and a `#[ignore]`'d live-network Sentry **discovery** smoke test (kept in-repo).

Manual interactive Sentry login (add → browser → tools/list → call tool → restart → reconnects w/o re-login) and an Auth0-style scope-narrowing test are **not formal gates** (per user). They remain recommended out-of-band confirmation — the manual Sentry round-trip is the real-world proof of the persistence/refresh path, but won't block the migration in CI.

---

## Phase 2 — split files over 500 lines

### D10 — Split scope + threshold
**Decision: split all files >500 lines, measured *after* Phase 1 lands. Threshold is a soft target, not a hard limit.**

- Aim: **<500 lines for most files**; **~1000 lines hard ceiling** (don't over-fragment — a cohesive 600–900-line file is fine if splitting would scatter tightly-coupled logic).
- Measure post-Phase-1 (the migration deletes `oauth.rs`, shrinks `probe.rs`, and reshapes `main.rs`/`db/mod.rs`).
- Include `.slint`; exclude `.sql` (all tiny). Leave sub-500 files alone.
- **`main.slint` (2375): split aggressively — as many files as possible, per component / per tab.**

### D11 — Split strategy per file (behavior-preserving moves only)
**Decision: adopt the proposed decomposition.**

- **`main.slint` → `ui/` tree (~12 files):** `ui/main.slint` (just `MainWindow` + imports); `ui/components/primitives.slint` (`TabButton`, `StatusDot`, `PillButton`, `EmptyState`, `SettingLine`, `LabeledInput`, `LabeledTextArea`); `ui/components/modal.slint` (`ModalShell`, `WizardStepIndicator`); `ui/tabs/{approvals,servers,add_server_dialog,skills,editors,log,settings,help}.slint`. (`approvals` also holds `ToolPolicyRowItem`/`ToolPolicyTable`; `servers` holds `HealthCard`/`TargetToggle`.)
- **`main.rs` → thin bootstrap + `gui/` submodules:** `main.rs` keeps `main()`+wiring; `gui/callbacks/{wizard,servers,ide,policy,skills}.rs`; `gui/refresh.rs` (`refresh_*`, `push_initial_models`, `spawn_event_bridge`). `wizard.rs` is reshaped by Phase 1.
- **`db/mod.rs` → actor core + domain handlers:** `mod.rs` keeps `Db` handle + `actor_loop` + central `dispatch`; `db/commands.rs` (the `DbCmd` enum); `db/migrations.rs`; `db/{servers,policy,approvals,audit,ides,skills,settings,oauth_drafts}.rs` handler fns (dispatch delegates). One central match remains.
- **`skills/mod.rs` → extract `skills/repos.rs`** to drop the core under 500.

Principle: every `mod.rs` stays a thin coordinator; group by domain/concern; pure moves — no logic changes; tests stay green throughout.

### D12 — Enforce <500 in CI?
**Decision: No.** No automated line-count lint. Keep it a one-time cleanup + soft guideline (noted in `CLAUDE.md`'s code-quality section). A hard lint would fight the ~1000 cohesive-file exception.

### D13 — Splits are pure mechanical moves
**Decision: Yes, strictly mechanical.** No behavior/logic changes during Phase 2; `cargo nextest` green before and after each split; adjust `pub`/`pub(crate)` only as needed to compile; any real logic change rides in a separate commit so split diffs read as moves.

---

## Phase 3 — file headers + comment cleanup

### D14 — Header style + length
**Decision: multi-line `//!` module doc on every source file.**

- Style: `.rs` → `//!`; `.slint` → `//`; `.sql` → `--`. Scope: every source file under `src/` (incl. `build.rs`, `lib.rs`).
- **Length: 3–6 lines typical, ~10 hard cap** for complex coordinators. One-line summary + 2–5 lines of orientation: what lives here, how it fits its collaborators, any load-bearing invariant/gotcha. **Do not enumerate functions** (rots) — describe purpose and shape so a human/AI grasps the file from the header alone.
- Standardize existing `//`-style headers to `//!` while in each file.
- **CLAUDE.md amendment:** the multi-line module header is a standing exception to "default: no comments" and "no multi-line comment blocks" — but that exception applies **only** to the top-of-file `//!`/`//` block; inline comments still follow the minimal, why-only rule.

### D15 — Comment-cleanup aggressiveness
**Decision: (A) Follow CLAUDE.md's comment policy exactly.** Strip *what*-comments and rotted/stale ones ("used by X", "added for Y", commented-out code); **keep** *why*-comments (constraints, bug workarounds, subtle invariants — e.g. the `bearer_auth` double-prefix note, Cloudflare no-audience, thread-affinity warnings) and `///` API docs. Per-comment judgment, done file-by-file in the same pass as the header, as cleanup commits separate from logic changes.

---

## Process

### D17 — One-line double-`Bearer` fix disposition
**Decision: keep it — it stays load-bearing.** The fix is already committed (`3af8448`, merged via PR #1).

**Correction (confirmed with user):** Phase 1 does **not** delete the manual `auth_header` path entirely. Only the **OAuth** server path moves to `AuthClient`. **Static bearer-token** (non-OAuth) HTTP servers keep `cfg.auth_header(token)`, so the double-`Bearer` fix remains required for them. `manager.rs::ensure_running` HTTP branch therefore has three cases: OAuth (AuthClient) / static-bearer (auth_header) / no-auth (plain). See plan.md §1.4.

### D16 — Branch / PR structure
**Decision: (A) One PR per phase, sequential.** Phase 1 (`feat/rmcp-oauth`, or continue current branch) → merge → Phase 2 (`refactor/split-large-files`, measured post-Phase-1) → merge → Phase 3 (`chore/file-headers`). Forced by D10's "measure after Phase 1"; keeps risky Phase-1 logic out of move/comment churn for reviewable diffs.
