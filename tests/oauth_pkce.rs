#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test ergonomics"
)]

use std::time::Duration;

use conduit::daemon::upstream::oauth::{bind_callback_listener, wait_for_callback, OauthError};
use tokio_util::sync::CancellationToken;

/// Live-network smoke test: confirms rmcp discovers Sentry's OAuth metadata
/// (PRM → authorization server) end-to-end. Ignored by default; run with
/// `cargo test -- --ignored`.
#[tokio::test]
#[ignore = "hits live network (mcp.sentry.dev)"]
async fn sentry_oauth_discovery_smoke() {
    use rmcp::transport::auth::AuthorizationManager;

    let manager = AuthorizationManager::new("https://mcp.sentry.dev/mcp")
        .await
        .expect("build manager");
    let meta = manager
        .discover_metadata()
        .await
        .expect("discover Sentry OAuth metadata");
    assert!(
        !meta.authorization_endpoint.is_empty(),
        "expected an authorization_endpoint"
    );
    assert!(!meta.token_endpoint.is_empty(), "expected a token_endpoint");
}

#[tokio::test]
async fn callback_returns_code_and_state() {
    let (listener, port, _redirect) = bind_callback_listener().await.unwrap();
    let cancel = CancellationToken::new();
    let handle = tokio::spawn(wait_for_callback(listener, cancel));

    let url = format!("http://127.0.0.1:{port}/callback?code=secret-code&state=the-state");
    let _ignored = tokio::time::timeout(
        Duration::from_secs(1),
        reqwest::Client::new().get(&url).send(),
    )
    .await;

    let (code, state) = tokio::time::timeout(Duration::from_secs(3), handle)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(code, "secret-code");
    assert_eq!(state, "the-state");
}

#[tokio::test]
async fn callback_without_code_returns_error() {
    let (listener, port, _redirect) = bind_callback_listener().await.unwrap();
    let cancel = CancellationToken::new();
    let handle = tokio::spawn(wait_for_callback(listener, cancel));

    let url = format!("http://127.0.0.1:{port}/callback?state=only-state");
    let _ignored = tokio::time::timeout(
        Duration::from_secs(1),
        reqwest::Client::new().get(&url).send(),
    )
    .await;

    let res = tokio::time::timeout(Duration::from_secs(3), handle)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(res, Err(OauthError::MissingCode)), "got {res:?}");
}

#[tokio::test]
async fn callback_cancel_returns_cancelled() {
    let (listener, _port, _redirect) = bind_callback_listener().await.unwrap();
    let cancel = CancellationToken::new();
    let handle = tokio::spawn(wait_for_callback(listener, cancel.clone()));
    tokio::time::sleep(Duration::from_millis(50)).await;
    cancel.cancel();
    let res = tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(res, Err(OauthError::Cancelled)));
}
