// Built-in lifecycle allow-list. Spec Appendix A — 30 top MCP servers with
// recommended lifecycle_seconds. Used on Add Server to seed defaults; user
// can override in GUI.

const TABLE: &[(&str, i32)] = &[
    ("@modelcontextprotocol/server-filesystem", 30),
    ("@modelcontextprotocol/server-fetch", 0),
    ("@modelcontextprotocol/server-memory", 300),
    ("@modelcontextprotocol/server-sequential-thinking", -1),
    ("@modelcontextprotocol/server-git", 30),
    ("@modelcontextprotocol/server-github", 30),
    ("@modelcontextprotocol/server-gitlab", 30),
    ("@modelcontextprotocol/server-slack", 60),
    ("@modelcontextprotocol/server-postgres", 300),
    ("@modelcontextprotocol/server-sqlite", 60),
    ("@modelcontextprotocol/server-puppeteer", -1),
    ("@modelcontextprotocol/server-everything", 30),
    ("@modelcontextprotocol/server-brave-search", 30),
    ("@modelcontextprotocol/server-google-drive", 300),
    ("@modelcontextprotocol/server-google-maps", 30),
    ("mcp-server-time", 0),
    ("@upstash/context7-mcp", 0),
    ("@microsoft/mcp-server-playwright", -1),
    ("mcp-atlassian", 1800),
    ("@notionhq/notion-mcp-server", 300),
    ("linear-mcp", 300),
    ("mcp-obsidian", 60),
    ("@modelcontextprotocol/server-docker", 60),
    ("mcp-server-kubernetes", 60),
    ("perplexity-mcp", 30),
    ("tavily-mcp", 30),
    ("firecrawl-mcp", 30),
    ("mcp-browser-use", -1),
    ("exa-mcp", 30),
    ("mcp-server-sentry", 60),
];

/// Returns the recommended `lifecycle_seconds` for a known MCP package name,
/// or None for unknown packages. Exact match against the package identifier.
pub fn lookup(package: &str) -> Option<i32> {
    TABLE
        .iter()
        .find(|(name, _)| *name == package)
        .map(|(_, seconds)| *seconds)
}

/// Last-resort heuristic for the playwright / puppeteer / browser-use family —
/// matches if the binary path substring contains any of those names.
/// Spec §"Server lifecycle" auto-detection rule #5.
pub fn is_browser_pkg(binary_path_or_name: &str) -> bool {
    let lower = binary_path_or_name.to_lowercase();
    lower.contains("playwright") || lower.contains("puppeteer") || lower.contains("browser-use")
}

/// The complete static table, exposed so callers and tests can introspect it.
pub const fn all() -> &'static [(&'static str, i32)] {
    TABLE
}
