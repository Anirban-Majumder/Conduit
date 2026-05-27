# Conduit

> **MCPs and skills, synced to every IDE.**

Install an MCP server once. Add a skill once. Conduit mirrors both into Cursor,
VSCode, Claude Code, Opencode, and Openclaude — and keeps them in sync as you
add, remove, or swap IDEs.

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
