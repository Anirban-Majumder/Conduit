# Plan — rmcp OAuth migration, file splits, file headers

Three sequential PRs. Each merges before the next starts (D16). Full rationale for every choice is in [decision.md](decision.md) (referenced as D1–D17 below).

1. **PR 1 — `feat/rmcp-oauth`**: replace hand-rolled OAuth with rmcp's `auth` layer.
2. **PR 2 — `refactor/split-large-files`**: split files >500 lines (measured *after* PR 1).
3. **PR 3 — `chore/file-headers`**: add `//!` module headers + comment cleanup.

---

## Verified foundation — rmcp 1.7 `transport::auth` API

Confirmed present in `rmcp-1.7.0/src/transport/auth.rs` (codex uses the same shape on 0.8.2). Exact import paths to be confirmed at compile time (codex used `rmcp::transport::auth::OAuthState`, `rmcp::transport::AuthorizationManager`).

**Login (interactive) sequence:**
```
OAuthState::new(server_url, Some(http_client))          // discovers metadata (PRM → direct), no endpoint guessing
  .start_authorization(&scopes, &redirect_uri, Some("Conduit"))   // DCR (or pre-registered client_id)
  .get_authorization_url()                              // adds resource=<base_url> automatically
  // → open browser, run loopback, capture (code, state)
  .handle_callback(&code, &state)                       // token exchange + CSRF state validation
  .get_credentials()                                    // → (client_id, OAuthTokenResponse)
```

**Runtime (saved server) sequence:**
```
AuthorizationManager::new(url) + set_credential_store(KeyringCredentialStore{account})
  + with_client(reqwest) + discover_metadata()/set_metadata()
  → AuthClient::new(adapter, manager)
  → StreamableHttpClientTransport::with_client(auth_client, cfg)
```
`AuthClient` auto-attaches the token on every request and auto-refreshes via `manager.get_access_token()` (loads at auth.rs:1239) / `refresh_token()` (persists at auth.rs:1328), serialized by the manager's internal `Mutex`.

> **Implementation checkpoint (the one tricky spot):** confirm the runtime interplay between `set_credential_store` (our keyring), `set_credentials` / `configure_client` (client_id), and `discover_metadata` (token_endpoint) so that `get_access_token` loads the stored token and `refresh_token` persists back through our store. Validate with the `CredentialStore` round-trip test before wiring `manager.rs`.

---

## Phase 1 — rmcp OAuth migration (PR 1)

**Goal:** delete the hand-rolled discovery/DCR/PKCE/exchange/refresh + `RefreshCoordinator`; drive login through `OAuthState` and runtime through `AuthClient`; persist one credential blob to the OS keyring via a `CredentialStore` adapter. (D1, D2, D2a, D4)

### 1.1 New: keyring-backed `CredentialStore` (D4, D5)
`src/daemon/upstream/oauth_store.rs`:
- `struct KeyringCredentialStore { account: String, cache: std::sync::RwLock<Option<StoredCredentials>> }`.
- `#[async_trait] impl rmcp::transport::auth::CredentialStore`:
  - `load()` → return cache if `Some`; else `spawn_blocking` keyring read of `<account>:cred`, parse JSON → `StoredCredentials`, populate cache.
  - `save(creds)` → update cache + `spawn_blocking` keyring write of the JSON blob.
  - `clear()` → empty cache + `spawn_blocking` keyring delete.
- Cache exists so we don't hit Secret Service on every MCP request; `spawn_blocking` satisfies the thread-affinity rule.

### 1.2 `secrets.rs` — one blob per account (D5, D6)
- Replace `store_oauth_access/refresh/client_secret/meta` + their fetchers with `store_oauth_cred(account, &StoredCredentials)`, `fetch_oauth_cred(account) -> Option<StoredCredentials>`, `delete_oauth_cred(account)`. One keyring entry: `conduit-oauth / <account>:cred`.
- `promote_oauth_tokens(draft, server)` → copy the single blob, delete draft.
- Add one-time `delete_legacy_oauth_entries(account)` removing the old `:access`/`:refresh`/`:client_secret`/`:meta` suffixes; call it during the existing startup draft-purge path (D6).
- Env-secret functions unchanged.

### 1.3 `probe.rs` — login via `OAuthState` (D2)
- Keep: 401 detection (the connect-error string match, already verified), `bind_callback_listener`, `open::that`, the `ProbeEvent` progress sink, the 5-min timeout + HTML response.
- Replace `run_oauth_pre` + inline authorize/exchange with the **login sequence** above.
- `wait_for_callback` simplified: return `(code, state)`; rmcp validates state in `handle_callback`. Remove `fresh_state` / `state_matches`.
- After `get_credentials()`: build `StoredCredentials`, `store_oauth_cred(draft, …)`.
- Non-DCR hard-fail: if discovery yields no registration support and no client_id, fail with a clear message + the D7 code comment.

### 1.4 `manager.rs` + `http.rs` — runtime via `AuthClient` (D4)
- Delete `RefreshCoordinator` (from `oauth.rs`), `ensure_fresh_access_token` + `refresh_with_coordinator` (`http.rs`), and the `refresh_coord`/`refresh_http` fields on `UpstreamManager`.
- `ensure_running` HTTP branch distinguishes three cases:
  1. **OAuth server** (a `<server>:cred` blob exists) → build the **runtime sequence** (AuthClient transport).
  2. **Static bearer** (env `MCP_BEARER_TOKEN` / `Authorization` header) → keep `cfg.auth_header(token)` (the already-committed double-`Bearer` fix — still load-bearing here).
  3. **No auth** → plain transport.
- > Correction to D17: the manual `auth_header` path is **not** fully deleted — it remains for static-bearer servers. Only the *OAuth* path moves to `AuthClient`.

### 1.5 DB — drop `oauth_*` columns (D8)
- Remove `oauth_authority` / `oauth_client_id` / `oauth_scopes` from `db/types.rs::Server`, `ProbeResult` (`probe.rs`), and `main.rs` staging/save.
- `db/mod.rs`: drop the three `ensure_column` calls; remove from the `UpsertServer` SELECT/UPSERT column lists; **renumber the `row.get(n)` indices** in the server row mapper (currently 17/18/19) and the trailing reads. Add `ALTER TABLE servers DROP COLUMN` for each.
- Keep `last_auth_error_msg`/`_at` + `NeedsAuth` (load-bearing).

### 1.6 Deletions
- Most of `src/daemon/upstream/oauth.rs`: `Pkce`, `fresh_state`, `state_matches`, `build_authorize_url`, `DcrRequest`/`register_client`, `exchange_code_for_tokens`, `refresh_tokens`, `OauthChallenge`/`parse_www_authenticate`, all discovery fns (`fetch_authorization_server_metadata`, `fetch_auth_server_metadata_direct`, `generate_fallback_endpoints`, …), `RefreshCoordinator`. File likely removed entirely (replaced by `oauth_store.rs`).
- `tests/oauth_pkce.rs`: delete tests for removed fns; keep/port only what still applies.

### 1.7 Docs + comments (D7, D15)
- README: HTTP OAuth MCP servers must support Dynamic Client Registration; manual/pre-registered `client_id` not yet supported.
- Code comment at the non-DCR hard-fail noting `OAuthClientConfig::new(client_id, redirect_uri)` is the deferred path.

### 1.8 Tests + acceptance gate (D9)
- **Hard gate:** `cargo clippy --all-targets -- -D warnings` + `cargo nextest run` green.
- New tests: `KeyringCredentialStore` round-trip (load/save/clear + cache hit), callback-URL parse, `#[ignore]`'d live Sentry **discovery** smoke test (kept in-repo).
- Out-of-band (not gating): manual Sentry round-trip (add → browser → tools/list → call tool → **restart → reconnects w/o re-login**); Auth0-style scope test if credentials available — note in PR if unverified.

---

## Phase 2 — split files >500 lines (PR 2)

Measure **after** PR 1 lands (D10). Soft target <500; ~1000 hard ceiling for cohesive files. Pure mechanical moves — no logic changes, `cargo nextest` green before/after each split, `pub(crate)` adjusted only to compile (D13). `.sql` excluded.

### `main.slint` (2375) → `ui/` tree, ~12 files (D10, D11)
- `ui/main.slint` — `MainWindow` root + imports only.
- `ui/components/primitives.slint` — `TabButton`, `StatusDot`, `PillButton`, `EmptyState`, `SettingLine`, `LabeledInput`, `LabeledTextArea`.
- `ui/components/modal.slint` — `ModalShell`, `WizardStepIndicator`.
- `ui/tabs/{approvals,servers,add_server_dialog,skills,editors,log,settings,help}.slint` (approvals carries `ToolPolicyRowItem`/`ToolPolicyTable`; servers carries `HealthCard`/`TargetToggle`).

### `main.rs` (~2276) → thin bootstrap + `gui/` submodules
- `main.rs`: `main()` + wiring only.
- `gui/callbacks/{wizard,servers,ide,policy,skills}.rs`; `gui/refresh.rs` (`refresh_*`, `push_initial_models`, `spawn_event_bridge`). `wizard.rs` already reshaped by Phase 1.

### `db/mod.rs` (~2584) → actor core + domain handlers
- `mod.rs`: `Db` handle, `actor_loop`, central `dispatch` (delegates).
- `db/commands.rs` (`DbCmd` enum), `db/migrations.rs` (`init_conn`, `migrate_legacy_ide_tables`, `ensure_policies_fk`, `ensure_column`, `restrict_db_perms`), `db/{servers,policy,approvals,audit,ides,skills,settings,oauth_drafts}.rs` handlers.

### `skills/mod.rs` (584) → extract `skills/repos.rs`
- Drop core under 500 (`discover.rs`/`sync.rs` already exist).

> Re-measure the full tree after PR 1; split whatever is then >500 by the same domain/concern principle. Files near the line (`clients/mod.rs` 495, `manager.rs` 478) left alone unless PR 1 pushes them over.

---

## Phase 3 — file headers + comment cleanup (PR 3)

### Headers (D14)
- Every source file under `src/` (incl. `build.rs`, `lib.rs`): `.rs` → `//!`, `.slint` → `//`, `.sql` → `--`.
- **3–6 lines typical, ~10 cap** for complex coordinators. One-line summary + 2–5 lines of orientation (purpose, collaborators, load-bearing invariant). Do **not** enumerate functions. Standardize existing `//`-style headers to `//!`.

### Comment cleanup (D15)
- Follow CLAUDE.md's policy exactly: strip *what*-comments + rotted/stale ones + commented-out code; **keep** *why*-comments (the `bearer_auth` double-prefix note, Cloudflare no-audience, thread-affinity warnings) and `///` API docs. Per-comment judgment, file-by-file, in the same pass as the header.

### CLAUDE.md amendments
- Comment section: every source file opens with a one-line+ `//!`/`//` module header — a standing exception to "default: no comments" and "no multi-line comment blocks", applying **only** to the top-of-file block; inline comments stay why-only.
- Code-quality section: soft <500-line target / ~1000 ceiling guideline (no CI enforcement — D12).

---

## Risks / deferred / open

- **Runtime `set_credentials`/`credential_store`/`discover_metadata` interplay** — the one spot to validate against rmcp 1.7 before wiring `manager.rs` (see checkpoint above).
- **Auth0 scope-narrowing** unverified unless a test account exists (D9) — note in PR 1 if so.
- **Scope upgrade** (`AuthorizationSession::for_scope_upgrade`, 403 `insufficient_scope`) — deferred; not wired.
- **Non-DCR servers** — deferred, documented (D7); `OAuthState` path supports it when wanted.
- **Exact rmcp import paths** — compiler-checked; confirm at implementation.
- **`db` row-index renumbering** after dropping columns — easy to get wrong; covered by `servers_crud` tests.
```
