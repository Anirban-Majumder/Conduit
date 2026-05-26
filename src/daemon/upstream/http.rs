// HTTP streamable-MCP transport. Thin wrapper around rmcp's
// `StreamableHttpClientTransport` + `serve_client_with_ct`.
// Spec §"Add MCP server flow".
//
// For v0 we support **bearer-token** auth: a `MCP_BEARER_TOKEN` entry in the
// server's env vars (plain or secret-keyring-backed) becomes the
// `Authorization: Bearer <token>` header on every request. Most pre-
// authenticated MCP HTTP servers (Linear, Atlassian, Notion) hand the user a
// static token to paste — this covers them.
//
// Full OAuth-with-browser-redirect (spec §"OAuth handling") lands in v0.1 —
// rmcp exposes `AuthorizationManager` + `AuthClient` for that, but the
// browser-redirect / local-callback-listener / token-refresh wiring is a
// dedicated work item.

use rmcp::service::{serve_client_with_ct, RoleClient, RunningService};
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};
use tokio_util::sync::CancellationToken;

use crate::daemon::upstream::manager::UpstreamError;

/// Env-var name the daemon recognises as a pre-authenticated bearer token.
pub const BEARER_TOKEN_ENV: &str = "MCP_BEARER_TOKEN";

pub type HttpClient = RunningService<RoleClient, ()>;

/// Connect to an HTTP MCP server and complete the `initialize` handshake.
///
/// `bearer_token` is wrapped into `Authorization: Bearer <token>` on every
/// outbound request (see [`BEARER_TOKEN_ENV`]). HTTPS support comes from
/// `Cargo.toml`'s `rmcp` `reqwest` feature, which flips on `reqwest/rustls`.
pub async fn connect(
    url: &str,
    bearer_token: Option<&str>,
    cancel: CancellationToken,
) -> Result<HttpClient, UpstreamError> {
    let mut cfg = StreamableHttpClientTransportConfig::with_uri(url.to_string());
    if let Some(token) = bearer_token {
        cfg = cfg.auth_header(format!("Bearer {token}"));
    }
    let transport = StreamableHttpClientTransport::from_config(cfg);
    let client = serve_client_with_ct((), transport, cancel)
        .await
        .map_err(|e| UpstreamError::InitFailed(e.to_string()))?;
    Ok(client)
}
