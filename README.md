# Conduit

> **MCPs and skills, synced to every IDE.**

Install an MCP server once. Add a skill once. Conduit mirrors both into Cursor,
VSCode, Claude Code, Opencode, Openclaude, and Codex — and keeps them in sync
as you add, remove, or swap IDEs.

---

## ✨ Features

- 🔌 **One Tool, every IDE.** Add an MCP server in here; it shows up in every IDE. Remove it once, it leaves everywhere.
- 📦 **Skills sync.** Install a SKILL.md package once, and Conduit drops it into every agentic IDE's skills directory.
- 🛡️ **Per-tool approval gates.** First call to a new tool prompts you — always-allow, ask-each-time, or deny. Full audit trail in SQLite.
- 🌐 **HTTP MCP proxy.** Conduit fronts every upstream at localhost, so HTTP-only IDEs can use stdio MCP servers too.
- 🔑 **OAuth, handled.** Browser flow for upstreams that need it. Tokens live in the OS keyring — never in plaintext on disk. HTTP OAuth servers must support [Dynamic Client Registration](https://datatracker.ietf.org/doc/html/rfc7591) (RFC 7591); a pre-registered `client_id` isn't supported yet.
- 🦀 **Single Rust binary.** Pure Rust + Slint. No webview, no Node, no Electron. ~20 MB on disk, ~80 MB RAM idle.

## 🚀 Quick start

```sh
git clone https://github.com/Anirban-Majumder/conduit
cd conduit
cargo run
```

> **Linux deps:** `sudo pacman -S xdotool libxkbcommon` (Arch) or
> `sudo apt install libxdo-dev libxkbcommon-dev` (Debian/Ubuntu).

## 🔨 Build

```sh
cargo build --release
./target/release/conduit
```

## 🧪 Verify

```sh
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo test
```

## 🧭 How it works

Conduit is a tray app and an HTTP daemon in the same process. It binds
`127.0.0.1:9876`, hands each IDE a random 8-char key, and rewrites that IDE's
MCP config to point at its keyed URL. The tray window is where you manage
servers, grant approvals, and watch the audit log — close it and Conduit
keeps routing in the background.

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



## 🖥️ Supported clients

| Client          | MCP | Skills |
| --------------- | :-: | :----: |
| Claude Code     | ✅  |   ✅   |
| Cursor          | ✅  |   ✅   |
| VS Code Copilot | ✅  |   —    |
| OpenCode        | ✅  |   ✅   |
| OpenClaude      | ✅  |   ✅   |
| Codex           | ✅  |   ✅   |

Conduit syncs MCP servers (and where supported, skills) into the config files
each client auto-loads. Skills are placed in each IDE's skills directory as a
directory symlink to the canonical managed copy — edits propagate everywhere
for free; recursive copy is the fallback when symlinks aren't allowed
(sandboxed Windows accounts).


### Adding a new client

Copy [`src/daemon/clients/example.rs`](src/daemon/clients/example.rs) to
`src/daemon/clients/<name>.rs`, fill in the 3-line citation header from
authoritative docs/source (never invent URLs), then wire the new variant
through `IdeKind` and the dispatchers in [`mod.rs`](src/daemon/clients/mod.rs). 

## 📄 License

Apache-2.0. See [LICENSE](LICENSE).
