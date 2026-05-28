use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rmcp::service::{serve_client_with_ct, RoleClient, RunningService};
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};
use tokio_util::sync::CancellationToken;

use crate::daemon::secrets::{
    fetch_oauth_access, fetch_oauth_client_secret, fetch_oauth_meta, fetch_oauth_refresh,
    store_oauth_access, store_oauth_meta, store_oauth_refresh, OauthMeta,
};
use crate::daemon::upstream::manager::UpstreamError;
use crate::daemon::upstream::oauth::{refresh_tokens, RefreshCoordinator};

pub const BEARER_TOKEN_ENV: &str = "MCP_BEARER_TOKEN";
const REFRESH_WINDOW_SECS: i64 = 60;

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

/// Return a current access token for `server`, refreshing if near expiry.
///
/// `None` means no OAuth tokens are stored — caller should use the static
/// bearer-token path instead.
pub async fn ensure_fresh_access_token(
    server: &str,
    coord: &Arc<RefreshCoordinator>,
    http: &reqwest::Client,
) -> Option<String> {
    let Ok(access) = fetch_oauth_access(server) else {
        tracing::warn!(server = %server, "ensure_fresh_access_token: no access token in keyring");
        return None;
    };
    let Ok(meta) = fetch_oauth_meta(server) else {
        tracing::info!(
            server = %server,
            access_len = access.len(),
            "ensure_fresh_access_token: have access token, no meta (refresh disabled)",
        );
        return Some(access);
    };
    let now = now_unix();
    if meta.expires_at_unix - now > REFRESH_WINDOW_SECS {
        tracing::debug!(
            server = %server,
            ttl_secs = meta.expires_at_unix - now,
            "ensure_fresh_access_token: token still fresh",
        );
        return Some(access);
    }
    let Ok(refresh) = fetch_oauth_refresh(server) else {
        return Some(access);
    };
    refresh_with_coordinator(server, coord, http, &meta, &refresh).await
}

async fn refresh_with_coordinator(
    server: &str,
    coord: &Arc<RefreshCoordinator>,
    http: &reqwest::Client,
    meta: &OauthMeta,
    refresh: &str,
) -> Option<String> {
    if let Some(tx) = coord.try_acquire(server).await {
        let client_secret = fetch_oauth_client_secret(server).ok();
        let result = refresh_tokens(
            http,
            &meta.token_endpoint,
            &meta.client_id,
            client_secret.as_deref(),
            refresh,
            meta.resource.as_deref(),
        )
        .await;
        let outcome = match result {
            Ok(token_resp) => {
                let new_access = token_resp.access_token.clone();
                if let Err(e) = store_oauth_access(server, &new_access) {
                    tracing::warn!(error = %e, server = %server, "store access failed");
                }
                if let Some(r) = token_resp.refresh_token {
                    if let Err(e) = store_oauth_refresh(server, &r) {
                        tracing::warn!(error = %e, server = %server, "store refresh failed");
                    }
                }
                let new_meta = OauthMeta {
                    token_endpoint: meta.token_endpoint.clone(),
                    client_id: meta.client_id.clone(),
                    expires_at_unix: now_unix()
                        + i64::try_from(token_resp.expires_in.unwrap_or(3600)).unwrap_or(3600),
                    resource: meta.resource.clone(),
                };
                if let Err(e) = store_oauth_meta(server, &new_meta) {
                    tracing::warn!(error = %e, server = %server, "store meta failed");
                }
                Ok(new_access)
            }
            Err(e) => Err(e.to_string()),
        };
        tx.send(outcome.clone()).ok();
        coord.release(server).await;
        outcome.ok()
    } else if let Some(mut rx) = coord.wait_for(server).await {
        match rx.recv().await {
            Ok(Ok(token)) => Some(token),
            _ => None,
        }
    } else {
        fetch_oauth_access(server).ok()
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
