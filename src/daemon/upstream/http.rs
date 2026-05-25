// HTTP streamable-MCP transport. Thin wrapper around
// `rmcp::transport::StreamableHttpClientTransport` + `serve_client_with_ct`.
// Spec §"Add MCP server flow".
//
// OAuth wiring (spec §"OAuth handling") lands in a later stage — for v0
// scaffold, callers pass an already-authenticated URL.

use rmcp::service::{serve_client_with_ct, RoleClient, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use tokio_util::sync::CancellationToken;

use crate::daemon::upstream::manager::UpstreamError;

pub type HttpClient = RunningService<RoleClient, ()>;

/// Connect to an HTTP MCP server and complete the `initialize` handshake.
pub async fn connect(url: &str, cancel: CancellationToken) -> Result<HttpClient, UpstreamError> {
    let transport = StreamableHttpClientTransport::from_uri(url);
    let client = serve_client_with_ct((), transport, cancel)
        .await
        .map_err(|e| UpstreamError::InitFailed(e.to_string()))?;
    Ok(client)
}
