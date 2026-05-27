# Conduit

MCPs and skills, synced to every IDE. Single Rust binary using Slint for the GUI. See [specs/conduit-v0.md](specs/conduit-v0.md) for the full spec.

# Tech

- Rust stable, edition 2021. Single crate at root. Toolchain pinned in `rust-toolchain.toml`.
- GUI: Slint 1.x (`.slint` markup in `src/ui/`, Rust glue in `src/gui/`). No webview, no Node.
- HTTP server: axum on tokio.
- MCP wire: `rmcp = "~1.7"` (tilde — pin minor, allow patches).
- Storage: rusqlite (single `state.db` w/ audit table inside), OS keyring for secrets, `config.toml` for user-tunable settings.
- pnpm/npm/uv are used at runtime to install Node/Python MCP *upstreams* into `<config_dir>/conduit/installs/`. They are NOT used to build Conduit itself.

# Build & verify

- Rust check (fast): `cargo check`
- Lint: `cargo clippy --all-targets -- -D warnings`
- Format: `cargo fmt`
- Tests: `cargo nextest run` (or `cargo test`)
- Run: `cargo run`

Run lint + tests before claiming a task complete. If lints fail, fix the code — do NOT add `#[allow(...)]` unless you can explain *why* in a `// reason:` comment.

# When in doubt about any library, use context7

Rust crate APIs churn (axum 0.6 → 0.7 → 0.8, rmcp 1.x still moving, Slint less LLM training data than React). The Rust hallucination rate for AI is ~25% — methods that don't exist, signatures from the wrong version, plausible-sounding inventions.

**Before writing code that calls into any crate** — Slint especially, rmcp, axum, tokio, rusqlite, keyring, notify-rust — query context7:

- `mcp__context7__resolve-library-id` with the library name → docs ID
- `mcp__context7__query-docs` with that ID + focused query

If context7 doesn't help, say "I don't know the exact API" and read the source on docs.rs. Do not invent APIs. The compiler will catch hallucinations that don't compile; it can't catch ones that happen to type-check but mean something else.

# Layout

| Path | Purpose |
|---|---|
| `src/main.rs` | Entry: starts tokio runtime + Slint event loop |
| `src/daemon/` | axum HTTP server, MCP routing, policy, audit, upstream lifecycle |
| `src/daemon/upstream/` | MCP server spawn / lifecycle / install (pnpm/uv) |
| `src/daemon/ide_writer/` | Per-IDE config writers (5 IDEs × 3 OSes) |
| `src/gui/` | Slint Rust glue: tray, window, notifications, callback handlers, models, event bridge |
| `src/installer/` | systemd / launchd / scheduled-task install at first run |
| `src/ui/` | `.slint` markup: `main.slint`, `theme.slint`, `components/`, `tabs/` |
| `assets/` | Bundled fonts (Anton, Space Grotesk, Space Mono) + tray icons |
| `build.rs` | `slint_build::compile("src/ui/main.slint")` |
| `docs/` | Spec, design system, deferred-feature docs |

# Architectural contract (read this before writing any code)

Three patterns prevent ~80% of the footguns in this stack. Violations break in subtle, runtime-only ways. Treat as load-bearing.

## 1. Thread-affinity contract

| Thread | Owns | Never does |
|---|---|---|
| Main thread | Slint event loop (`app.run()`) | tokio current_thread runtime, blocking calls, DB access |
| Worker thread (dedicated `std::thread`) | Multi-thread tokio runtime | Slint property mutation |
| DB-actor thread | The single `rusqlite::Connection` | Anything async |

Cross-boundary calls use only three primitives:
- **Worker → UI:** `slint::Weak::upgrade_in_event_loop(|ui| ...)`. Tasks hold `Weak<MainWindow>`, never the strong handle.
- **UI callback → worker:** `tokio::runtime::Handle::spawn(async move { ... })`.
- **Anything → DB:** `mpsc::Sender<DbCmd>` w/ `oneshot::Sender<Result<T>>` reply embedded in each command.

Violations: `current_thread` tokio on the Slint thread will hang. Direct UI mutation from tokio tasks will fail to compile (good) or panic at runtime if you reach for `unsafe` (very bad). `Arc<Mutex<Connection>>` will deadlock under contention.

## 2. DB-actor pattern

```rust
enum DbCmd {
    GetServer { name: String, reply: oneshot::Sender<Result<ServerRow>> },
    InsertAudit { row: AuditRow, reply: oneshot::Sender<Result<()>> },
    // ...
}
```

A single `std::thread` owns the `Connection`, receives `DbCmd` via `mpsc::Receiver`, dispatches synchronously, replies via `oneshot`. No pool, no `Arc<Mutex<Connection>>`, no `spawn_blocking` sprinkled at call sites.

Why: `rusqlite::Connection` is `!Send + !Sync` and cannot cross `.await`. Pooling is overkill for a GUI-app traffic profile. The actor pattern makes prepared-statement cache amortize for free, runs WAL+busy_timeout configuration exactly once, and sequences migrations naturally.

## 3. CancellationToken everywhere

Hand every long-lived task a `tokio_util::sync::CancellationToken` clone. Axum handlers `select!` on it. Child-process supervisors await it. Slint's window-close callback fires it.

Shutdown sequence: Slint `app.run()` returns → fire token → `runtime.shutdown_timeout(Duration::from_secs(5))`. Never rely on `Drop` of `Runtime` (blocks main thread indefinitely on stuck `spawn_blocking` tasks).

# Slint ↔ Rust rules

- Every UI-triggered action is a Slint `callback` declared in `.slint` and wired in Rust via `app.on_<name>(...)`. Missing wire = silent no-op when UI invokes.
- All UI mutations from non-UI threads go through `weak.upgrade_in_event_loop(|ui| ui.set_*(...))`. `.ok()` the result — UI may have shut down.
- **`slint::invoke_from_event_loop` and `Weak::upgrade_in_event_loop` are fire-and-forget**. They return once the closure is queued, NOT once it runs. The closure runs later on the UI thread. If you need a reply, embed a `tokio::sync::oneshot::Sender` in the closure and `.await` it on the caller side.
- List models: use `Rc<VecModel<T>>` once, mutate in place via `push` / `set_row_data` / `remove` / `set_vec`. Never reassign the property to a new model on each update — wasteful re-render.
- Coalesce high-frequency updates upstream of `invoke_from_event_loop`. Post at most one UI update per ~16ms.
- Non-trivial UI: `slint-build` in `build.rs` + `slint::include_modules!()`, not the `slint!` macro.

# Async / tokio rules

- **Exactly one tokio runtime per process.** Build it explicitly on a dedicated `std::thread`. Never let a library construct one.
- **Multi-thread runtime, not `current_thread`.** Slint owns the main thread; tokio gets its own.
- `tokio::select!` branches **must be cancel-safe**. `read_exact`, `write_all`, `read_to_end`, `read_to_string` are NOT cancel-safe — wrap in `tokio::spawn` and `select!` on the join handle.
- Inside `async fn`, only use `tokio::fs`, `tokio::time`, `tokio::process`, `tokio::io`. For unavoidable sync calls (rusqlite, keyring, password hashing, std-fs at startup), use `tokio::task::spawn_blocking`.
- **Every `tokio::spawn` inside a loop requires bounded concurrency** — `JoinSet` + `Semaphore::acquire_owned()`, OR `StreamExt::buffer_unordered(N)`. Document the bound in a comment.
- **Unbounded channels (`mpsc::unbounded_channel`) are banned without written justification.** Default to bounded `mpsc::channel(N)`.
- Long-poll/SSE handlers (the 5-min approval hold) MUST `select!` on the shared `CancellationToken` AND a hard timeout. Wrap `axum::serve::with_graceful_shutdown` in `tokio::time::timeout`.

# Axum rules

- Body-consuming extractor (`Json`, `Form`, `Bytes`, `Multipart`, `Request`) is the **last** handler arg. `State<Arc<AppState>>` is first.
- One `AppError` enum implementing `IntoResponse` with `From<...>` for every error source. Handlers return `Result<_, AppError>`. No `.unwrap()` in handlers.
- SSE handlers always set `.keep_alive(...)` — idle connections die to NAT/proxies after ~60s.

# rusqlite rules

- `Connection` never crosses `.await`. Use the DB-actor (see above) — no exceptions.
- `PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;` set once at Connection open.
- Use `Connection::prepare_cached` (per-connection cache amortizes with the long-lived actor connection).
- Migrations run synchronously on startup before any other thread can read. Use `PRAGMA foreign_keys=ON; PRAGMA foreign_key_check;` after each.
- Always use `rusqlite` `bundled` feature — distributed binary, no system-SQLite dep.

# Error handling

- **`thiserror`** for library-style modules with typed error enums (`daemon/upstream/`, `daemon/db/`, etc.). Erasing error types via `anyhow` kills the ability to match on specific failures.
- **`anyhow`** only at bin/handler boundaries where errors are surfaced to the user or logged. Not in core logic.
- **Never `Result<T, Box<dyn std::error::Error>>`** in a return type.
- **No `.unwrap()` / `.expect()` in non-test code** — `clippy::unwrap_used` is `deny`. Tests are allowed via `#[cfg(test)] #![allow(...)]`.
- **No `panic!()`** in library/daemon code. Return `Result`. Panics belong in `main.rs` startup or `debug_assert!`.

# Cross-OS

- Paths: use `dirs::config_dir().join("conduit")` — never hardcode `~/.config/...`, `~/Library/...`, `%APPDATA%\...`.
- Windows subprocess spawns: set `CommandExt::creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP)`. Wrap in a helper used everywhere we spawn.
- Child MCP servers: process group on Unix (`setsid` via `nix` or `process-wrap` crate), Job Object on Windows. `kill_on_drop(true)` alone does NOT kill grandchildren (Node/Python workers leak).
- One task reading child stdout, one reading stderr — pipe buffers (~64KB) fill and deadlock the child if unread.
- Linux keyring requires `gnome-keyring` / `KWallet` running. Catch `keyring::Error::NoStorageAccess` / `PlatformFailure` and fall back to a file-based encrypted store.
- macOS unsigned-binary notification action buttons can silently fail. Documented in README. Don't invent workarounds.

# Rust patterns

- Match exhaustively on protocol/state enums. Avoid `_ =>` catch-all on types that gain variants.
- `tracing` for all logging — never `println!` / `eprintln!` (lint enforces).
- Prefer `&str` over `String` for parameters. Return `String` or `Cow<'_, str>` when ownership is unclear; don't clone defensively.
- Avoid `.clone()` to satisfy the borrow checker — restructure scope first.
- `std::sync::Mutex` over `tokio::sync::Mutex` by default. Switch only if guard must cross `.await`. `clippy::await_holding_lock` enforces.
- Hold lock guards for minimum scope. Drop before any `.await` or any Slint API call.
- Public APIs use owned types; borrow only inside implementations. No lifetime parameters in public signatures unless load-bearing.
- Enumerate `tokio` features in `Cargo.toml` — `"full"` is a smell.

# Code quality

- Match existing style. Don't reformat or rename outside the requested change.
- Don't add features, abstractions, or "flexibility" beyond what the task asks for.
- Don't add error handling for impossible conditions. Trust internal code.
- If a fix is 5 lines, the diff is 5 lines. No surrounding cleanup unless asked.
- Run `cargo check` after every edit. Don't end a turn with a red build.

# Comments — minimal, on purpose

- **Default: write no comments.** Names and types carry the meaning.
- Add a comment only when the *why* is non-obvious from the code: a hidden constraint, a workaround for a specific bug, a subtle invariant, behavior that would surprise a reader.
- Never explain *what* the code does. Never write "used by X" or "added for Y flow" — those rot.
- Doc-comments (`///`) on public APIs are fine; one-line summary, no marketing prose.
- No multi-line comment blocks. If a comment needs a paragraph, it's a missing function/type abstraction or a missing entry in `docs/`.

# Project-specific gotchas

- `lifecycle_seconds` is a single int: `0` = ephemeral, `-1` = persistent, `N > 0` = idle TTL seconds. Auto-detection upgrades only (never shortens). See `src/daemon/upstream/lifecycle.rs`.
- IDE auto-write tracks every entry in `managed_ide_configs` table. Removing an IDE must purge entries via this table + URL regex match (entries pointing to `localhost:9876/<key>/<server>/mcp`). NO inline `_mcpRouter: { managed: true }` marker — some IDEs schema-validate and would reject. Don't rewrite IDE config files blindly.
- `config.toml` is authoritative for user settings; watched via `notify` crate (5s debounce). Settings tab writes back to TOML, not DB.
- `state.db` is the single SQLite file. Audit log is a TABLE inside; don't reintroduce `audit.db`.
- Install Node MCP upstreams via `pnpm add --prefix <installs_dir>` then spawn binary directly. **Never `npx -y` at runtime** — 5+ second cold-fetch tax.
- Pin `rmcp = "~1.7"` (tilde), not `"^1"`. MCP spec moves; treat upgrades as deliberate work.
- Never `serde(deny_unknown_fields)` on MCP message types — kills forward compatibility with newer spec versions.

# Do NOT

- Add a Slint callback in `.slint` without wiring `.on_*` in Rust (silent no-op).
- Mutate Slint properties from tokio task without `slint::Weak::upgrade_in_event_loop`.
- Hold `Mutex` guard across `.await` or any Slint API call.
- Spawn unbounded `tokio::spawn` in a loop.
- Use `mpsc::unbounded_channel()` without written justification.
- Use blocking I/O (`std::fs`, `std::thread::sleep`) inside `async fn` — use `tokio::*` equivalents or `spawn_blocking`.
- `.unwrap()` / `.expect()` / `panic!()` / `todo!()` / `unimplemented!()` / `dbg!()` in non-test code.
- `println!` / `eprintln!` — use `tracing`.
- `_ =>` catch-all on protocol/state enums.
- Pull in `openssl` / `async-std` / `smol` — banned in `deny.toml`. Use `rustls` + tokio.
- `Result<T, Box<dyn std::error::Error>>` in return types.
- `kill_on_drop(true)` as the only child-cleanup mechanism — leaks grandchildren.
- Reintroduce React, Tauri, or any webview-based GUI dep. The spec chose Slint.
- Write code that "might be useful later." Build only what the current task needs.
