# Conduit Quality Refactor — Implementation Plan

> **Status:** Phase 0 ✅ + Phase 1 ✅ committed on 2026-05-30 (commit `8b8f84a`). Phase 2 ✅ committed on 2026-05-30. Phase 3 ✅ committed on 2026-05-31 (commit `51678ca`). Phase 4 pending.
> **Scope of analysis:** all of `src/` (~13.7k Rust LOC + 3.8k Slint LOC, ~90 files).
> **Goal stated by request:** make the codebase SOLID, DRY, extensible, and Google-level readable while preserving its architectural contracts.

---

## 0. Verdict (read this first)

**Conduit does not need a mega-rewrite.** Every subsystem audit independently confirmed that the *hard* architectural contracts are correctly and consistently enforced:

- **DB-actor pattern** — single `std::thread` owns the `!Send` `rusqlite::Connection`; all access via `mpsc<DbCmd>` + `oneshot`. No `Arc<Mutex<Connection>>`, no pool.
- **Thread-affinity** — Slint on main, multi-thread tokio on a dedicated worker, DB actor separate. Cross-boundary only via the three sanctioned primitives.
- **Error model** — `thiserror` in libs, `anyhow` only at boundaries, no `Box<dyn Error>` in return types, no `unwrap`/`panic` in core (with the two exceptions noted below).
- **CancellationToken** plumbing and `shutdown_timeout` sequence are correct.

A rewrite would put all of that load-bearing correctness at risk for **zero** benefit.

The real debt is the *second* quality bar: **DRY / SOLID / extensibility / readability**. Mechanical patterns are copy-pasted across the codebase (oneshot ceremony ×50, `now_unix` ×5, IDE enum-dispatch ×7 sites, callback clone-and-spawn ×50+, decision→color ×4 Slint tabs). This debt breaks nothing today but multiplies the cost of every new IDE, transport, DB op, callback, or tab. It is **low-regret, high-leverage** — exactly the kind worth paying down deliberately.

**Three genuine contract violations** are live and should be fixed before any cosmetic work:
1. `unbounded_channel` for notification actions (`src/main.rs:231-232`).
2. Unbounded per-event `std::thread` spawning in `src/gui/notifications.rs`.
3. **Unix child cleanup is `kill_on_drop`-only** (`src/daemon/upstream/stdio.rs:5-7`, 33). CLAUDE.md bans this exact pattern ("leaks grandchildren — Node/Python workers leak"). Windows already does it correctly via `JobObject`; the Unix arm has a standing `TODO` to adopt `process-wrap`'s `ProcessGroup` (already a dependency). This is a cross-platform *asymmetry*, not just debt — the bug is invisible on a Windows dev box.

**Cross-platform caveat that colors the whole plan:** Conduit is 5 IDEs × 3 OSes, and the most refactor-prone subsystems (`installer/`, `clients/`, `gui/notifications.rs`, `upstream/stdio.rs`, keyring) are heavily `#[cfg(...)]`-gated. The host compiler only type-checks the *active* target's arms, so a green `cargo check` on one OS proves nothing about the other two. Every slice below that touches `cfg`-gated code must be verified per-target (see §1) — and a "Cross-platform correctness" section is added that cuts across all phases.

---

## 1. How this plan is organized

Work is grouped into **phases ordered by dependency and risk**, not by subsystem. Each *slice* within a phase is independently shippable as one reviewable PR.

**Every slice ends with the same verification gate (host target):**

```
cargo check
cargo clippy --all-targets -- -D warnings
cargo nextest run        # or cargo test
```

**Slices that touch `#[cfg(...)]`-gated code additionally require a per-target check** — the host compiler skips inactive `cfg` arms, so a green host build does not cover the other two OSes. Affected slices are tagged **[cfg]** in the phase tables. The cross-OS gate is:

```
# TODO(human): fill in the project's actual cross-target verification commands.
# Decide the strategy that fits this repo's setup (dev box is win32):
#   - local cross-compile, e.g.  cargo check --target x86_64-apple-darwin
#                                 cargo check --target x86_64-unknown-linux-gnu
#     (needs the rustup targets + a linker/SDK — may not be feasible from Windows)
#   - WSL for the Linux arm + CI for the macOS arm
#   - rely entirely on a CI matrix (win/mac/linux) as the merge gate
# Whatever the choice: no [cfg]-tagged slice merges until all three OS arms compile + clippy-clean.
```

A slice is not "done" until that gate is green for every OS its diff can reach. No slice may end the turn with a red build on any target.

**Risk legend:** `low` = compiler-verified, behavior-preserving · `medium` = touches rendered output or shared signatures · `high` = concurrency-sensitive paths.

---

## 2. Phase plan

### Phase 0 — Correctness regressions ✅ DONE

> The only true architectural-contract violations in the audit. Smallest blast radius, highest correctness leverage. Land before any cosmetic work so the contract is clean.

| Slice | Files | Risk | Effort |
|---|---|---|---|
| **0.1 — Bound the notification action channel** | `src/main.rs` | medium | S |
| **0.2 — Cap notification-thread spawning** **[cfg]** | `src/gui/notifications.rs` | medium | M |
| **0.3 — Fix Unix grandchild leak in stdio transport** **[cfg]** | `src/daemon/upstream/stdio.rs` | high | M |

**0.1** — Replace `tokio::sync::mpsc::unbounded_channel` (`main.rs:231-232`) with a bounded `mpsc::channel(N)` (e.g. `256`). Document the bound in a `// reason:` comment. On full: log + drop oldest (back-pressure policy must be explicit).

**0.2** — `notifications.rs:74-78` (close-on-`ApprovalResolved`) and `notifications.rs:117-119` (Linux `wait_for_action`) spawn one `std::thread` per event. Under a burst of auto-approvals this is unbounded. Cap concurrency with a `tokio::sync::Semaphore` (or a small fixed worker pool). Document the bound per the loop rule. **Cross-platform note:** `wait_for_action` is the **Linux** notify-rust path; `notifications.rs:74-78` (close) is cross-cutting. The macOS/Windows notification paths do not block on `wait_for_action` and so are not the unbounded-spawn source — but they share the close path, so verify the cap doesn't regress macOS/Windows close behavior. macOS unsigned-binary action buttons can silently no-op (CLAUDE.md) — do not "fix" a missing macOS action callback that is actually this documented platform limitation.

**0.3** — `stdio.rs:33` relies on `kill_on_drop(true)` as the sole Unix cleanup; the `#[cfg(windows)]` arm (`stdio.rs:38-52`) already wraps the child in a `JobObject` so grandchildren die with the parent. Bring the Unix arm to parity with `process-wrap`'s `ProcessGroup` (already a dependency, per the file's own `TODO`) so killing the child reaps the whole group. Remove the `TODO` comment once done. **Verify on a real Unix host** (spawn an MCP upstream that itself forks a Node/Python worker, kill Conduit, confirm no orphan) — this cannot be observed from the Windows dev box.

**Acceptance:** burst of N rapid approvals spawns ≤ cap threads/tasks; channel cannot grow without bound; killing an stdio upstream leaves no orphaned grandchild process on Unix *and* Windows; existing notification behavior otherwise unchanged on all three OSes.

---

### Phase 1 — Pure DRY, low-risk, compiler-verified ✅ DONE

> Highest line-count reduction with zero behavior change. Each is mechanical and guarded by the compiler and existing tests. No dependencies between these except where noted.

| Slice | Files | Risk | Effort |
|---|---|---|---|
| **1.1 — Centralize `now_unix` / `now_unix_nanos`** | new `src/daemon/time_util.rs` + 6 call sites | low | S |
| **1.2 — `Db::send_cmd` helper; collapse ~50 oneshot wrappers** | `src/daemon/db/{mod,servers,ides,policy,approvals,audit,skills,settings,oauth_drafts,migrations}.rs` | low | M |
| **1.3 — DB readability: `collect::<Result<_>>` + `bind_in_clause` + enum round-trip tests** | `src/daemon/db/{servers,ides,policy,approvals,skills,audit,migrations,types}.rs` | low | M |
| **1.4 — Installer `xml_escape` + cross-platform `atomic_write` shared util** **[cfg]** | `src/installer/{mod,systemd_linux,launchd_macos,scheduled_task_windows}.rs` + `src/daemon/clients/mod.rs` | medium | M |

**1.1** — `now_unix` is duplicated identically in 5 files (`approval.rs:62`, `mcp_proxy.rs:322`, `clients/mod.rs:560`, `skills/mod.rs:535`, `upstream/manager.rs:421`) — and `manager.rs`'s copy has **already drifted** (inline `try_from`), which is the proof the duplication is dangerous. Promote one implementation to `src/daemon/time_util.rs` (`now_unix_secs() -> i64`, `now_unix_nanos() -> u128`). **Decide the clock-skew policy once here:** `tracing::warn!` on `duration_since` failure / `i64` overflow instead of silent `unwrap_or(0)` / `unwrap_or(i64::MAX)` (fixes the `db/mod.rs:68` finding too).

**1.2** — Add one private generic helper on `Db`:
```rust
async fn send_cmd<T>(
    &self,
    make: impl FnOnce(oneshot::Sender<Result<T, DbError>>) -> DbCmd,
) -> Result<T, DbError>
```
Each public method then collapses to a single `self.send_cmd(|reply| DbCmd::GetServer { name, reply }).await` line. **Do NOT** macro away the `DbCmd` enum or the dispatch `match` — those are honest, exhaustive (catch new variants at compile time), and correct for the actor pattern (see Do-Not-Touch).

**1.3** — Replace the ~10 `let mut out = Vec::new(); for r in rows { out.push(r?) } Ok(out)` blocks with `rows.collect::<Result<Vec<_>, _>>()?`. Add one `bind_in_clause(items) -> (placeholder_string, params_vec)` helper for the 3 hand-built IN/NOT-IN sites (removes off-by-one / param-order footguns; keep `format!` for the static SQL skeleton — parameters still bound via `?`). Add explicit round-trip tests for every `as_db_str`/`from_db_str` enum so a future variant cannot silently corrupt data. *Depends on 1.2 (same files; sequence to avoid churn).*

**1.4** — `xml_escape` is identical in `launchd_macos.rs:98` and `scheduled_task_windows.rs:116`; `atomic_write` is near-identical in `systemd_linux.rs:75` and `launchd_macos.rs:81` (differs only by a fallback constant). Centralize `xml_escape` in `src/installer/mod.rs` (trivial).

**The `atomic_write` part is a cross-platform correctness fix, not just DRY** — promote with care:
- Both installer copies use bare `fs::rename(&tmp, path)` (`systemd_linux.rs:83`, `launchd_macos.rs:89`). On POSIX `rename` atomically *replaces* an existing file; **on Windows it errors with `ERROR_ALREADY_EXISTS` when the destination exists.** The copies are safe today only because both are `#[cfg]`-gated to Unix.
- The Windows back-end (`scheduled_task_windows.rs:51`) doesn't even use atomic write — it does a plain `std::fs::write`, so a crash mid-write can leave a truncated `autostart.xml`.
- `src/daemon/clients/mod.rs` **already has a Windows-safe `atomic_rename_or_replace`** (per CLAUDE.md / the clients audit). Do **not** promote the Unix-only bare-rename version. Instead, reuse/extract the clients' rename-or-replace logic into one shared cross-platform `atomic_write(path, bytes, fallback_name)` and route **all three** installer back-ends (including the Windows `fs::write`) plus the clients writer through it. This collapses the two divergent atomic-write implementations (slice 1.4 and the `io.rs` split in slice 2.2) into a single correct primitive.
- Where this lives is a judgment call: a tiny `src/atomic_fs.rs` (crate-shared) is cleaner than `installer/mod.rs` if `clients/` is also a consumer. Keep `xml_escape` in `installer/mod.rs` (installer-only concern).

---

### Phase 2 — SOLID / extensibility (the structural win) ✅ DONE

> The single highest-leverage SOLID refactor in the codebase: turn "edit 7 match arms" into "add one module + one registry line." Self-contained behind stable public signatures, so callers/tests don't move.

| Slice | Files | Risk | Effort | Depends on |
|---|---|---|---|---|
| **2.1 — `ClientSpec` trait + registry replacing 7 `match ide {}` dispatchers** ✅ | `src/daemon/clients/{mod,claude_code,cursor,codex,opencode,openclaude,vscode_copilot}.rs` | medium | M | — |
| **2.2 — Split `clients/mod.rs` by concern + serde error context** ✅ | `src/daemon/clients/mod.rs`, new `merger.rs`, new `io.rs` | low | M | 2.1 |

**2.1** — `clients/mod.rs` has **7 exhaustive `match ide {}` dispatchers** (`json_schema_for`, `config_path_for`, `config_path_under`, `ide_name`, `icon_svg`, `skills_dir_for`, `skills_dir_under` at lines 36/78/92/113/124/151/167). Adding the Nth IDE forces editing all 7. Introduce:
```rust
trait ClientSpec {
    const IDE_NAME: &'static str;
    const ICON_SVG: &'static [u8];
    fn config_path(home: &Path) -> Option<PathBuf>;
    fn skills_dir(home: &Path) -> Option<PathBuf>;
    fn config_format() -> ConfigFormat;   // JsonSchema(..) | Toml — makes the JSON-vs-TOML contract explicit
}
```
plus a const registry `&[(IdeKind, &dyn ClientSpec)]` in `mod.rs`. Public function signatures (`config_path_for`, etc.) stay identical — they become registry lookups. **Adding an IDE = new module + one registry line, zero dispatcher edits.** Fold the `DbEnum` decode helper for `IdeStatus`/`Decision` in here if convenient. *Leave the Slint tab array and the skills `SKILL_TARGET_IDES` loop out of this slice* (separate concerns — see Phase 3 / Do-Not-Touch).

**Cross-platform constraint on the trait design (don't skip this):** today `config_path()` / `skills_dir()` are home-relative and OS-uniform for the clients that were spot-checked (e.g. `claude_code.rs` → `~/.claude.json` on every OS). But IDE config locations are *not* universally OS-uniform — VS Code / Cursor-class editors live under `%APPDATA%` on Windows, `~/Library/Application Support` on macOS, and `~/.config` on Linux. **Before locking the trait signature, audit every per-IDE module for an existing `#[cfg(target_os)]` branch in its path logic.** The trait must be able to express per-OS divergence — keep `fn config_path() -> Option<PathBuf>` (each impl does its own `dirs::config_dir()` / `home_dir()` + `cfg!` internally) rather than a `fn config_path(home: &Path)` that hard-codes "everything hangs off `$HOME`". The `*_under(home)` test seams stay, but for OS-divergent IDEs `_under` must take whatever base the real path uses, not assume `$HOME`. Because per-IDE path code is `cfg`-gated, this slice carries the **[cfg]** multi-target gate.

**2.2** — After 2.1 shrinks the dispatch, `clients/mod.rs` (559-566 lines, over the soft limit) cleanly splits into three concerns: dispatch/schema (`mod.rs`), JSON/TOML merge (`merger.rs`), atomic IO (`io.rs`). Also attach `path` + `stage` context to the serde parse failures (`mod.rs:305/357/455`) so `cascade.rs` logs name the failing file.

---

### Phase 3 — Glue & UI ergonomics ✅ DONE

> No behavior change, but every future callback and every color-semantics change gets cheaper. Done after structural daemon work so it can adopt any new shared helpers. Phase 3 slices touch rendered output / shared signatures → review carefully.

| Slice | Files | Risk | Effort |
|---|---|---|---|
| **3.1 — `CallbackContext` struct + UI-state envelope helpers + `read_ui_state`** ✅ | `src/glue/{callbacks,callbacks_servers,callbacks_ide,callbacks_misc,skills,wizard,util}.rs` | medium | M |
| **3.2 — Slint `DecisionBadge` + `DecisionChip` + `PolicySelector` reusable components** ✅ | `src/ui/components/primitives.slint` + `tool_policy/dashboard/log/settings` tabs | medium | M |

**3.1** — Two low-risk moves:
- **(a)** Bundle shared deps into `CallbackContext { window, db, rt, skills, approvals, bus, port }`; change every `install_*_callbacks` to take `&CallbackContext`. Kills the divergent signatures (`callbacks.rs:27`, `callbacks_ide.rs:26`, `callbacks_servers.rs:29`, `skills.rs:24`) and makes adding a dependency a one-line change.
- **(b)** Add free helpers `ui_set_busy(ui, msg)`, `ui_set_error(ui, msg)`, `ui_clear_status(ui)` to dedup the scattered `set_busy/set_status/set_is_error` triads, and generalize the `util.rs:90-97` oneshot UI-read into `read_ui_state`.

**Deliberately deferred:** the heavier event-bus / `RefreshScope` rework of the callback *bodies* (`callbacks_ide` orchestration). High risk for a concurrency-sensitive UI, low payoff right now.

**3.2** — `decision→color` (allow/ask/deny → accent/warning/danger) is duplicated across 4 tabs (`tool_policy.slint:43`, `dashboard.slint:422`, `log.slint:37`, `settings.slint:85`); `status→tint` across 4 more. Create two theme-aware components in `src/ui/components/`. The canonical enum→semantic-meaning stays in Rust; the component owns only semantic-color→pixel via theme tokens. **Verify each tab renders pixel-identical** before/after.

---

### Phase 4 — Upstream error ergonomics (most concurrency-sensitive, do last)

| Slice | Files | Risk | Effort |
|---|---|---|---|
| **4.1 — `From<UpstreamError>` impls replacing single-use `map_*` fns; tighten `call_tool`/`list_tools` lock scope** | `src/daemon/upstream/{probe,manager}.rs` | high | M |

Delete `map_oauth_err` / `map_connect_error` / `map_tools_error` (`probe.rs:352/393/403`) in favor of `From<…>` + `?`. **Keep the four typed error enums** — do **not** merge them into one stringly `InitFailed(String)` (CLAUDE.md forbids erasing typed errors). Separately, extract a `get_peer_for_call()` helper that checks-and-gets within **one** lock scope to narrow the crash-detection window in `call_tool`/`list_tools` (`manager.rs:207`/`244`). Last because it's the lowest-duplication payoff in the most sensitive area.

---

## Cross-platform correctness (cross-cutting — applies across all phases)

> Conduit ships one binary to win32 / macOS / Linux, and the refactor targets exactly the subsystems where the three OSes diverge. The host compiler only checks the *active* `cfg` arm, so this section enumerates the OS-specific traps a single-OS green build will not catch. Each item names the slice it modifies or a verification it adds.

| ID | Concern | Status | Lands in |
|---|---|---|---|
| **CP-1** | `atomic_write` correctness diverges by OS | verified bug-risk | slice 1.4 (+ folds slice 2.2 `io.rs`) |
| **CP-2** | Unix stdio grandchild leak; Windows already correct | verified violation | slice 0.3 |
| **CP-3** | Per-OS IDE config paths must fit the new trait | design constraint | slice 2.1 |
| **CP-4** | Notification paths split Linux vs macOS vs Windows | scoping clarification | slice 0.2 + quick wins |
| **CP-5** | Linux keyring fallback to encrypted file store | verify-only | new quick win below |
| **CP-6** | Path case-sensitivity (`SKILL.md`) differs by FS | already handled | no change — keep the helper |

**CP-1 — atomic file writes.** POSIX `fs::rename` replaces atomically; Windows `fs::rename` errors when the target exists. The Unix installer copies hide this behind `#[cfg]`; the Windows installer skips atomicity entirely (`std::fs::write`). Resolution detailed in slice 1.4: one shared rename-or-replace primitive (reuse the clients' existing `atomic_rename_or_replace`), routed through all three installer back-ends and the clients writer. **Test the replace-over-existing-file case on Windows specifically** — it's the exact case bare `rename` fails.

**CP-2 — child-process group cleanup.** Detailed in slice 0.3. The asymmetry (Windows `JobObject` ✓, Unix `kill_on_drop` ✗) means the leak is *structurally invisible* on a Windows dev box. Whoever implements 0.3 must verify on a real Unix host with a forking upstream.

**CP-3 — IDE config path divergence.** Detailed in slice 2.1: the `ClientSpec` trait must let each IDE compute its own OS-specific base (`dirs::config_dir()` vs `home_dir()` + `cfg!`), not assume `$HOME`. Audit every per-IDE module for `#[cfg(target_os)]` before freezing the signature.

**CP-4 — notification platform split.** `notify-rust`'s behavior is not uniform: Linux uses the blocking `wait_for_action` (the unbounded-spawn source in 0.2); macOS shows action buttons that **silently no-op for unsigned binaries** (documented in CLAUDE.md — not a bug to "fix"); Windows uses toast with its own action model. Quick-win observability edits (`notifications.rs:53`, `:153`) are cross-cutting and safe, but any change to *action dispatch* must be verified on the OS whose path it touches. Don't assume the Linux action flow generalizes.

**CP-5 — keyring fallback (verify-only).** CLAUDE.md requires catching `keyring::Error::NoStorageAccess` / `PlatformFailure` on Linux (no gnome-keyring/KWallet running) and degrading to a file-based encrypted store. The `oauth_store` poisoning quick win is orthogonal to this. **Action:** confirm the Linux keyring-unavailable fallback path actually exists and is exercised; if it's missing, that's a new finding outranking most of the cosmetic work — file it rather than folding it silently into a DRY slice.

**CP-6 — filesystem case-sensitivity.** The skills subsystem already matches `SKILL.md` case-insensitively (skills audit, slice candidates `skills/mod.rs:203`, `discover.rs:228`). This is *correct* cross-platform handling (Linux ext4 is case-sensitive; macOS APFS and Windows NTFS are case-insensitive by default). When slice consolidates the three case-checks into one helper, **preserve the case-insensitive behavior** — do not "simplify" it to an exact `== "SKILL.md"` match, which would break discovery on case-sensitive Linux filesystems.

---

## 3. Quick wins (safe, can fold into Phase 0/1 PRs)

- Replace the ~10 manual `Vec` loops with `collect::<Result<Vec<_>, _>>()?` (this is slice 1.3; can ship piecemeal).
- Centralize `now_unix`, delete the 5 copies (slice 1.1).
- Add `tracing::warn!` on silent-degrade paths: `now_unix` clock-skew/overflow (`db/mod.rs:68`), `oauth_store` RwLock poisoning (`oauth_store.rs:35,46,70,81`), the notification action catch-all `_ => None` (`notifications.rs:153`).
- Split the IDE-lookup match so `Ok(None)` logs `debug` and `Err` logs `warn` instead of both collapsing to "Unknown IDE" (`notifications.rs:53-56`) — same UI, real observability.
- Handle `JoinError` explicitly in `main.rs:121-127` (`match` on the `spawn_blocking` result, `warn!` on `Err`) instead of `.unwrap_or(0)`.
- Add a one-line `// reason:` comment on the `i64::try_from(...).unwrap_or(i64::MAX)` tray casts (`tray.rs:275,289,317`) explaining the overflow is unreachable; same for the `spawn_done`/timeout asymmetry in `probe.rs`.
- Extract `NOTIFICATION_ACTIONS` as a `const` table used by **both** the `notify_rust` registration (`notifications.rs:125-129`) and the dispatch match (`notifications.rs:144-153`) so the two can't drift.
- **(CP-5, verify-only)** Confirm the Linux keyring-unavailable fallback exists: a `keyring::Error::NoStorageAccess` / `PlatformFailure` path that degrades to the file-based encrypted store. If present, add a one-line `// reason:` / `tracing::warn!` so the degrade is observable; if **absent**, file it as a new high-priority finding (a missing fallback fails closed on headless Linux) — do not bury it in a DRY slice.

---

## 4. Do NOT touch (load-bearing — leave exactly as is)

These look refactorable but are deliberate. "Make it more SOLID" here would make the code worse.

1. **The `DbCmd` enum + the single dispatch `match`** (`db/mod.rs:133`). The `#[expect(too_many_lines)]` is an honest, documented tradeoff; the match is exhaustive (catches new variants at compile time) and is the correct shape for the actor pattern. `send_cmd` (slice 1.2) already removes the *actual* boilerplate — macro-generating the dispatch adds indirection for no extensibility gain.
2. **The DB-actor architecture** (single `std::thread` owns `Connection`, `mpsc`+`oneshot`, no `Arc<Mutex>`, no pool). Confirmed correct by multiple reports.
3. **Collapsing the four upstream error enums** (`UpstreamError`/`ProbeError`/`OauthError`/`InstallerError`) into one stringly type. CLAUDE.md explicitly forbids erasing typed errors. Only remove the redundant single-use `map_*` adaptors (slice 4.1).
4. **A `TransportFactory` / `TransportConnector` registry for upstream.** There are exactly two transports (Stdio, Http) and no third on the roadmap. A plugin registry is speculative "might be useful later" abstraction the CLAUDE.md ethos bans. An exhaustive match on a 2-variant enum is clearer. Revisit only when a real third transport lands.
5. **A Slint tab-registry / `TabMetadata` rework** (`main.slint:214`). High effort, the tab set is stable at 6, and threading a Rust-populated tab registry through Slint layout fights the framework. At most extract the labels array to a shared constant — do not build a registry.
6. **The tray polling loop** (`tray.rs:202-256`). The 100 ms poll is a deliberate accommodation of synchronous `MenuEvent` APIs with bounded cancel-token latency. "Fixing" it to async `select!` would be incorrect. Add a clarifying comment only.
7. **CancellationToken plumbing and `shutdown_timeout` sequence.** Correctly propagated; do not refactor.
8. **`oauth_store` fail-soft-on-poison behavior.** The degrade-to-keyring fallback is intentional and correct. Only add the missing `warn!` diagnostic; do not switch the locking strategy.
9. **The `installer/` module being test-only/unwired** (`lib.rs:15`). Intended for a future autostart-toggle. Leave it public; just note its status — do not delete or `#[cfg(test)]`-gate it.

---

## 5. Suggested PR sequence

```
PR1  Phase 0.1+0.2  bound channel + cap notification threads   correctness — merge first
PR2  Phase 0.3      Unix stdio ProcessGroup cleanup     [cfg]  contract fix — verify on Unix
PR3  Phase 1.1      now_unix util + skew policy + quick-win warns
PR4  Phase 1.2      Db::send_cmd collapse
PR5  Phase 1.3      DB collect/bind_in_clause + enum round-trip tests   (after PR4)
PR6  Phase 1.4      cross-platform atomic_write + xml_escape  [cfg]  (folds clients io.rs atomic write)
PR7  Phase 2.1      ClientSpec registry                  [cfg]  (audit per-OS paths first)
PR8  Phase 2.2      clients/mod.rs split + serde context  [cfg]  (after PR7; atomic write already shared by PR6)
PR9  Phase 3.1      CallbackContext + UI envelope helpers
PR10 Phase 3.2      DecisionBadge/StatusDot                     (visual review)
PR11 Phase 4.1      upstream From impls + lock-scope tighten    (careful review)
```

`[cfg]` PRs must clear the per-target gate (§1), not just the host build. PR1 and PR2 are independent correctness fixes; split so the high-risk Unix-only 0.3 (which needs a real Unix host to verify) doesn't block the trivial 0.1/0.2. PRs 3–6 are otherwise mutually independent (except PR5-after-PR4). PRs 7→8 sequence; PR6 lands the shared `atomic_write` so PR8's `io.rs` split just consumes it rather than re-implementing. PR10-after-PR9. Phase 4 last.

---

## Appendix A — Full per-subsystem findings

The complete file:line-backed findings from each of the 9 subsystem auditors are preserved below for reference. Severity/category/effort tags in brackets.

<!-- BEGIN per-subsystem findings -->
#### daemon/db
The daemon/db subsystem exhibits solid architectural adherence to the Conduit contract—the DB-actor pattern (single thread owning rusqlite::Connection, mpsc+oneshot for cross-boundary calls) is correctly enforced. Error handling via thiserror is proper, and no panics occur in non-test code. However, several DRY violations exist: decode_*_row functions duplicate enum-to-string error handling logic; the ~50 Db async wrapper methods repeat the same oneshot ceremony; and enum conversion patterns (as_db_str/from_db_str) lack type-safe machinery to catch unknown values. SQL construction using format! is safe (parameters go in separately via placeholders) but is scattered and not normalized. Minor antipatterns include the now_unix function's unwrap_or silently handling clock skew, manual loop-based Vec accumulation instead of collect(), and fragmented organization where each domain (servers, ides, policy, etc.) duplicates the encode/decode and wrapper scaffolding.

- [high/DRY/M] **DRY violation: repeated enum decode-and-error-conversion pattern**
  - loc: src/daemon/db/approvals.rs:15-23, src/daemon/db/audit.rs:14-34, src/daemon/db/ides.rs:77-85, src/daemon/db/policy.rs:16-32, src/daemon/db/servers.rs:183-218
  - problem: Every decode_*_row function manually repeats the pattern of extracting a string, calling from_db_str, and building a rusqlite::Error::FromSqlConversionFailure with the same boilerplate. Decision, DecisionSource, ApprovalStatus, AuditDecision, and IdeStatus all use this identical error shape. This is knowledge duplication that should be centralized.
  - fix: Create a helper trait (e.g., DbEnum) with a from_db_str_or_err method that takes the column index and produces the rusqlite::Error inline. Implement it once for each enum in types.rs, eliminating 5+ copies of the conversion ceremony.
- [high/DRY/M] **DRY violation: ~50 identical oneshot async wrapper methods in Db impl blocks**
  - loc: src/daemon/db/servers.rs:331-470 (10 methods), src/daemon/db/ides.rs:267-392 (10 methods), src/daemon/db/policy.rs:143-247 (7 methods), src/daemon/db/approvals.rs:119-184 (5 methods), src/daemon/db/audit.rs:144-174 (3 methods), src/daemon/db/skills.rs:202-311 (9 methods), src/daemon/db/settings.rs:37-62 (2 methods), src/daemon/db/oauth_drafts.rs:35-62 (3 methods), src/daemon/db/migrations.rs:301-331 (3 methods)
  - problem: Every public async method on Db follows the same rigid shape: create a oneshot channel, send a DbCmd variant over self.tx, await the reply receiver, map errors to ActorGone. This scaffolding is repeated ~50 times with only the variant constructor and field names changing. The pattern is mechanical and error-prone (easy to forget the .to_string() conversions).
  - fix: Create a single generic helper method on Db: `fn send_cmd<T: Send + 'static>(&self, cmd: impl FnOnce(oneshot::Sender<Result<T, DbError>>) -> DbCmd) -> impl Future<Output = Result<T, DbError>>`. This eliminates boilerplate and ensures consistency. Alternatively, use a macro (e.g., db_cmd! { GetServer, name => DbCmd::GetServer { name, reply } }) to generate the wrapper one-liners.
- [medium/SOLID/M] **Enum validation gap: unknown DB values silently pass through (no compile-time or runtime exhaustiveness check)**
  - loc: src/daemon/db/types.rs:28-35 (Decision::from_db_str), src/daemon/db/types.rs:58-67 (DecisionSource::from_db_str), src/daemon/db/types.rs:100-110 (ApprovalStatus::from_db_str), src/daemon/db/types.rs:147-159 (AuditDecision::from_db_str), src/daemon/db/types.rs:255-263 (IdeStatus::from_db_str), src/daemon/db/policy.rs:167 (resolve_policy: fallback to Ask)
  - problem: All enum::from_db_str methods return Option and the decode functions convert None to a rusqlite error. However, the fallback in resolve_policy (line 167) silently defaults to Decision::Ask if settings_kv is missing or corrupted. This couples business logic (policy defaults) to database access in a way that's not extensible: adding a new DecisionSource means updating from_db_str in types.rs AND every decode_*_row function that uses it.
  - fix: Implement a Decode trait (or use strum's #[derive(FromStr)]) to provide exhaustiveness checking. For enums exposed to DB, ensure both the string mapping and fallback behavior are explicitly documented and tested. In resolve_policy, make the fallback explicit and potentially configurable rather than hard-coded.
- [low/readability/S] **Code antipattern: manual Vec collection instead of .collect()**
  - loc: src/daemon/db/servers.rs:276-284 (list_servers), src/daemon/db/servers.rs:160-180 (list_server_tools), src/daemon/db/ides.rs:109-117 (list_ides), src/daemon/db/ides.rs:120-130 (list_active_ides), src/daemon/db/policy.rs:105-118 (list_policies_for_server), src/daemon/db/policy.rs:121-131 (list_all_policies), src/daemon/db/approvals.rs:79-91 (list_pending_approvals), src/daemon/db/skills.rs:186-199 (list_skills), src/daemon/db/audit.rs:125-131 (query_query_audit_log), src/daemon/db/migrations.rs:290-298 (list_tables)
  - problem: Patterns like `let mut out = Vec::new(); for r in rows { out.push(r?); } Ok(out)` are repeated ~10 times. This is mechanical and less idiomatic than `rows.collect::<Result<Vec<_>, _>>()`. The loop form also obscures intent—it's not immediately clear that errors are propagated.
  - fix: Use `.collect::<Result<Vec<_>, _>>()?` consistently. This is more concise, idiomatic, and makes error handling explicit to readers.
- [medium/error-handling/S] **Error handling antipattern: unwrap_or in now_unix silently ignores clock skew**
  - loc: src/daemon/db/mod.rs:68-75 (now_unix)
  - problem: The now_unix function calls .unwrap_or(0) on duration_since(UNIX_EPOCH), which would only fail if the system clock is before 1970—an extremely unlikely but valid error condition. Silently defaulting to 0 (year 1970) creates audit entries and timestamps with nonsensical values, making debugging harder. Additionally, i64::try_from(secs).unwrap_or(i64::MAX) silently truncates times beyond 2262, another edge case that deserves logging.
  - fix: Log a warn! if duration_since fails, and log a warn! if the conversion overflows. Use a more principled default (e.g., current time at startup, or an error). Alternatively, remove the unwrap_or and ensure the function signature reflects that it can fail, or add a comment explaining why these edge cases are acceptable.
- [medium/DRY/M] **Lack of normalization for SQL parameter construction (IN clauses)**
  - loc: src/daemon/db/servers.rs:53-66 (cancelled_ids IN clause), src/daemon/db/servers.rs:95-118 (tool_names NOT IN clause, duplicated for two queries), src/daemon/db/ides.rs:205-223 (kinds_present NOT IN clause)
  - problem: Building SQL with IN/NOT IN clauses requires manually iterating over a collection to build placeholders and a parallel params_vec. This pattern is repeated three times with slight variations. The logic is error-prone (off-by-one in placeholder counts, parameter order mismatches) and not reusable. Additionally, the two delete statements on lines 101-104 and 107-117 use similar patterns but aren't unified.
  - fix: Create a helper function like `fn bind_in_clause(items: &[impl ToSql]) -> (String, Vec<Box<dyn ToSql + '_>>)` that returns both the placeholder string and the parameter vector, eliminating manual loop code. Alternatively, use a library like rusqlite_derive or a small DSL for building dynamic SQL safely.
- [medium/error-handling/S] **Potential SQL injection via table/column names in migrations.rs**
  - loc: src/daemon/db/migrations.rs:271 (ALTER TABLE {table} ADD COLUMN {col}), src/daemon/db/migrations.rs:279 (ALTER TABLE {table} DROP COLUMN {col}), src/daemon/db/migrations.rs:258 (PRAGMA table_info({table}))
  - problem: The ensure_column, drop_column_if_exists, and query_list_columns functions use format! to insert table and column names directly into SQL. While these are called only internally with known table/column names, this pattern is fragile: if these functions are ever called with user input or mistyped arguments, SQL injection is possible. Additionally, SQLite's PRAGMA table_info does not support parameterized identifiers.
  - fix: Add a validation function that ensures table and column names match a whitelist (e.g., [a-zA-Z_][a-zA-Z0-9_]*) and fails loudly if they don't. Alternatively, document the invariant clearly and add a test that covers all known table/column names. For PRAGMA, using format! is unavoidable; at least validate the input rigorously.
- [low/DRY/S] **Contract adherence: DB-actor thread correctly isolated, no async/Connection leaks**
  - loc: src/daemon/db/mod.rs:112-130 (actor_loop), src/daemon/db/mod.rs:94-100 (spawn_with_inner)
  - problem: No architectural violation found. The DB-actor correctly owns the Connection in a blocking std::thread, and the oneshot pattern prevents .await from crossing thread boundaries. However, this merit is slightly offset by the boilerplate duplication in wrappers.
  - fix: No fix required; this is correctly implemented.
- [low/error-handling/S] **Missing integration test for enum DB round-trip conversions**
  - loc: src/daemon/db/types.rs:333-354 (existing tests for fresh_ide_key only)
  - problem: There are no tests verifying that all enum values survive a round-trip: enum → as_db_str → from_db_str → enum. This is a critical invariant for data integrity; if a new variant is added but the as_db_str/from_db_str mappings are incomplete, the database can silently corrupt data during reads. The types.rs module has tests for fresh_ide_key but not for enum conversions.
  - fix: Add property-based tests (e.g., using proptest) or explicit enumeration tests for each enum to verify all variants round-trip correctly. Example: for Decision, test that all three variants encode to unique strings and decode back to themselves.
- [medium/SOLID/L] **Architectural gap: no central registry for DB operations or extensibility mechanism**
  - loc: src/daemon/db/commands.rs:24-242 (DbCmd enum with 52 variants), src/daemon/db/mod.rs:134-409 (dispatch function with 52 match arms)
  - problem: Adding a new DB operation requires editing three places: (1) DbCmd enum, (2) dispatch match arm, (3) impl Db wrapper method. The dispatch function is already 270+ lines with a catch-all #[expect(clippy::too_many_lines)] suppression. This violates Open/Closed principle: extending the system requires modifying core code. Future additions (e.g., new IDE type, new skill source, new MCP upstream kind) will force cascading edits.
  - fix: Consider a command-registry macro or trait-based dispatch if the subsystem grows beyond 60-80 operations. For now, if growth is expected, document that DbCmd must be split into logical groups (IdeOps, ServerOps, PolicyOps) with separate dispatch functions, or use a builder/derive-macro to reduce boilerplate. Keep the current approach if the 52 variants are stable and unlikely to grow.

#### daemon/upstream
The upstream subsystem is architecturally sound with strong error handling and thoughtful design choices. The manager correctly enforces thread-affinity, uses proper locking discipline, and implements a clear reaper pattern. The probe flow is well-structured with good async/await patterns. However, there are four categories of improvements: (1) a potential race window in entry validation before peer access, (2) significant DRY violations around transport matching and error mapping that should be table-driven, (3) extensibility constraints when adding new transports or auth types (Open/Closed), and (4) overlapping error semantics across four separate error types serving similar purposes.

- [high/contract-violation/M] **Race condition in call_tool and list_tools: entry may disappear between crash-check and peer use**
  - loc: src/daemon/upstream/manager.rs:207-220, src/daemon/upstream/manager.rs:244-256
  - problem: Both `call_tool` and `list_tools` follow the same pattern: lock state, check if entry exists and is not closed, get peer, drop lock, then use peer. The entry can be removed by the reaper between the crash-check and the peer RPC call. While this returns Crashed (correct outcome), the window is wider than necessary. The ensure_running check at line 202/242 doesn't guarantee the entry will still exist when we lock again on line 207/244, because ensure_running only checks if already_running once outside the lock. A second concurrent ensure_running could race-win the lock and insert a duplicate, or the reaper could kill it mid-flight. The code handles this gracefully (crash detection on lines 246-247 and 246-251) but the inner lock scope is unnecessarily conservative.
  - fix: Extract a helper `fn get_peer_for_call()` that atomically checks-and-gets within a single lock scope, returning an error if the entry is missing or closed. Alternatively, move the is_closed() check into the entry.client.peer() result check, so crash detection is tighter. The dual crash-check pattern (lines 213-216 and 249-251) can be unified into a single macro or helper.
- [medium/DRY/M] **Duplicated transport matching and connection logic in manager and probe**
  - loc: src/daemon/upstream/manager.rs:297-310, src/daemon/upstream/probe.rs:126-177
  - problem: Both `UpstreamManager::ensure_running` and `probe_with_progress` match on Transport/DraftTransport and call the appropriate connector. They differ slightly (probe handles HttpAuth + headers, manager checks OAuth), but the Stdio branch is identical except for async timeout wrapping. If a third transport type (e.g., gRPC, WebSocket) is added, both sites must be updated. The HTTP OAuth retry logic in probe (lines 144-174) duplicates the intent of manager's http::connect_oauth fallback (lines 304-309). DRY principle: these patterns should centralize.
  - fix: Create a shared ConnectorConfig enum that unifies Transport and DraftTransport shapes. Implement a trait-based `TransportConnector` with methods like `connect_stdio()`, `connect_http()` that both manager and probe call, passing a shared config. This also opens a path to plug in new transports without touching core logic (Open/Closed principle).
- [medium/SOLID/L] **Error-type fragmentation: UpstreamError, ProbeError, OauthError, InstallerError serve overlapping roles**
  - loc: src/daemon/upstream/manager.rs:59-87, src/daemon/upstream/probe.rs:41-57, src/daemon/upstream/oauth.rs:16-26, src/daemon/upstream/installer.rs:16-30
  - problem: Four error types exist, each with similar variants (IO, Init/Connect failed, Timeout, Serialization, Cancelled in some cases). ProbeError, OauthError, and InstallerError are result types for leaf operations (probe a server, run OAuth flow, install a package), while UpstreamError is the manager's public surface. The caller (mcp_proxy) must handle `UpstreamError` only, so ProbeError/OauthError/InstallerError are private leaf types. However, within probe.rs, there are four separate error-mapping functions (map_oauth_err, map_connect_error, map_tools_error) that convert from upstream errors into ProbeError. This creates friction when the error landscape changes. Adding a new error case requires updating multiple mapping sites.
  - fix: Define a single `UpstreamError` variant for `InitFailed(String)` that captures all non-Rmcp failures. Keep Rmcp as a pass-through. Remove ProbeError, OauthError, InstallerError and use UpstreamError + `?` throughout. Callers can pattern-match on UpstreamError variants or use `e.to_string()`. If future work needs rich error metadata (e.g., for UI), introduce an ErrorContext trait once it's clear what the contract should be, rather than pre-fragmenting.
- [medium/extensibility/L] **Hardcoded transport matching prevents extensibility: adding gRPC/WebSocket/etc requires core edits**
  - loc: src/daemon/upstream/manager.rs:35-49, src/daemon/upstream/manager.rs:297-310, src/daemon/upstream/probe.rs:68-79, src/daemon/upstream/probe.rs:126-177
  - problem: Transport is an enum with Stdio and Http variants hardcoded in the manager. When a new transport type (e.g., gRPC, local WebSocket) is desired, the Transport enum must be extended and manager::ensure_running must add a new branch. This violates the Open/Closed principle: Transport should not require modification of the manager to support new kinds. DraftTransport has the same constraint in probe. The same issue appears with HttpAuth variants (None, Bearer) — adding OAuth2 client credentials would require editing probe's bearer_for and auth-logic branches.
  - fix: Introduce a `TransportFactory` trait with a `connect(&config) -> Result<RunningService, Error>` method. Callers register transport factories at startup (e.g., `registry.register("stdio", StdioFactory); registry.register("http", HttpFactory)`). manager::ensure_running calls `registry.connect(&config.transport_name)`. This allows plugins or test doubles without core edits. For HttpAuth, move Bearer/None into a KV map with pluggable auth handlers.
- [low/error-handling/S] **Cache-invalidation bug in oauth_store: RwLock poisoning leads to silent degrades**
  - loc: src/daemon/upstream/oauth_store.rs:33-39, src/daemon/upstream/oauth_store.rs:46-48, src/daemon/upstream/oauth_store.rs:70-71
  - problem: The load() method at lines 34-39 silently ignores a poisoned read lock and falls through to a keyring read. This is intentional (comment line 34: 'A poisoned cache lock degrades...'), but it means a panicked writer (which Rust's RwLock never triggers, but a hypothetical async panic in a lock scope would) causes all future loads to bypass the cache forever without any diagnostic. Similarly, save() and clear() ignore write-lock poisoning (lines 70-71, line 81). If a future code path panics while holding the cache lock, callers will suffer increased keyring I/O without any warning. The pattern is 'fail-soft' but 'silent-fail': there should be a metric or trace when poisoning occurs.
  - fix: Add a tracing::warn! when lock poisoning is detected (lines 35, 46, 70, 81). Alternatively, store a poisoned flag and track it, so a diagnostic can be logged once. If poisoning is truly unrecoverable, consider using a Tokio Mutex instead of RwLock to eliminate the possibility entirely.
- [low/DRY/S] **Duplicated error-mapping functions in probe.rs**
  - loc: src/daemon/upstream/probe.rs:352-361, src/daemon/upstream/probe.rs:393-400, src/daemon/upstream/probe.rs:403-405
  - problem: Three separate functions (map_oauth_err, map_connect_error, map_tools_error) all convert from upstream/rmcp errors into ProbeError using almost identical patterns (match + to_string()). Each is called once. This violates DRY and makes the error-type fragmentation worse: if new error cases appear, all three must be updated.
  - fix: Consolidate into a single error-adaptor or use a trait impl that converts from UpstreamError → ProbeError automatically. If ProbeError variants align with UpstreamError, implement `From<UpstreamError>` and use `?` instead of explicit calls.
- [low/readability/S] **Inconsistent spawn_done timeout wrapping in probe**
  - loc: src/daemon/upstream/probe.rs:133-137, src/daemon/upstream/probe.rs:145, src/daemon/upstream/probe.rs:388-391
  - problem: Stdio connections use spawn_with_phase_timeout (line 133), HTTP ok-case uses spawn_done (line 145), and spawn_done yields_now() without any time tracking. This asymmetry is confusing: Stdio gets a 30-second timeout, HTTP gets none. The spawn_done function (lines 388-391) is a no-op wrapper for symmetry, but it's not obvious why Stdio and HTTP have different timeouts or what spawn_done actually ensures. A reader cannot quickly determine if this is a bug or intentional.
  - fix: Rename spawn_done to something that clarifies its purpose (e.g., `ensure_spawned_ok()` or just inline the yield_now) and add a comment explaining the timeout asymmetry. If HTTP should have a timeout too, apply it. If spawn_done is genuinely just for clarity, document that.

#### daemon/clients (IDE config writers)
The daemon/clients subsystem properly isolates per-IDE config metadata into dedicated modules and uses atomic file I/O correctly. However, it violates SOLID principles through repeated match-on-IdeKind dispatch tables that force edits in multiple places when adding an IDE. The JSON/TOML merge logic is sound but co-located with dispatch infrastructure in a 566-line file that exceeds cohesion targets. Error handling discards parse-context by leaning on generic serde errors.

- [high/DRY/M] **Seven exhaustive match dispatchers on IdeKind create DRY violation**
  - loc: src/daemon/clients/mod.rs:35-59, src/daemon/clients/mod.rs:77-86, src/daemon/clients/mod.rs:91-100, src/daemon/clients/mod.rs:112-121, src/daemon/clients/mod.rs:123-132, src/daemon/clients/mod.rs:150-159, src/daemon/clients/mod.rs:166-175
  - problem: Each of seven functions dispatches on IdeKind by matching all 6 variants (json_schema_for, config_path_for, config_path_under, ide_kind_from_db_name, ide_name, icon_svg, skills_dir_for, skills_dir_under). Adding a 7th IDE requires updating all 7 match blocks. This is fragile: a developer adding Glide (hypothetical IDE #7) must edit 7 places; missing one silently fails due to the note at line 31-34 (the #[expect] only applies to json_schema_for).
  - fix: Create a trait-based registry: `trait ClientSpec { const IDE_NAME: &str; const ICON_SVG: &[u8]; fn config_path() -> Option<PathBuf>; fn skills_dir() -> Option<PathBuf>; }` and implement it for each IDE module. Store implementations in a static array (e.g. `CLIENT_SPECS: &[(&IdeKind, &dyn ClientSpec)]`) or a const match-once lookup in a single dispatch function. This way, adding an IDE modifies only the per-IDE module and the registry, never the dispatcher functions.
- [high/SOLID/M] **Open/Closed violation: new IDEs force edits to dispatcher core**
  - loc: src/daemon/clients/mod.rs:35-175
  - problem: Per-IDE implementations (claude_code.rs, cursor.rs, etc.) each define IDE_NAME, ICON_SVG, and path functions. Retrieving these requires an exhaustive dispatcher in mod.rs. The design is closed to extension (e.g., adding Glide IDE) without modification to the dispatcher functions themselves, violating OCP. The spec at line 34 expects new IDEs to only add a new file plus wire in dispatchers, but wiring means editing 7 match blocks in mod.rs.
  - fix: Lift the per-IDE constants and functions into a registry that maps IdeKind to ClientSpec trait object. Define the registry once at module level (e.g., `const REGISTRY: &[(IdeKind, &dyn ClientSpec)] = &[(IdeKind::ClaudeCode, &claude_code::SPEC), ...]`). Query the registry instead of match-dispatching in public functions. This makes adding an IDE a matter of implementing ClientSpec in the new module plus adding one line to the registry, with zero edits to core dispatcher logic.
- [medium/cohesion-size/M] **Cohesion: mod.rs mixes config dispatch, atomic I/O, and schema logic (566 lines)**
  - loc: src/daemon/clients/mod.rs:1-566
  - problem: The file contains: dispatch tables (lines 35-175), JSON/TOML merge+write logic (lines 182-428), atomic file I/O primitives (lines 349-551). These are three distinct concerns. The project's soft target is <500 lines; this file at 566 is above that. Maintainers reason about dispatch, then shift context to JSON state merging, then to platform-specific atomic rename. No cohesive theme ties them together - they are IDE config work but mechanically separate.
  - fix: Split into two files: (1) mod.rs: dispatchers, public API (config_path_for, write, purge, etc.), and URL matching logic (~200 lines). (2) merger.rs: write_uniform_mcp_servers, write_toml_mcp_servers, purge_uniform_mcp_servers, atomic_write_json, atomic_write_toml, and atomic_rename_or_replace (~300 lines). This follows the project's concern-based split pattern.
- [low/DRY/S] **Redundant knowledge: IDE name stored in both DB strings and const IDE_NAME**
  - loc: src/daemon/clients/mod.rs:146-159, src/daemon/clients/claude_code.rs:9, src/daemon/clients/cursor.rs:9, src/daemon/clients/codex.rs:9, src/daemon/clients/opencode.rs:9, src/daemon/clients/openclaude.rs:9, src/daemon/clients/vscode_copilot.rs:9
  - problem: Each per-IDE module defines const IDE_NAME. The ide_name() function duplicates this knowledge as a match statement. The ide_kind_from_db_name() function also duplicates it. A change to one IDE's name requires edits in three places. The DB stores these strings directly (ManagedIdeConfig.ide_name), creating fragile string coupling.
  - fix: Have each IDE module export its IDE_NAME, and populate a single registry that maps IdeKind bidirectionally to name. Replace ide_name() and ide_kind_from_db_name() with registry queries. This consolidates knowledge to one place per IDE.
- [medium/error-handling/S] **Error context lost in generic serde errors**
  - loc: src/daemon/clients/mod.rs:305-307, src/daemon/clients/mod.rs:357-359, src/daemon/clients/mod.rs:455-457
  - problem: When JSON/TOML parsing fails, the code returns generic serde::de::Error::custom messages. This error message does not indicate which file failed, at what stage, or retain serde context. Callers in cascade.rs (line 91) must then log only the generic message, making diagnosis harder.
  - fix: Create custom error variants in ClientError for parse failures with file path and stage context, or wrap serde errors with anyhow::Context to attach operation context. Example: `serde_json::from_slice(&bytes).context(format!("parse IDE config at {}", path.display()))?`.
- [low/error-handling/S] **Inconsistent error handling between json_schema_for and writer dispatch**
  - loc: src/daemon/clients/mod.rs:35-59, src/daemon/clients/mod.rs:182-193
  - problem: The write() function calls json_schema_for(ide) and handles None by calling write_toml_mcp_servers. However, there is no guarantee: a hypothetical future IDE could have json_schema_for() return None but not have a write_toml_mcp_servers implementation. Only Codex is TOML; this works today but is fragile.
  - fix: Make the contract explicit: store the schema selector (JSON vs TOML) as part of the per-IDE ClientSpec. Example: enum ConfigFormat { JsonSchema(JsonSchema), Toml } and only allow writes through a single write_mcp_servers(ide: IdeKind, ...) dispatcher that dispatches on the format. This prevents half-implementing a new TOML IDE.

#### daemon/skills
The skills subsystem (discover, sync, mod) is well-architected and maintains the Conduit contract: async operations respect the boundary via spawn_blocking, DB calls go through the actor pattern, sync operations are atomic. Core logic is solid. However, there are three concrete findings: a DRY violation where skill metadata extraction logic duplication creates maintenance risk, path validation is invoked multiple times with identical parsing, and extensibility could be improved by registering IDE+schema pairs table-driven instead of hardcoding dispatchers. No architectural violations, error handling is solid (thiserror throughout, no unwrap in non-test code), and files are appropriately sized.

- [medium/DRY/S] **SKILL.md case-insensitive path checks duplicated across install and discovery**
  - loc: src/daemon/skills/mod.rs:203-207, src/daemon/skills/discover.rs:228-232, src/daemon/skills/discover.rs:277-280
  - problem: The same logic to detect SKILL.md files case-insensitively appears in three places: (1) install() checking for the file in the tree before download, (2) discover_one() stripping suffix in a loop, and (3) path_is_skill_md() checking lowercase. This spreads the case-insensitive list ["SKILL.md", "Skill.md", "skill.md"] and the matching algorithm. If a new variant is needed or the pattern changes, three sites must be updated. The code also hardcodes three variants instead of being data-driven or using a helper that encapsulates the list once.
  - fix: Extract a public `fn is_skill_md_path(path: &str) -> bool` in discover.rs that checks case-insensitively (unifying path_is_skill_md logic). Provide a companion `fn skill_md_basename() -> &'static [&'static str]` for the three variants. Update install() in mod.rs lines 203-207 to use the helper, and refactor discover_one() lines 228-232 to call it as well. This eliminates duplication and makes the list a single source of truth.
- [medium/DRY/S] **Metadata extraction from SKILL.md repeated in install and register_local_skills**
  - loc: src/daemon/skills/mod.rs:282-285, src/daemon/skills/mod.rs:388-390
  - problem: Both the install() flow (lines 282-285) and register_local_skills() (lines 388-390) read SKILL.md bytes, call `discover::read_skill_metadata()` with a fallback name, and construct a partial Skill struct. The pattern is identical: locate file → read bytes → parse metadata → use fallback. This knowledge duplication means future changes to metadata extraction (e.g., adding a new field or changing the fallback strategy) must be remembered in both places.
  - fix: Extract a private async helper `async fn load_skill_metadata_from_dir(dir: &Path, fallback: &str) -> Result<SkillMetadata, SkillError>` in mod.rs that encapsulates the locate + read + parse logic. Call it from both install() and register_local_skills(). Reduces call sites from 2 to 1 per operation.
- [low/DRY/S] **Path validation logic spread: sanitize_relative called in parallel with case checks**
  - loc: src/daemon/skills/mod.rs:191-192, src/daemon/skills/mod.rs:233-234
  - problem: In the install() flow, the source_directory is sanitized once (line 191), then later during file fetch loop, each relative path is sanitized again (lines 233-234). Both convert and validate the path; the second validation is redundant because the first already proved the source path is safe. Additionally, the pattern 'get raw path → validate → return tuple with (safe, error)' could be more uniform across the codebase.
  - fix: After initial sanitize_relative(&candidate.directory) at line 191-192, reuse source_dir throughout the install method without re-validating. The file loop can safely assume source_dir is valid because it was already checked. Remove the redundant sanitization at line 233-234 since it's applied to relative paths derived from a pre-validated base.
- [medium/extensibility/M] **IDE target iteration hardcoded in apply_targets_blocking and uninstall, not data-driven**
  - loc: src/daemon/skills/mod.rs:334-337, src/daemon/skills/mod.rs:449-458
  - problem: Both uninstall() and apply_targets_blocking() iterate SKILL_TARGET_IDES and call sync::sync_to_target() and sync::remove_from_target(). If a new IDE is added, these two dispatch points must be found and kept in sync manually. The pattern is coupled to the constant, making extensibility fragile. The sync module itself (discover, sync) has no registry; callers must hardcode loops.
  - fix: Create a data-driven dispatch table in sync.rs: a small registry like `pub fn apply_ide_targets(source: &Path, install_name: &str, targets: &[String]) -> Result<(), SkillError>` that centralizes the SKILL_TARGET_IDES loop and calls sync_to_target/remove_from_target. Callers in mod.rs invoke this one function. If a new IDE is added, the loop lives in one place. Alternatively, if adding a new IDE is rare, document the two sites (lines 334–337, 449–458) as a TODO checklist.
- [low/readability/S] **Metadata struct SkillMetadata only used for frontmatter parsing; not named to reflect that role**
  - loc: src/daemon/skills/discover.rs:283-287, src/daemon/skills/discover.rs:288-294
  - problem: The struct is named `SkillMetadata` but only extracts YAML frontmatter (name, description). The name suggests it could hold all skill metadata (tags, version, dependencies, etc.). The actual semantics are 'frontmatter fields extracted from SKILL.md'. This can mislead readers into thinking it's the superset of skill info, when it's a subset used during discovery. The naming is not wrong, but slightly imprecise.
  - fix: Consider renaming to `SkillFrontmatter` to clarify scope, or add a doc comment on the struct: `/// Fields extracted from SKILL.md frontmatter, used during discovery and install.` The latter is less invasive and clarifies intent at the type's definition site.
- [low/readability/S] **Concurrency limits scattered as module-level constants across mod.rs and discover.rs**
  - loc: src/daemon/skills/mod.rs:23-27, src/daemon/skills/discover.rs:16-17
  - problem: Four concurrency-tuning constants live in different files: DISCOVER_CACHE_TTL (mod), FETCH_TIMEOUT (mod), FILE_FETCH_CONCURRENCY (mod), MAX_SKILLS_PER_REPO and DISCOVER_CONCURRENCY (discover). This makes it hard to understand the full tuning surface in one place. A future operator wanting to adjust limits must search multiple files.
  - fix: Collect all concurrency/timing constants into a single `pub mod config` submodule or a const block at the top of mod.rs with doc comments explaining each. E.g., `/// Max repos resolved in parallel during discovery.` This is purely organizational; no logic change needed.

#### daemon core (http_server, mcp_proxy, approval, cascade, ipc, events, config, IDE clients)
The daemon subsystem maintains solid architectural boundaries (thread-affinity rules, DB-actor isolation, cancel-safe async) and proper error handling (typed thiserror enums, no anyhow in core, no Box<dyn Error>). However, it exhibits a critical DRY violation with `now_unix()` duplicated identically across 5 files (approval, mcp_proxy, clients, skills, upstream/manager), multiple opportunities for extensibility via traits are missed in the IDE clients dispatcher (currently 6+ manual match arms duplicated across config_path_for, config_path_under, skills_dir_for, skills_dir_under, ide_name, icon_svg), and the mcp_proxy handler does defensive unwrapping of JSON keys with placeholder defaults rather than rejecting malformed requests.

- [high/DRY/S] **DRY violation: `now_unix()` duplicated 5 times**
  - loc: src/daemon/approval.rs:62-68, src/daemon/mcp_proxy.rs:322-329, src/daemon/clients/mod.rs:560-566, src/daemon/skills/mod.rs:535-541, src/daemon/upstream/manager.rs:421-426
  - problem: The same knowledge (convert SystemTime to Unix seconds) is implemented identically in 5 places. This is a pure DRY violation and a maintenance risk — a future bug in the conversion logic must be fixed in all 5 locations. The slightly different error handling in manager.rs (which diverges from the others by calling try_from inline) is a warning sign that the functions are not being kept in sync.
  - fix: Extract a single `pub fn now_unix_secs() -> i64` into a new module like `src/daemon/time_util.rs` (or add to `errors.rs` as a utility) and re-export from `mod.rs`. All five files import and call this single implementation.
- [medium/extensibility/M] **Extensibility: IDE dispatch tables use manual match arms instead of trait registry**
  - loc: src/daemon/clients/mod.rs:77-86 (config_path_for), src/daemon/clients/mod.rs:91-100 (config_path_under), src/daemon/clients/mod.rs:112-121 (skills_dir_for), src/daemon/clients/mod.rs:123-132 (skills_dir_under), src/daemon/clients/mod.rs:146-159 (ide_name), src/daemon/clients/mod.rs:166-175 (icon_svg)
  - problem: Adding the Nth IDE forces editing 6+ dispatcher functions in clients/mod.rs. Each IDE module (claude_code, cursor, etc.) exports functions matching a fixed signature (config_path(), config_path_under(), skills_dir(), skills_dir_under()), but there is no trait abstraction. A trait-based registry would decouple IDE module addition from mod.rs, allowing new IDEs to be wired purely through a table lookup. The example.rs template hints at the intended pattern but provides no actual registry mechanism.
  - fix: Define a trait: `trait IdeClient { const IDE_NAME: &'static str; fn config_path_for(home: &Path) -> Option<PathBuf>; fn skills_dir_for(home: &Path) -> Option<PathBuf>; ... }`. Implement it in each IDE module. Create a const array: `const IDE_REGISTRY: &[(&IdeKind, &dyn IdeClient)] = &[(IdeKind::ClaudeCode, &claude_code::CLIENT), ...]`. Replace all 6 dispatcher functions with single-lookup implementations that search the registry by IdeKind.
- [medium/error-handling/M] **Error handling: mcp_proxy unwraps JSON keys with silent defaults instead of rejecting malformed requests**
  - loc: src/daemon/mcp_proxy.rs:78-83, src/daemon/http_server.rs:84-90
  - problem: When parsing the JSON-RPC envelope, the code uses `.unwrap_or("")` and `.unwrap_or(Value::Null)` to handle missing/malformed `method` and `params`. This silently accepts invalid requests (e.g., a request with no `method` key results in method="", which then fails the `.starts_with("tools/")` check or matches nothing). This violates the principle of early rejection — a malformed JSON-RPC request should fail at the parsing stage with a specific error, not silently accept and later produce a confusing outcome. The spec expects JSON-RPC error -32600 (invalid request) for malformed envelopes.
  - fix: Extract a struct-level parser for the envelope that returns `Result<(id, method, params), RpcError>`, explicitly validating that method is a non-empty string and params is an object (or null). Reject missing fields at parse time with -32600, not downstream.
- [low/cohesion-size/M] **Cohesion: clients/mod.rs is large and mixes schema definition, file I/O, and IDE dispatch**
  - loc: src/daemon/clients/mod.rs (559 lines)
  - problem: The clients module combines: (1) JsonSchema const definitions (lines 20–59), (2) IDE dispatch tables (lines 77–175), (3) file I/O and atomic write logic (lines 233–560). This makes it hard to reason about — adding an IDE schema is mixed with file-writing concerns. The file is also at the edge of the soft 500-line guideline and would benefit from splitting.
  - fix: Split into three modules: `clients/schema.rs` (JsonSchema definitions and lookups), `clients/dispatch.rs` (all the match_ide dispatcher functions using a trait registry), `clients/io.rs` (write_uniform_mcp_servers, atomic_write_json, read_existing_object, now_unix, now_nanos). Clients/mod.rs becomes a facade that re-exports the public API.
- [medium/DRY/M] **Readability: http_server::server_to_config hides a 'convert DB type to upstream type' which duplicates knowledge**
  - loc: src/daemon/http_server.rs:205-261
  - problem: The `server_to_config` function rebuilds the upstream::manager::ServerConfig from a DB Server row, manually handling env resolution (Plain vs Secret), transport type matching (TRANSPORT_STDIO, TRANSPORT_HTTP), and bearer token extraction. This is essentially a schema translation that could be fragile if the DB schema evolves — there's duplicated knowledge about how Server fields map to ServerConfig fields. The function is called from both mcp_proxy and cascade, suggesting it's a core pattern, but it's only defined in http_server (not in a shared module).
  - fix: Move `server_to_config` to a new module `src/daemon/server_config.rs` (or into db/mod.rs as a method on Server). Document the DB→upstream mapping explicitly as a comment. If this pattern spreads (other DB types need upstream translation), consider a trait: `trait IntoUpstream<T> { fn into_upstream(self) -> Result<T, E>; }`. Implement for Server.
- [low/extensibility/S] **Contract adherence: approval timeout sweeper uses spawn_timeout_sweeper but does not expose JoinHandle for shutdown control**
  - loc: src/daemon/approval.rs:229-256
  - problem: The `spawn_timeout_sweeper` function returns a `JoinHandle<()>` but the caller (main.rs, presumed) may not be storing or cancelling it explicitly. If the sweeper is not awaited during shutdown, the approval table may not be cleaned up before the DB connection is torn down. This is a minor issue because the cancel token is passed, but the function signature could be clearer about the expectation that the handle is kept alive.
  - fix: Add a comment to the function doc: 'The returned handle must be kept alive and awaited or awaited with .abort() during shutdown to ensure the sweeper cleanly exits.' Or, use a scoped spawner pattern where the sweeper is tied to a struct that implements Drop.
- [low/naming/S] **Naming: `PendingApproval.status` field name shadows the approval status enum**
  - loc: src/daemon/approval.rs:20-25 (usage context), src/daemon/db/types.rs (PendingApproval definition, presumed)
  - problem: In approval.rs, the code refers to `ApprovalStatus::Pending`, `ApprovalStatus::Approved`, etc., and also to a `.status` field on the `PendingApproval` struct. While this works, the naming is slightly confusing — it would be clearer to name the field something more specific like `.current_status` or `.resolution_status` to distinguish it from the enum name. This is a minor readability issue.
  - fix: Rename `PendingApproval.status` to `PendingApproval.resolution_status` throughout the DB layer and approval.rs. Update usages accordingly.

#### src/glue/ (Slint ↔ Rust callback wiring and UI refresh layer)
The glue subsystem is architecturally sound with correct thread-boundary enforcement (callbacks on Slint thread, work on tokio runtime, UI updates via upgrade_in_event_loop). However, it exhibits moderate DRY violations through pervasive closure-capture boilerplate (26-59 `*_for_*` variable patterns per file), weak abstraction layering for similar callback shapes, and opportunities for table-driven registry patterns to reduce the Open/Closed principle violations as new IDE/MCP kinds are added. No critical contract violations detected, but code organization and extensibility could improve substantially.

- [high/DRY/M] **Pervasive closure-capture boilerplate (DRY violation)**
  - loc: src/glue/callbacks_servers.rs:37-87, src/glue/callbacks_servers.rs:83-125, src/glue/callbacks_servers.rs:121-131, src/glue/callbacks_ide.rs:33-67, src/glue/callbacks_ide.rs:178-197, src/glue/skills.rs:29-61, src/glue/skills.rs:63-107
  - problem: Every callback repeats the pattern: (1) clone db/rt/skills/approvals, (2) bind to descriptive names (`db_for_X`, `rt_for_X`, `weak_for_X`), (3) clone again inside the closure, (4) spawn onto rt. This duplicates knowledge of 'which dependencies each callback needs' 26-59 times across 287-406 line files. Files like callbacks_servers.rs and callbacks_ide.rs become 30-40% repetitive setup. When a new callback is added or a dependency is shared, every similar handler must be updated in parallel.
  - fix: Introduce a callback-builder abstraction that captures the pattern. Example: a CbBuilder struct holding db, rt, weak, etc., with a method like `spawn_handler(|db, rt, weak| async { ... })` that handles all the cloning and event-loop writes internally. Alternatively, macro-based codegen for the boilerplate. This eliminates ~60% of callback setup code and makes the business logic immediately visible.
- [high/SOLID/L] **Open/Closed violation: IDE client callbacks hardcoded instead of registry**
  - loc: src/glue/callbacks_ide.rs:26-242, src/glue/refresh.rs:328-397
  - problem: IDE config read/validate/save/reload/reveal/remove/retrack/refresh are wired as individual callbacks with duplicate probe/write/refresh patterns. Adding a new IDE client kind requires editing callbacks_ide.rs, adding a new callback shape, and modifying refresh.rs. The logic is 'open to modification', not extension. Similarly, skills callbacks (install/uninstall/toggle/add-repo/remove-repo/toggle-repo) all share a similar shape but are hand-wired. A table-driven registry of (callback_name, handler_fn) pairs keyed by IDE kind or operation type would make the system 'closed for modification'.
  - fix: Define a callback registry trait: `trait CallbackHandler { fn on_event(...) -> BoxFuture; }`. For IDEs, create a static registry: `[(kind, handler)]` mapping. For skills, define a SkillOp enum and handler map. Clients can register new handlers at startup (or at build time via macro). The install_ide_callbacks and install_skills_callbacks functions become meta-loops that register handlers from the registry, not N explicit closures.
- [medium/DRY/S] **Weak<MainWindow> -> oneshot pattern repeated for synchronous reads**
  - loc: src/glue/util.rs:90-97, src/glue/wizard.rs:105-112 (inline pattern)
  - problem: The pattern `let (tx, rx) = tokio::sync::oneshot::channel(); weak.upgrade_in_event_loop(|ui| tx.send(...).ok()); rx.await.ok()` is used in util.rs for `current_selected_server` and duplicated inline in wizard.rs for reading UI state. This is a synchronous read across thread boundaries; it's load-bearing but appears in multiple places without an abstraction.
  - fix: Introduce a helper fn `async fn read_ui_state<T: Send>(weak: &Weak<MainWindow>, f: impl Fn(&MainWindow) -> T + Send) -> Option<T>` that encapsulates the oneshot pattern. Apply it to both current_selected_server and any inline reads in wizard/callbacks to reduce duplication.
- [medium/extensibility/M] **Multiple callback installers all take (window, db, rt, ...) with no abstraction**
  - loc: src/glue/callbacks.rs:27-47, src/glue/callbacks_ide.rs:26-32, src/glue/callbacks_servers.rs:29-36, src/glue/callbacks_misc.rs:18-23, src/glue/skills.rs:24-28, src/glue/wizard.rs:46-52
  - problem: Each install_*_callbacks function signature is slightly different (some omit skills, some omit approvals, etc.). Adding a new shared dependency (e.g., a new manager) requires updating all 6 signatures. The install_callbacks dispatcher in callbacks.rs must know about every installer and thread all the parameters. This violates the Single Responsibility of the dispatcher and makes extensibility painful.
  - fix: Define a CallbackContext struct that bundles all dependencies: `struct CallbackContext { window, db, rt, skills, approvals, bus, port }`. Change each installer to take `&CallbackContext`. The dispatcher becomes `install_callbacks(..., ctx) { [installer(ctx) for each installer] }`. New dependencies are added once to CallbackContext; installers opt-in to what they need. This also makes the contract clearer.
- [medium/DRY/M] **Parallel structure duplication in callbacks_servers vs callbacks_misc for policy/settings**
  - loc: src/glue/callbacks_servers.rs:190-217 (on_change_tool_policy), src/glue/callbacks_misc.rs:50-69 (on_change_policy), src/glue/callbacks_misc.rs:26-46 (on_toggle_open_window_at_startup, on_set_default_unknown_tool_policy)
  - problem: Policy-setting callbacks follow the same shape: clone db, clone rt, spawn async, match on enum, await db.set_*, log errors. The pattern is duplicated for global policies (callbacks_misc.rs:50), per-server policies (callbacks_servers.rs:190), and settings toggles. The only differences are the DB function called and the log message.
  - fix: Factor out a helper fn `async fn set_db_property<T>(db: &Db, setter: impl Fn(&Db) -> impl Future<Output=Result<()>>, name: &str)` that encapsulates the spawn/await/log pattern. Register policy/setting changes via a table-driven dispatcher: `enum DbProperty { Policy(...), Setting(...) }` with a handler map. Eliminates ~3x callback boilerplate.
- [low/cohesion-size/S] **Skill filtering logic inline in callback; could be UI-layer concern**
  - loc: src/glue/skills.rs:204-211 (on_skills_search_changed), src/glue/skills.rs:275-288 (filter_skill_rows)
  - problem: The search callback reads the full skills list from UI state, filters it client-side every keystroke, and re-sets the filtered list. This is correct (non-blocking, ~instant), but `filter_skill_rows` is defined in the glue layer, not in the UI layer. The business logic (case-insensitive substring match on name/description/repo) is fine here, but the coupling to skills-tab data shapes (SkillRow, name, description, repo_label) makes it glue-specific. If skills UI is ever reshaped or rendered differently, this filter must move.
  - fix: Move filter_skill_rows to crate::ui module as a pure function that works on the UI-layer SkillRow type, or keep it in glue but rename to clarify it's a UI-projection helper, not a business rule. Low priority since the code is correct and the file is already 406 lines.
- [medium/DRY/M] **Status messages and UI state transitions duplicated across callbacks**
  - loc: src/glue/callbacks_ide.rs:56-64 (select_ide: set status to empty, clear error), src/glue/callbacks_servers.rs:262-284 (edit_server_clicked: similar reset), src/glue/callbacks_ide.rs:109-112 (save error: set status, set error flag), src/glue/skills.rs:35-39 (set_busy, set_message), src/glue/callbacks_misc.rs:108-111 (parse_paste error recovery)
  - problem: Every callback that shows progress or clears errors repeats the same UI state mutations: `set_status(msg), set_is_error(false/true), set_busy(true/false)`. The order and combination vary slightly, but the underlying shape is identical. This is scattered across 4 callback files and is brittle (if the UI model changes, all these sites must update).
  - fix: Define UI state envelope helpers on MainWindow or as free functions: `fn ui_set_busy_progress(ui, msg: &str) { ui.set_busy(true); ui.set_status(msg); ui.set_error(false); }`, `fn ui_set_error(ui, msg: &str) { ui.set_busy(false); ui.set_status(msg); ui.set_error(true); }`. Apply consistently across callbacks. Reduces mutation boilerplate and centralizes UI contract.
- [medium/cohesion-size/M] **Callback chain patterns (IDE removal triggers refresh_ides, refresh_skills, refresh_server_targets)**
  - loc: src/glue/callbacks_ide.rs:183-197 (on_remove_ide_clicked), src/glue/callbacks_ide.rs:203-219 (on_retrack_ide_clicked), src/glue/callbacks_ide.rs:225-241 (on_refresh_ides_clicked)
  - problem: Three IDE callbacks explicitly orchestrate a sequence of refresh_* calls. The orchestration logic is scattered: remove_ide → refresh_ides, refresh_skills, refresh_server_targets (if a server is selected). If a new refresh operation is needed, all three must be updated. This couples the callback to the refresh graph and makes it fragile.
  - fix: Define a refresh policy enum or graph: `enum RefreshScope { IdesOnly, IdesAndSkillsAndTargets }`. Replace the three scattered refresh calls with a single dispatcher that applies the scope. Better yet, emit a domain event (IdeRemoved, IdeRetracted, IdesRefreshing) and let the event bridge (spawn_event_bridge in refresh.rs) subscribe and decide which refreshes to run. This decouples callbacks from orchestration.
- [medium/error-handling/M] **Error-handling patterns: inconsistent tracing.warn and error ignoring**
  - loc: src/glue/callbacks_servers.rs:42-47 (approvals.resolve error not propagated), src/glue/callbacks_misc.rs:30-34 (silent trace.warn on DB failure), src/glue/wizard.rs:209 (silent trace.warn on retrack), src/glue/callbacks_ide.rs:225-241 (ok().ok() on weak.upgrade, then proceed)
  - problem: Callbacks consistently log errors with tracing.warn but don't reflect failure in UI (no error message shown to user unless explicitly added per-callback). Some callbacks silently return on error (e.g., remove_ide if not found), others proceed with partial state (e.g., on_refresh_ides_clicked sets busy=true, then if retrack fails, busy is never cleared). The pattern is: spawn, await, log, ignore. This can leave UI in a broken state.
  - fix: Standardize: (1) Every long-running callback should set ui_busy(true) at start, ui_busy(false) at end (in both success and error paths). (2) Errors should be mapped to user-facing messages via upgrade_in_event_loop in the error case. (3) Log at warn for user-actionable errors, debug for internal failures. (4) Consider a result-tracking helper: `async fn run_callback_and_report(ui, work, msg_ok, msg_err)` that handles the full lifecycle. This prevents UI lock-up and improves UX.
- [medium/cohesion-size/M] **Large callback files approaching size ceiling without clear boundaries**
  - loc: src/glue/wizard.rs:553 lines, src/glue/refresh.rs:548 lines, src/glue/skills.rs:406 lines
  - problem: wizard.rs (553 lines) combines env-row editing callbacks, probe orchestration, tool policy assignment, and helper functions (lifecycle, headers, env field serialization) in one file. refresh.rs (548 lines) combines refresh_pending, refresh_servers, refresh_server_detail (with detailed health logic), refresh_ides, refresh_policies, refresh_audit, refresh_dashboard all in one module. Both are on/near the 500-line soft guideline and have mixed concerns. Adding new refresh ops (e.g., refresh_policies_with_details) requires touching refresh.rs, and new wizard steps require wizard.rs edits.
  - fix: Split wizard.rs into: (1) wizard_callbacks (probe, save, step navigation), (2) wizard_helpers (env_pairs_from_model, lifecycle_from_picker, etc.) as separate module/file. Split refresh.rs into: (1) refresh_core (pending, servers), (2) refresh_detail (server_detail, server_targets), (3) refresh_polices_audit (policies, audit, dashboard). Keep the coordinators (push_initial_models, spawn_event_bridge) in a coordinator module. Reduces file size, clarifies boundaries, and makes extension easier.
- [low/error-handling/S] **Weak<MainWindow> captures in closures without defensive upgrades in some paths**
  - loc: src/glue/skills.rs:35-39 (ok() on upgrade_in_event_loop), src/glue/callbacks_ide.rs:229-230 (ok() on upgrade_in_event_loop), src/glue/callbacks_servers.rs:231-240 (multiple ok() calls), src/glue/refresh.rs:169-177 (ok() on upgrade_in_event_loop)
  - problem: Weak<MainWindow>.upgrade_in_event_loop returns Result; callbacks uniformly call .ok() to ignore upgrade failures. This is architecturally correct (UI thread gone = best-effort update), but the pattern is scattered without a guard. If upgrade fails, the closure silently does nothing; some paths don't check. Example: callbacks_servers.rs:231-240 calls toggle_server_for_ide unconditionally, then refresh_server_detail, then ok() on the upgrade. If upgrade fails, refresh is skipped. Inconsistent defensive checks.
  - fix: Define a consistent guard pattern: either (a) never call ok() — let the Result bubble and log at the call site in refresh.rs, or (b) always wrap: `if let Ok(_) = weak.upgrade_in_event_loop(...) { ... }`. Choose one and apply uniformly. Low priority since the code is defensive by design (worst case: UI update skipped), but consistency improves readability.
- [low/DRY/S] **Inline string literals for policy/status values instead of constants**
  - loc: src/glue/callbacks_ide.rs:45 ("active"), src/glue/callbacks_ide.rs:80 ("valid JSON"), src/glue/callbacks_ide.rs:110 ("no config path"), src/glue/callbacks_servers.rs:199 ("*" for wildcard tool), src/glue/wizard.rs:239 ("ask" for default decision), src/glue/skills.rs:36-37 ("Refreshing…", "Installing…")
  - problem: Policy decisions, status strings, and UI messages are hardcoded throughout callbacks. Changing 'ask' to 'ask_each_time' requires editing wizard.rs, skills.rs, refresh.rs in parallel. Wildcard '*' for 'apply to all tools' appears only in callbacks_servers.rs:199, but the logic is duplicated conceptually in wizard.rs. This violates DRY for business constants.
  - fix: Define a constants module: `pub const DECISION_ASK: &str = "ask"; pub const DECISION_ALLOW: &str = "allow"; pub const WILDCARD_TOOL: &str = "*"; pub const UI_STATUS_BUSY_CONNECTING: &str = "Connecting..."`. Import and use throughout. Centralizes policy/UI strings and makes them easy to audit or internationalize.

#### gui (tray, notifications, hotkey, notification_router)
The gui subsystem is generally well-structured with clear architectural boundaries (dedicated threads own non-Send resources, communication via mpsc/broadcast). Thread-affinity rules are respected. However, there are notable violations of DRY principles (duplicate query logic for pending approvals and auth failures across init + event loop), a string-literal-based action dispatch that lacks extensibility, one acceptable use of unwrap for overflow handling, and minor error-handling inconsistencies (catch-all in notification action matching).

- [medium/DRY/S] **Duplicate pending-approval query logic in spawn_badge_updater**
  - loc: src/gui/tray.rs:274-276, src/gui/tray.rs:288-290
  - problem: The logic to query pending approvals and convert to i64 count is repeated between initial load and event-loop update. This duplicates knowledge about how to fetch and count approvals, violates DRY, and creates a risk that the two branches diverge if the count logic changes.
  - fix: Extract a shared async fn `get_pending_count(db: &Db) -> i64` that encapsulates the list_pending_approvals().await followed by try_from(pending.len()).unwrap_or(i64::MAX). Call it from both locations (lines 274-276 and 288-290).
- [medium/DRY/M] **count_auth_failures duplicates knowledge about server health checks**
  - loc: src/gui/tray.rs:309-318
  - problem: The pattern 'list all servers, filter those with last_auth_error_msg.is_some(), count' is specific knowledge about auth-failure state. This same logic may be needed elsewhere (glue module, HTTP routes, etc.) but is only implemented here. If the definition of 'auth failure' changes, this function must be found and updated manually.
  - fix: Move this logic to a Db method or a daemon::util module as a reusable auth-state query, e.g., `Db::count_auth_failures()` or `fn count_failed_servers(servers: &[Server]) -> i64`. This makes the knowledge explicit and centralized.
- [medium/extensibility/M] **String-literal action dispatch in notifications.rs lacks extensibility**
  - loc: src/gui/notifications.rs:144-153
  - problem: Notification actions are dispatched via string literals ("default", "approve", "deny", "approve_always") matched in a closure. If a new action type is added (e.g., "escalate", "snooze"), both the notify_rust call (lines 125-129) and the match statement (lines 144-153) must be updated in sync, in different places. The action names are not unified by a type or constant table. This violates SOLID (Open/Closed: adding an action forces edits in tray + notification_router + notifications).
  - fix: Define an action registry as `const NOTIFICATION_ACTIONS: &[(&str, &str)] = &[("approve", "Approve"), ("deny", "Deny"), ...]` at the top level, and loop over it to register actions on the notify-rust Notification. Then in the match (line 144), use the same registry keys. This way, adding an action is one edit to a table, not three edits across different functions. Alternatively, define a private enum mapping to string literals and use a trait-based dispatch.
- [high/contract-violation/M] **Unbounded spawning of notification-close threads per ApprovalResolved event**
  - loc: src/gui/notifications.rs:74-78
  - problem: For every Event::ApprovalResolved, a new std::thread is spawned to call close_by_id(). If many approvals resolve in rapid succession (e.g., a skill auto-approves 100 calls), this spawns 100 threads, each of which blocks on notify_rust synchronously. The architectural contract (§3: Async, bounded tokio::spawn in loops using JoinSet+Semaphore) explicitly forbids unbounded spawning. While this is a std::thread (not tokio::spawn), the same principle applies — unbounded threads exhaust system resources.
  - fix: Collect pending close operations in a queue or use a tokio::spawn with a bounded Semaphore, or batch close operations into a single background task per notification window. For the simplest fix: use a tokio::spawn + tokio::sync::Semaphore to limit concurrent close operations, e.g., `tokio::spawn(async move { let _permit = semaphore.acquire().await; close_by_id(nid); })`, capping at e.g. 10 concurrent closes.
- [high/contract-violation/M] **Unbounded spawning of per-notification threads in spawn_notification (Linux)**
  - loc: src/gui/notifications.rs:117-119, src/gui/notifications.rs:75-77
  - problem: For every Event::ApprovalPending on Linux, a dedicated std::thread is spawned that blocks on handle.wait_for_action(). In a high-approval scenario (e.g., discovery returns 1000 tools to approve one by one), this creates 1000 threads, all blocked. The contract forbids unbounded spawning. Even though spawning is async-context-safe here, the resource exhaustion is a runtime risk.
  - fix: Implement a bounded thread pool (e.g., via crossbeam::queue or threadpool crate) or use tokio::sync::Semaphore to cap concurrent notification threads. Or re-architect to batch notifications and reuse a single worker thread per N notifications. A simple bounded approach: wrap the spawn in a Semaphore acquire/release, capping at e.g. 32 concurrent notification threads.
- [high/contract-violation/S] **Mutex lock held during long-blocking operation (notify_rust.wait_for_action)**
  - loc: src/gui/notifications.rs:138-162
  - problem: On line 138, registry.lock() is acquired, then map.insert() adds the notification ID. The lock is then released, but wait_for_action() (line 143) blocks for up to the user-interaction timeout (potentially indefinite). If the notification isn't dismissed quickly, the registry is uncontended, so this is not a critical lock-hold. However, on lines 159-160, the lock is reacquired and the entry is removed. If another thread tries to look up the notif_id during the wait, it will see it registered. The design is defensible but the split lock-acquire+release across a long-blocking action is subtle and could be made clearer. More critically, if the registry is later used for synchronization (which it currently isn't, but could be), this pattern breaks.
  - fix: Move the registry.insert() call to happen only if wait_for_action() completes successfully, or structure as: insert → wait (unlocked) → remove. Alternatively, use a more explicit lifetime guard (e.g., a RAII NotificationGuard struct that unregisters in Drop). Document that registry removal is idempotent if the entry is missing.
- [medium/readability/S] **Potential logic error: Ok(None) treated same as Err in IDE lookup**
  - loc: src/gui/notifications.rs:53-56
  - problem: When looking up an IDE by key, the code is: `Ok(Some(k)) => k.ide_name, Ok(None) | Err(_) => "Unknown IDE".to_string()`. This treats 'key not found' the same as 'database error', which masks whether the lookup actually failed or the IDE is just unregistered. From a user perspective, both show 'Unknown IDE', but from a diagnostics perspective, an Err should be logged separately. This makes it harder to debug why an IDE name isn't rendering.
  - fix: Split the match: `Ok(Some(k)) => k.ide_name, Ok(None) => { tracing::debug!(key = %ide_key, "IDE key not registered"); "Unknown IDE".to_string() }, Err(e) => { tracing::warn!(error = %e, key = %ide_key, "IDE lookup failed"); "Unknown IDE".to_string() }`. This preserves the same UI outcome but adds observability.
- [low/readability/S] **Overflow fallback in tray (i64::MAX) lacks context on why it's safe**
  - loc: src/gui/tray.rs:275, src/gui/tray.rs:289, src/gui/tray.rs:317
  - problem: The code uses `i64::try_from(pending.len()).unwrap_or(i64::MAX)` to convert a usize count to i64. The fallback to i64::MAX silently hides overflow (though Vec::len() will never reach i64::MAX in practice). Without a comment, this looks like defensive programming but is actually unreachable. The readable intent is: 'cap at max displayable count', but that's not explicit.
  - fix: Add a comment: `// len() fits in i64; overflow is unreachable but fallback to max displayable count` or use a debug_assert to verify the assumption, e.g., `let n = i64::try_from(pending.len()).expect("too many pending approvals");` if this should be caught, or keep the pattern but add a one-line comment explaining why.
- [low/error-handling/S] **Catch-all in action_id match hides unknown actions silently**
  - loc: src/gui/notifications.rs:153
  - problem: The match on action_id includes `_ => None`, which silently ignores any unregistered action strings. If notify_rust ever fires an action name we didn't register (e.g., due to a platform-specific extension), the action is dropped with no trace. This is not a bug (None → no send → silent skip), but it's fragile for future changes.
  - fix: Log unexpected actions: `_ => { tracing::warn!(action_id = %action_id, "unregistered notification action"); None }`. This helps catch bugs if action names drift between registration (lines 125-129) and dispatch (lines 144-153).
- [low/readability/S] **TrayCmd enum variants duplicated in naming but not in semantic clarity**
  - loc: src/gui/tray.rs:50-54
  - problem: The enum variants (SetPendingCount, SetAuthFailureCount, SetPaused) all start with 'Set', and the clippy expect attribute acknowledges this. While the attribute suppresses the lint, the naming suggests a more general 'command setter' pattern that could scale to other badge-update scenarios (e.g., SetErrorCount, SetConnectedCount). The enum is fine as-is, but the expect comment suggests awareness of a pattern that isn't fully formalized.
  - fix: No change required, but document the pattern: 'TrayCmd variants are imperative setters for tray badge state (pending, auth, paused). Add new variants here when badge state sources change.' This clarifies that the naming is intentional and guides future contributors.
- [low/readability/S] **Tray thread poll loop uses busy-sleep pattern instead of event-driven wakeup**
  - loc: src/gui/tray.rs:202-256
  - problem: The tray thread's main loop (lines 202-256) uses try_recv on both MenuEvent and cmd_rx, then sleeps 100ms (POLL_INTERVAL). This is a polling loop, not event-driven. The comment on line 80 notes 'upper bound ~100ms' for cancel-token detection, justifying the sleep. However, this pattern means badge updates take up to 100ms to render, and the loop burns CPU checking for events every 100ms even when idle. For a tray icon, this is acceptable, but it's not optimal.
  - fix: No change required for correctness (the contract is honored, cancellation is bounded), but document: 'Tray thread uses polling (not async select!) because MenuItem/MenuEvent APIs are synchronous/blocking. POLL_INTERVAL caps responsiveness; GTK drain (lines 206-210) and try_recv avoid blocking. This trade-off is acceptable for tray latency.' This explains the architecture choice and prevents future refactors from 'fixing' it incorrectly.
- [low/cohesion-size/S] **File cohesion: tray.rs is large (424 lines); consider splitting icon generation**
  - loc: src/gui/tray.rs:349-399
  - problem: The file tray.rs is 424 lines, approaching the soft 500-line limit. The build_icon function (lines 351-399) is a self-contained icon-rendering utility that could live in its own module (gui/icon.rs or gui/tray_icon.rs). This would improve readability and make the icon logic reusable if needed elsewhere (e.g., app icon, system image, etc.).
  - fix: Extract lines 349-399 (build_icon, ICON_SIZE, ATTENTION_DOT_RADIUS, glyph/dot color constants, and tests) into a new file src/gui/tray_icon.rs. Expose build_icon as pub fn. Import it in tray.rs with `use crate::gui::tray_icon::build_icon;`. This reduces tray.rs to ~340 lines (well under limit) and makes icon logic a reusable component. Update src/gui/mod.rs to pub mod tray_icon;.

#### UI (Slint markup) subsystem
The UI subsystem is fundamentally sound with respect to architectural contracts—all state lives in the Rust glue, callbacks respect thread affinity, and Slint views are pushed via upgrade_in_event_loop. Code cleanliness is strong: components are well-factored, naming is explicit, and the design token system (theme.slint) is properly centralized. However, three moderate DRY issues exist: (1) decision-state color logic (allow/ask/deny → accent/warning/danger) is repeated across dashboard.slint, log.slint, tool_policy.slint, and settings.slint; (2) status-to-color mappings (health_state, ide_status → tint) are scattered across multiple tabs without a helper; and (3) the six tab names are hardcoded as a string array in main.slint and the wizard steps as a separate array in modal.slint, forcing future tab/step additions to edit core logic. Files are all under 500 lines and well-organized by concern, reducing cohesion risks.

- [medium/DRY/M] **Repeated decision→color mapping logic (allow/ask/deny)**
  - loc: src/ui/components/tool_policy.slint:43-58, src/ui/tabs/dashboard.slint:422-435, src/ui/tabs/log.slint:37-39, src/ui/tabs/settings.slint:85-99
  - problem: The same decision enum (allow/ask/deny) is mapped to colors (accent/warning/danger) in four independent places. Each time a color or decision state changes, all four must be updated. This violates DRY for business logic (decision→visual mapping) and creates a maintenance burden.
  - fix: Create a reusable helper component `DecisionChip` or `PolicyBadge` that encapsulates the decision→color mapping logic (takes decision: string, outputs color). Use it in all four locations.
- [medium/DRY/M] **Status-to-color tint logic scattered across tabs**
  - loc: src/ui/tabs/servers.slint:92-96, src/ui/tabs/editors.slint:121-124, src/ui/tabs/log.slint:37-39, src/ui/tabs/dashboard.slint:276
  - problem: Status values (live/idle/down, active/stale/not_tracked) are each converted to a tint color (success/warning/danger) inline in multiple tabs. The mapping is not centralized, so adding a new status or changing color semantics requires editing multiple files.
  - fix: Add helper components `StatusDotWithLogic` or similar for each status domain (health_state, ide_status, decision) that encapsulate the mapping. Alternatively, use computed properties at the tab level to reduce inline conditionals.
- [high/extensibility/L] **Tab names hardcoded as string array in main.slint**
  - loc: src/ui/main.slint:214
  - problem: The tab labels `["Dashboard", "Servers", "Editors", "Skills", "Log", "Settings"]` are hardcoded in a for loop that also generates tab indices (0-5). Adding a new tab requires: (1) editing this array, (2) adding a new `in property` or `in-out property` for the tab's data, (3) adding an `if root.active-tab == N` block for the new tab's content in the same file, and (4) updating the help tab icon hardcoded to index 6 (line 228-229). This forces core logic edits instead of extension-by-registration. The active-tab system is positional, not semantic.
  - fix: Introduce a struct TabMetadata (name: string, index: int) or similar and pass a hardcoded-but-named array of registered tabs. Better yet, move each tab's registration into a trait or data structure that the Rust glue populates, decoupling tab discovery from main.slint's layout logic. If that's too heavy, at minimum extract the tab-labels array into a shared constant (in structs.slint or a new constants.slint).
- [low/extensibility/S] **Wizard step names hardcoded in modal.slint**
  - loc: src/ui/components/modal.slint:50
  - problem: The wizard step labels `["Connection", "Probing", "Permissions"]` are hardcoded. If the wizard flow changes (e.g., add/remove a step), this array and the corresponding `if root.step == N` blocks in add_server_dialog.slint must be updated in two files. Not as critical as tabs (wizard is less likely to change), but the pattern is the same.
  - fix: Extract step labels into a constant in structs.slint or a new constants file, or move the step array definition to the parent (add_server_dialog) and pass it as a property to WizardStepIndicator.
- [low/DRY/S] **Modal overlay pattern repeated without a unifying abstraction**
  - loc: src/ui/main.slint:387-424, src/ui/main.slint:425-461, src/ui/main.slint:480-497, src/ui/components/approval_modal.slint:50-68
  - problem: Each modal (AddServerDialog, delete confirmation, approval detail) follows the same scrim+centered-card pattern: `if condition : Component { dismiss => { ... reset state ... } }`. The reset logic (setting show-*-dialog to false, clearing pending-* fields) is duplicated. Not critical (Slint has no inheritance), but reduces cohesion.
  - fix: Document the modal protocol in a comment at the top of main.slint, or create a ModalHost mixin that standardizes dismiss-and-reset behavior. Low priority if the current pattern is stable.
- [low/readability/S] **Computed properties for health status and policy decision rendering could be centralized**
  - loc: src/ui/tabs/dashboard.slint:82-87, src/ui/tabs/servers.slint:59-136, src/ui/components/tool_policy.slint:9-82
  - problem: Long conditional chains for rendering status labels (e.g., `detail.health-state == "live" ? "Connected & Healthy" : detail.health-state == "idle" ? "Idle" : ...`) are embedded inline in markup. These reduce readability and scatter the business logic (state-to-label mapping).
  - fix: Define computed properties or helper functions (in Rust or as Slint `property <string>` in a parent component) that map states to display labels and colors. Example: expose `health_label: string` from ServersTab computed from `detail.health-state`.
- [low/readability/S] **No contract docs on Slint↔Rust data flow**
  - loc: src/ui/main.slint:1-5, src/ui/structs.slint:1-5
  - problem: structs.slint briefly notes that exported structs become Rust types, but main.slint and tabs lack inline documentation of which properties are `in` vs `in-out`, where mutations happen, and which callbacks are async (offloaded to worker). This makes it harder for maintainers to follow the thread-affinity contract when adding features.
  - fix: Add a comment block at the top of main.slint (or a DESIGN.md anchor) documenting: (1) all `in` properties are pushed from Rust via refresh_*; (2) all `in-out` and callbacks flow back to Rust callbacks (which spawn work on tokio); (3) the push_initial_models and spawn_event_bridge pattern. Link to the glue/ module docs.
- [low/cohesion-size/M] **add_server_dialog.slint is close to the 500-line soft limit at 492 lines**
  - loc: src/ui/tabs/add_server_dialog.slint:1
  - problem: The Add Server wizard dialog handles three distinct steps (form, probing + OAuth hold, permissions) in a single file. At 492 lines, it's nearing the soft 500-line guideline. If features are added (e.g., a validation step or advanced options), it could cross into the 600+ range where cognitive load becomes a problem.
  - fix: Consider factoring the three steps into separate components (AddServerFormStep, AddServerProbeStep, AddServerPermissionsStep) in a new `components/` subdirectory. Each would be ~150–180 lines and reusable. Keep the parent dialog as a light orchestrator.
- [low/cohesion-size/M] **dashboard.slint contains two independent sections that could be split**
  - loc: src/ui/tabs/dashboard.slint:89-217, src/ui/tabs/dashboard.slint:219-453
  - problem: The dashboard has two major layouts: (1) Hero + KPI grid (lines 89–217), and (2) Active Servers + Recent Activity panels (lines 219–453). They serve different purposes, and the file is 454 lines. Splitting would improve readability and allow independent reuse.
  - fix: Factor into DashboardHeroSection (hero + KPIs) and DashboardActivitySection (servers + audit). Keep main DashboardTab as a thin orchestrator. Each section would be ~220 lines and more self-contained.

#### Entry points + Installer + Runtime (src/main.rs, src/lib.rs, src/runtime.rs, src/paths.rs, src/ui.rs, src/installer/)
This subsystem is well-architected in most areas: thread-affinity is correct (main thread owns Slint, worker thread owns tokio, DB actor is separate), cancellation is properly propagated, and error handling is generally good using thiserror with proper context chaining. Runtime and Paths modules are clean and focused. However, there are two concrete DRY violations in the installer module (xml_escape and atomic_write duplicated across platforms), one architectural contract violation (unbounded_channel violates the "no unbounded_channel" rule), one error-handling contract violation (unwrap_or in spawn_blocking context), and moderate extensibility friction in the callback glue layer due to repetitive clone-and-spawn patterns.

- [medium/DRY/S] **Duplicated xml_escape function across installer platforms**
  - loc: src/installer/launchd_macos.rs:98-111, src/installer/scheduled_task_windows.rs:116-129
  - problem: The xml_escape function is identical in launchd_macos.rs and scheduled_task_windows.rs (11 lines each). This duplicates the knowledge of XML entity escaping rules. Any future bug in entity handling must be fixed in two places, and adding support for new XML entities requires changes to multiple modules.
  - fix: Extract xml_escape into a shared module-level function in src/installer/mod.rs or a dedicated utils submodule, then import and call it from both platform-specific modules. This centralizes the escaping logic and makes it easier to test.
- [medium/DRY/S] **Duplicated atomic_write function across installer platforms**
  - loc: src/installer/systemd_linux.rs:75-90, src/installer/launchd_macos.rs:81-96
  - problem: The atomic_write function is nearly identical in both files (15 lines each). The only difference is the fallback constant name (SERVICE_FILE_NAME vs PLIST_FILE_NAME). This duplicates knowledge about atomic file writing semantics and temporary file cleanup. A bug in the atomic write logic requires fixes in two places.
  - fix: Extract atomic_write into a shared utility function in src/installer/mod.rs that takes the fallback filename as a parameter. Call it from both platform modules. This reduces duplication and centralizes the critical file-write safety logic.
- [high/contract-violation/M] **Unbounded channel violates async contract**
  - loc: src/main.rs:231-232
  - problem: The notification action channel is created with unbounded_channel() which violates the architectural contract: 'Async: exactly one runtime; multi-thread; cancel-safe select! branches; bounded tokio::spawn in loops (JoinSet+Semaphore); no unbounded_channel; no blocking I/O in async fn.' Unbounded channels can lead to memory exhaustion if the dispatcher (action_rx loop) falls behind notification producers. The contract requires bounded channels everywhere to prevent resource leaks.
  - fix: Replace unbounded_channel with a bounded mpsc::channel(capacity) where capacity is chosen based on the expected notification rate (e.g., 100-1000 pending actions). Update the spawn at line 244 to handle backpressure if the channel is full (log or drop older notifications). Document the chosen capacity with a comment explaining the rationale.
- [high/error-handling/S] **Error suppression in spawn_blocking task**
  - loc: src/main.rs:121-127
  - problem: The spawn_blocking call uses .await.unwrap_or(0), which silently drops JoinError if the task panics or is cancelled. This violates the contract: 'no unwrap/expect/panic/todo in non-test code'. If purge_draft_oauth_tokens panics or the task is abruptly cancelled, the error is lost. The startup sequence continues silently, which could leave stale tokens if the cancellation was the real issue.
  - fix: Handle the JoinError explicitly: match spawn_blocking(...).await { Ok(count) => ..., Err(e) => tracing::warn!(error = %e, 'purge oauth drafts task failed'), }. This ensures errors are logged and the startup behavior is explicit and debuggable.
- [medium/extensibility/M] **Callback boilerplate: repeated clone-and-spawn pattern**
  - loc: src/glue/callbacks_servers.rs:37-49, 51-60, 62-81, 83-119, 121-131, 133-185, 187-218, 220-233, 249-286, src/glue/callbacks_ide.rs (9 instances), src/glue/callbacks_misc.rs (7 instances)
  - problem: Every callback in callbacks_servers.rs (and similar files) follows the same pattern: clone resources (db, rt, weak, approvals, bus) before the callback, then clone again inside the closure. This is 37 lines of repetitive setup for ~9 callbacks. With 37 total clones in callbacks_servers.rs alone, adding a new callback or dependency requires copying this boilerplate. This makes the code harder to refactor and creates friction for extensibility. The pattern appears 37 times across all callback files.
  - fix: Introduce a helper macro or builder struct to capture the common pattern: wrap the cloned dependencies in a struct, then pass it to a callback installer function. For example: define a CallbackContext { db: Db, rt: Handle, weak: Weak<MainWindow>, bus: Bus } and create a helper like `install_server_callback(window, ctx, handler_fn)` that does the cloning internally. This centralizes boilerplate and makes adding new callbacks a one-liner.
- [low/extensibility/L] **Tab constants are hardcoded indices, not extensible**
  - loc: src/ui.rs:28-36
  - problem: Tab indices are defined as literal constants (APPROVALS=0, SERVERS=1, ..., HELP=6). If a new tab is added to the UI, this file and all code referencing tab indices must be manually updated. The index-based design is brittle; renumbering or reordering breaks references. Adding a new tab forces edits across multiple files.
  - fix: Consider switching to string-based tab identifiers or a scoped enum once the UI framework supports it better. If staying with indices, at least group all tab definitions in a single const block with clear ordering comments. For now, document the tab index order and make this const file the single source of truth.
- [low/cohesion-size/S] **Installer module is exported but never called**
  - loc: src/lib.rs:15, src/installer/mod.rs (entire module)
  - problem: The installer module is public in lib.rs but is never imported or called from any binary code (main.rs, daemon, or gui). It is only used in tests (tests/installer.rs). This creates maintenance burden: changes to the installer signature must be backward-compatible even though no production code uses it. The module's public API is exposed but unused, which violates cohesion principles.
  - fix: Document explicitly in the module that it is currently test-only and available for future GUI integration (e.g., a Preferences > Autostart toggle). Or, if this is truly dead code, move it to a tests-only submodule or feature gate behind #[cfg(test)]. If it will be called soon, file a task to integrate it and add a FIXME comment here.

<!-- END per-subsystem findings -->
