#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, reason = "test ergonomics")]

use std::time::Duration;

use conduit::daemon::upstream::oauth::{
    bind_callback_listener, parse_www_authenticate, wait_for_callback, OauthError,
    RefreshCoordinator,
};
use tokio_util::sync::CancellationToken;

#[test]
fn parses_www_authenticate_with_resource_metadata() {
    let challenge = parse_www_authenticate(
        r#"Bearer realm="linear", resource_metadata="https://auth.linear.app/.well-known/oauth-protected-resource""#,
    )
    .unwrap();
    assert_eq!(challenge.realm.as_deref(), Some("linear"));
    assert_eq!(
        challenge.resource_metadata_url.as_deref(),
        Some("https://auth.linear.app/.well-known/oauth-protected-resource")
    );
}

#[test]
fn non_bearer_returns_none() {
    assert!(parse_www_authenticate("Basic realm=admin").is_none());
}

#[tokio::test]
async fn callback_state_mismatch_returns_error() {
    let (listener, port, _redirect) = bind_callback_listener().await.unwrap();
    let cancel = CancellationToken::new();
    let handle = tokio::spawn(wait_for_callback(listener, "expected-state", cancel.clone()));

    // Hit the callback with a wrong state.
    let url = format!("http://127.0.0.1:{port}/callback?code=abc&state=wrong");
    let _ignored = tokio::time::timeout(
        Duration::from_secs(1),
        reqwest::Client::new().get(&url).send(),
    )
    .await;

    let res = tokio::time::timeout(Duration::from_secs(3), handle)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(res, Err(OauthError::StateMismatch)), "got {res:?}");
}

#[tokio::test]
async fn callback_with_correct_state_returns_code() {
    let (listener, port, _redirect) = bind_callback_listener().await.unwrap();
    let cancel = CancellationToken::new();
    let handle = tokio::spawn(wait_for_callback(listener, "the-state", cancel));

    let url = format!("http://127.0.0.1:{port}/callback?code=secret-code&state=the-state");
    let _ignored = tokio::time::timeout(
        Duration::from_secs(1),
        reqwest::Client::new().get(&url).send(),
    )
    .await;

    let res = tokio::time::timeout(Duration::from_secs(3), handle)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(res.unwrap(), "secret-code");
}

#[tokio::test]
async fn callback_cancel_returns_cancelled() {
    let (listener, _port, _redirect) = bind_callback_listener().await.unwrap();
    let cancel = CancellationToken::new();
    let handle = tokio::spawn(wait_for_callback(listener, "state", cancel.clone()));
    tokio::time::sleep(Duration::from_millis(50)).await;
    cancel.cancel();
    let res = tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(res, Err(OauthError::Cancelled)));
}

#[tokio::test]
async fn refresh_coordinator_serialises_concurrent_calls() {
    let coord = RefreshCoordinator::new();
    let first = coord.try_acquire("gmail").await;
    let second = coord.try_acquire("gmail").await;
    assert!(first.is_some());
    assert!(second.is_none(), "second concurrent acquire should be denied");

    // Resolve and release.
    if let Some(tx) = first {
        let _ignored = tx.send(Ok("token".into()));
    }
    coord.release("gmail").await;

    // After release, can acquire again.
    let third = coord.try_acquire("gmail").await;
    assert!(third.is_some());
}
