#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test ergonomics"
)]

use std::time::Duration;

use conduit::daemon::upstream::probe::{
    probe, DraftServerConfig, DraftTransport, HttpAuth, ProbeError,
};
use tokio_util::sync::CancellationToken;

fn stdio_cfg(name: &str, command: &str, args: Vec<String>) -> DraftServerConfig {
    DraftServerConfig {
        name: name.to_string(),
        transport: DraftTransport::Stdio {
            command: command.to_string(),
            args,
            env: vec![],
        },
        lifecycle_seconds: 300,
    }
}

#[tokio::test]
async fn nonexistent_binary_returns_connect_error() {
    let cfg = stdio_cfg("none", "/nonexistent/path/to/no/binary", vec![]);
    let err = probe(&cfg, CancellationToken::new())
        .await
        .expect_err("probe should fail");
    assert!(
        matches!(err, ProbeError::Connect(_)),
        "expected Connect, got {err:?}"
    );
}

#[tokio::test]
async fn already_cancelled_token_short_circuits() {
    let cfg = stdio_cfg("none", "/bin/sh", vec!["-c".into(), "sleep 60".into()]);
    let cancel = CancellationToken::new();
    cancel.cancel();
    let err = probe(&cfg, cancel)
        .await
        .expect_err("pre-cancelled probe should not succeed");
    assert!(
        matches!(err, ProbeError::Cancelled),
        "expected Cancelled, got {err:?}"
    );
}

#[tokio::test]
async fn cancel_mid_probe_returns_cancelled() {
    // Use a long-running command that never speaks MCP. Probe will be blocked
    // on initialize; cancelling mid-flight must return Cancelled.
    #[cfg(windows)]
    let cfg = stdio_cfg(
        "hang",
        "cmd",
        vec![
            "/c".into(),
            "ping".into(),
            "127.0.0.1".into(),
            "-n".into(),
            "31".into(),
        ],
    );
    #[cfg(not(windows))]
    let cfg = stdio_cfg("hang", "/bin/sh", vec!["-c".into(), "sleep 30".into()]);
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    let handle = tokio::spawn(async move { probe(&cfg, cancel_clone).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    cancel.cancel();
    let res = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("probe should return promptly after cancel")
        .expect("join");
    let err = res.expect_err("expected error");
    assert!(
        matches!(err, ProbeError::Cancelled),
        "expected Cancelled, got {err:?}"
    );
}

#[tokio::test]
async fn http_probe_against_unreachable_url_returns_connect_or_rejected() {
    // localhost port that nothing is bound to — TCP refused.
    let cfg = DraftServerConfig {
        name: "unreachable".to_string(),
        transport: DraftTransport::Http {
            // Reserved port unlikely to host an MCP server.
            url: "http://127.0.0.1:1/mcp".to_string(),
            auth: HttpAuth::None,
            headers: vec![],
        },
        lifecycle_seconds: 300,
    };
    let err = probe(&cfg, CancellationToken::new())
        .await
        .expect_err("unreachable HTTP should fail");
    assert!(
        matches!(err, ProbeError::Connect(_) | ProbeError::InitRejected(_)),
        "expected Connect or InitRejected, got {err:?}"
    );
}
