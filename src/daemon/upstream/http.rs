//! Streamable-HTTP transport for HTTP MCP upstreams.
//!
//! `connect` handles static-bearer / no-auth servers; `connect_oauth` drives
//! the rmcp `AuthClient` path against credentials in `KeyringCredentialStore`,
//! which auto-refreshes tokens. A keyring miss or read failure deliberately
//! falls through to the non-OAuth path so a broken keyring can't brick plain
//! HTTP servers.

use rmcp::service::{serve_client_with_ct, RoleClient, RunningService};
use rmcp::transport::auth::{AuthClient, AuthorizationManager, CredentialStore};
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};
use tokio_util::sync::CancellationToken;

use crate::daemon::upstream::manager::UpstreamError;
use crate::daemon::upstream::oauth_store::KeyringCredentialStore;

pub const BEARER_TOKEN_ENV: &str = "MCP_BEARER_TOKEN";

pub type HttpClient = RunningService<RoleClient, ()>;

pub async fn connect(
    url: &str,
    bearer_token: Option<&str>,
    cancel: CancellationToken,
) -> Result<HttpClient, UpstreamError> {
    let mut cfg = StreamableHttpClientTransportConfig::with_uri(url.to_string());
    if let Some(token) = bearer_token {
        // rmcp applies this via reqwest `bearer_auth`, which prepends `Bearer `
        // itself. Passing a pre-prefixed value yields `Bearer Bearer <token>`,
        // which servers reject as invalid_token. Pass the raw token.
        cfg = cfg.auth_header(token);
    }
    let transport = StreamableHttpClientTransport::from_config(cfg);
    let client = serve_client_with_ct((), transport, cancel)
        .await
        .map_err(|e| UpstreamError::InitFailed(e.to_string()))?;
    Ok(client)
}

/// Connect an OAuth upstream from stored credentials.
///
/// Returns `Ok(None)` when no credentials exist for `server` (caller falls back
/// to static-bearer / no-auth). rmcp's `AuthClient` attaches the access token
/// to every request and refreshes transparently, persisting through
/// `KeyringCredentialStore`.
pub async fn connect_oauth(
    url: &str,
    server: &str,
    cancel: CancellationToken,
) -> Result<Option<HttpClient>, UpstreamError> {
    // Decide OAuth-ness from the keyring first, and treat *both* a miss and a
    // read failure as "not OAuth here" → fall through to the static-bearer /
    // no-auth path. A broken/absent keyring (e.g. no gnome-keyring on Linux)
    // must not brick plain HTTP servers — matches pre-migration behavior, where
    // a keyring miss or error simply meant "no token". Loading here also warms
    // the store cache so the manager's load below is a cache hit, not a second
    // keyring round-trip.
    let store = KeyringCredentialStore::new(server);
    match store.load().await {
        Ok(Some(creds)) if creds.token_response.is_some() => {}
        Ok(_) => return Ok(None),
        Err(e) => {
            tracing::warn!(server = %server, error = %e, "OAuth credential read failed; treating as non-OAuth");
            return Ok(None);
        }
    }
    // Credentials exist → genuine OAuth server; setup failures now propagate
    // (rather than masquerading as a non-OAuth fall-through).
    let mut manager = AuthorizationManager::new(url)
        .await
        .map_err(|e| UpstreamError::InitFailed(format!("oauth manager: {e}")))?;
    manager.set_credential_store(store);
    if !manager
        .initialize_from_store()
        .await
        .map_err(|e| UpstreamError::InitFailed(format!("oauth init: {e}")))?
    {
        return Ok(None);
    }
    // Mirror rmcp's default transport client: disabling idle pooling avoids the
    // Linux Delayed-ACK stall on reused connections.
    let http = reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .build()
        .map_err(|e| UpstreamError::InitFailed(format!("oauth http client: {e}")))?;
    let auth_client = AuthClient::new(http, manager);
    let cfg = StreamableHttpClientTransportConfig::with_uri(url.to_string());
    let transport = StreamableHttpClientTransport::with_client(auth_client, cfg);
    let client = serve_client_with_ct((), transport, cancel)
        .await
        .map_err(|e| UpstreamError::InitFailed(e.to_string()))?;
    Ok(Some(client))
}
