// Vercel-style paste-magic parser for the Add MCP server import modal.
// Spec §Add MCP server flow > Import modal.
//
// Inputs (any of):
//   - Full MCP JSON `{ "mcpServers": {...} }`
//   - JSON fragment `{ "command": ..., "env": ... }`
//   - Plain JSON object of env vars
//   - Dotenv-style lines (handles `export`, quotes, comments, blank lines)
//   - KEY=value single line
//   - YAML-ish KEY: value lines
