#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test ergonomics")]

use mcp_router::daemon::upstream::allow_list;

#[test]
fn all_known_packages_return_correct_value() {
    let cases: &[(&str, i32)] = &[
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

    for (package, expected) in cases {
        assert_eq!(
            allow_list::lookup(package),
            Some(*expected),
            "wrong value for {package}"
        );
    }
}

#[test]
fn unknown_package_returns_none() {
    assert_eq!(allow_list::lookup("some-unknown-mcp-server"), None);
    assert_eq!(allow_list::lookup(""), None);
    assert_eq!(allow_list::lookup("@modelcontextprotocol/server-unknown"), None);
}

#[test]
fn is_browser_pkg_true_cases() {
    assert!(allow_list::is_browser_pkg("playwright"));
    assert!(allow_list::is_browser_pkg("puppeteer"));
    assert!(allow_list::is_browser_pkg("browser-use"));
    assert!(allow_list::is_browser_pkg("/usr/local/bin/playwright-binary"));
    assert!(allow_list::is_browser_pkg("MCP-Browser-Use"));
}

#[test]
fn is_browser_pkg_false_cases() {
    assert!(!allow_list::is_browser_pkg("@modelcontextprotocol/server-filesystem"));
    assert!(!allow_list::is_browser_pkg("mcp-server-time"));
    assert!(!allow_list::is_browser_pkg("exa-mcp"));
}

#[test]
fn all_returns_exactly_30_entries() {
    assert_eq!(allow_list::all().len(), 30);
}
