#![allow(clippy::unwrap_used, reason = "test ergonomics")]
#![allow(clippy::expect_used, reason = "test ergonomics")]
#![allow(clippy::indexing_slicing, reason = "test ergonomics")]

use mcp_router::daemon::paste_parser::{parse, ParseError};

// ── Format 1: full mcpServers wrapper ───────────────────────────────────────

#[test]
fn test_mcp_servers_full() {
    let input = r#"{
        "mcpServers": {
            "my-server": {
                "command": "node",
                "args": ["dist/index.js"],
                "env": { "API_KEY": "secret123" }
            }
        }
    }"#;
    let result = parse(input).unwrap();
    assert_eq!(result.name, Some("my-server".to_owned()));
    assert_eq!(result.command, Some("node".to_owned()));
    assert_eq!(result.args, vec!["dist/index.js"]);
    assert_eq!(result.env.len(), 1);
    assert_eq!(result.env[0].name, "API_KEY");
    assert_eq!(result.env[0].value, "secret123");
    assert!(result.env[0].is_secret);
}

#[test]
fn test_mcp_servers_picks_first() {
    let input = r#"{
        "mcpServers": {
            "alpha": { "command": "node", "args": [] },
            "beta":  { "command": "python", "args": [] }
        }
    }"#;
    let result = parse(input).unwrap();
    // Must pick the first key and set name from it.
    assert!(result.name.is_some());
    assert!(result.command.is_some());
}

// ── Format 2: JSON fragment without wrapper ──────────────────────────────────

#[test]
fn test_json_fragment() {
    let input = r#"{ "command": "uvx", "args": ["mcp-server-fetch"], "env": { "DEBUG": "1" } }"#;
    let result = parse(input).unwrap();
    assert_eq!(result.name, None);
    assert_eq!(result.command, Some("uvx".to_owned()));
    assert_eq!(result.args, vec!["mcp-server-fetch"]);
    assert_eq!(result.env[0].name, "DEBUG");
    assert_eq!(result.env[0].value, "1");
    assert!(!result.env[0].is_secret);
}

// ── Format 3: plain env-var JSON object ──────────────────────────────────────

#[test]
fn test_plain_env_json_object() {
    let input = r#"{ "API_KEY": "abc", "DEBUG": "1" }"#;
    let result = parse(input).unwrap();
    assert_eq!(result.command, None);
    assert_eq!(result.args, Vec::<String>::new());
    assert_eq!(result.env.len(), 2);
    let api_key = result.env.iter().find(|e| e.name == "API_KEY").unwrap();
    assert_eq!(api_key.value, "abc");
    assert!(api_key.is_secret);
}

// ── Format 4: dotenv-style lines ─────────────────────────────────────────────

#[test]
fn test_dotenv_format() {
    let input = "export API_KEY=abc\nDEBUG=\"1\"\n# a comment\n\nSECRET_TOKEN='xyz'";
    let result = parse(input).unwrap();
    assert_eq!(result.env.len(), 3);

    let api_key = result.env.iter().find(|e| e.name == "API_KEY").unwrap();
    assert_eq!(api_key.value, "abc");
    assert!(api_key.is_secret);

    let debug = result.env.iter().find(|e| e.name == "DEBUG").unwrap();
    assert_eq!(debug.value, "1");
    assert!(!debug.is_secret);

    let token = result.env.iter().find(|e| e.name == "SECRET_TOKEN").unwrap();
    assert_eq!(token.value, "xyz");
    assert!(token.is_secret);
}

#[test]
fn test_dotenv_export_prefix_stripped() {
    let input = "export MY_TOKEN=hello";
    let result = parse(input).unwrap();
    assert_eq!(result.env[0].name, "MY_TOKEN");
    assert_eq!(result.env[0].value, "hello");
    assert!(result.env[0].is_secret);
}

#[test]
fn test_dotenv_comments_and_blank_lines_skipped() {
    let input = "\n# This is a comment\n\nFOO=bar\n\n# another comment\nBAZ=qux\n";
    let result = parse(input).unwrap();
    assert_eq!(result.env.len(), 2);
    assert_eq!(result.env[0].name, "FOO");
    assert_eq!(result.env[1].name, "BAZ");
}

// ── Format 5: single KEY=value line ──────────────────────────────────────────

#[test]
fn test_single_key_value_line() {
    let input = "DATABASE_PASSWORD=hunter2";
    let result = parse(input).unwrap();
    assert_eq!(result.env.len(), 1);
    assert_eq!(result.env[0].name, "DATABASE_PASSWORD");
    assert_eq!(result.env[0].value, "hunter2");
    assert!(result.env[0].is_secret);
}

// ── Format 6: YAML-ish KEY: value lines ──────────────────────────────────────

#[test]
fn test_yaml_kv_format() {
    let input = "API_KEY: mykey\nDEBUG: false";
    let result = parse(input).unwrap();
    assert_eq!(result.env.len(), 2);
    let api_key = result.env.iter().find(|e| e.name == "API_KEY").unwrap();
    assert_eq!(api_key.value, "mykey");
    assert!(api_key.is_secret);
}

// ── Secret name heuristic ─────────────────────────────────────────────────────

#[test]
fn test_secret_heuristic_substrings() {
    let cases = [
        ("OPENAI_API_KEY", true),
        ("GITHUB_TOKEN", true),
        ("MY_SECRET", true),
        ("DB_PASSWORD", true),
        ("AWS_CREDENTIAL", true),
        ("AUTHORIZATION", true),
        ("BEARER_TOKEN", true),
        ("DEBUG", false),
        ("HOST", false),
        ("PORT", false),
    ];
    for (name, expected) in cases {
        let input = format!("{name}=value");
        let result = parse(&input).unwrap();
        assert_eq!(
            result.env[0].is_secret, expected,
            "is_secret mismatch for {name}"
        );
    }
}

#[test]
fn test_secret_heuristic_case_insensitive() {
    let input = r#"{ "api_key": "val" }"#;
    let result = parse(input).unwrap();
    assert!(result.env[0].is_secret);
}

// ── Quote stripping ───────────────────────────────────────────────────────────

#[test]
fn test_double_quotes_stripped() {
    let input = r#"FOO="bar baz""#;
    let result = parse(input).unwrap();
    assert_eq!(result.env[0].value, "bar baz");
}

#[test]
fn test_single_quotes_stripped() {
    let input = "FOO='bar baz'";
    let result = parse(input).unwrap();
    assert_eq!(result.env[0].value, "bar baz");
}

// ── Garbage input returns ParseError ─────────────────────────────────────────

#[test]
fn test_garbage_returns_parse_error() {
    let result = parse("this is not valid in any format at all !@#$%");
    assert!(
        matches!(result, Err(ParseError::NoMatch)),
        "expected NoMatch error"
    );
}

#[test]
fn test_empty_input_returns_parse_error() {
    let result = parse("   ");
    assert!(matches!(result, Err(ParseError::NoMatch)));
}
