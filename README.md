# Conduit

> **MCPs and skills, synced to every IDE.**

Install an MCP server once. Add a skill once. Conduit mirrors both into Cursor,
VSCode, Claude Code, Opencode, Openclaude, and Codex — and keeps them in sync
as you add, remove, or swap IDEs.

---

## ✨ Features

- 🔌 **One catalog, every IDE.** Add an MCP server in Conduit; it shows up in
  every IDE you've connected. Remove it once, it leaves everywhere.
- 📦 **Skills sync.** Install a SKILL.md package once, and Conduit drops it into
  every agentic IDE's skills directory.
- 🛡️ **Per-tool approval gates.** First call to a new tool prompts you —
  always-allow, ask-each-time, or deny. Full audit trail in SQLite.
- 🌐 **HTTP MCP proxy.** Conduit fronts every upstream at
  `http://127.0.0.1:9876/<ide-key>/<server>/mcp`, so HTTP-only IDEs can use
  stdio MCP servers too.
- 🔑 **OAuth, handled.** Browser flow for upstreams that need it. Tokens live
  in the OS keyring — never in plaintext on disk.
- 🦀 **Single Rust binary.** Pure Rust + Slint. No webview, no Node, no
  Electron. ~20 MB on disk, ~80 MB RAM idle.

## 🖥️ Supported clients

Conduit syncs MCP servers (and where supported, skills) into the config files
each client auto-loads. Per-client module lives in
[`src/daemon/clients/`](src/daemon/clients/) with a 3-line header citing the
upstream source for every path — if a vendor changes their schema, the fix
is one file, with the citation right there.

| Client | MCP config | Top-level key | Required fields | Skills dir |
|---|---|---|---|---|
| Claude Code | `~/.claude.json` | `mcpServers` | `type:"http"`, `url` | `~/.claude/skills/` |
| Cursor | `~/.cursor/mcp.json` | `mcpServers` | `url` (no type) | `~/.cursor/skills/` |
| VS Code Copilot | `<Code/User>/mcp.json` | `servers` | `type:"http"`, `url` | — |
| OpenCode | `~/.config/opencode/opencode.json` | `mcp` | `type:"remote"`, `url` | `~/.config/opencode/skills/` |
| OpenClaude | `~/.openclaude.json` | `mcpServers` | `type:"http"`, `url` | `~/.openclaude/skills/` |
| Codex | `~/.codex/config.toml` | `mcp_servers` (TOML) | `url` (no type) | `~/.codex/skills/` |

### Adding a new client

Copy [`src/daemon/clients/example.rs`](src/daemon/clients/example.rs) to
`src/daemon/clients/<name>.rs`, fill in the 3-line citation header from
authoritative docs/source (never invent URLs), then wire the new variant
through `IdeKind` and the dispatchers in
[`mod.rs`](src/daemon/clients/mod.rs). The example template:

```rust
// Repo:   <upstream repo URL, or N/A if closed-source>
// MCP:    <config file path>; top-level `<key>`; schema `<one-line>`. Source: <docs/repo URL>
// Skills: <skills dir path, or "not supported">. Source: <docs/repo URL, or N/A>

use std::path::{Path, PathBuf};

pub(crate) const IDE_NAME: &str = "ExampleClient";

pub(crate) fn config_path_under(home: &Path) -> PathBuf {
    home.join(".example").join("config.json")
}

pub(crate) fn config_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(config_path_under(&home))
}

// Delete these two if the client has no skills directory.
pub(crate) fn skills_dir_under(home: &Path) -> PathBuf {
    home.join(".example").join("skills")
}

pub(crate) fn skills_dir() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(skills_dir_under(&home))
}
```

Then in [`mod.rs`](src/daemon/clients/mod.rs):

1. Add a variant to `IdeKind` in `crate::daemon::ide_detect`.
2. `pub mod <name>;`
3. Map the variant in `json_schema_for` (top-level key + optional `type`),
   OR add a TOML branch like Codex.
4. Map the variant in `config_path_for` / `config_path_under` /
   `ide_kind_from_db_name` / `ide_name`.
5. If skills are auto-loaded, map it in `skills_dir_for` /
   `skills_dir_under` and add to `SKILL_TARGET_IDES`.

## 🧭 How it works

Conduit is a tray app and an HTTP daemon in the same process. It binds
`127.0.0.1:9876`, hands each IDE a random 8-char key, and rewrites that IDE's
MCP config to point at its keyed URL. The tray window is where you manage
servers, grant approvals, and watch the audit log — close it and Conduit
keeps routing in the background.

## 🚀 Quick start

```sh
git clone https://github.com/Anirban-Majumder/conduit
cd conduit
cargo build --release
./target/release/conduit
```

> **Linux deps:** `sudo pacman -S xdotool libxkbcommon` (Arch) or
> `sudo apt install libxdo-dev libxkbcommon-dev` (Debian/Ubuntu).

## 📁 Where state lives

| OS      | Path                                              |
| ------- | ------------------------------------------------- |
| Linux   | `~/.config/conduit/`                              |
| macOS   | `~/Library/Application Support/conduit/`          |
| Windows | `%APPDATA%\conduit\`                              |

Inside that directory:

```
conduit/
├── config.toml      # user settings (port, approval timeout, …)
├── state.db         # SQLite: servers, policies, audit log
├── installs/        # auto-installed upstream MCP packages
├── skills/managed/  # canonical copies of installed skills
└── logs/daemon.log
```

Secrets — env-var values and OAuth tokens — never touch disk in plaintext.
Conduit uses the OS keyring: `gnome-keyring` / `KWallet` on Linux, Keychain on
macOS, Credential Manager on Windows.

## 🧪 Verify

```sh
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo test
```

## 📄 License

Apache-2.0. See [LICENSE](LICENSE).
