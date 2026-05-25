// IDE config writers: Claude Code, Cursor, VS Code Copilot, OpenCode, OpenClaude.
// Each per-IDE module reads the existing config (preserving user entries),
// merges our entries (URL: http://127.0.0.1:<port>/<key>/<server>/mcp), and
// atomically writes back. See spec §Add IDE flow > IDE config paths.
//
// Submodules added as their implementations land.
