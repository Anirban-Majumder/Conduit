//! Probe an upstream MCP server without registering it with the runtime.

use std::sync::Arc;
use std::time::Duration;

use rmcp::ServiceError;
use serde_json::Value;
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

use crate::daemon::secrets;
use crate::daemon::upstream::http;
use crate::daemon::upstream::oauth;
use crate::daemon::upstream::stdio;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const TOOLS_LIST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeResult {
    pub server_name: String,
    pub server_version: String,
    pub tools: Vec<ToolMeta>,
    /// Set when the probe ran OAuth — draft session UUID for keyring promotion on Save.
    pub oauth_session: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolMeta {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,
}

#[derive(Debug, Error)]
pub enum ProbeError {
    #[error("connect failed: {0}")]
    Connect(String),
    #[error("initialize timed out after {0:?}")]
    InitTimeout(Duration),
    #[error("initialize rejected: {0}")]
    InitRejected(String),
    #[error("tools/list failed: {0}")]
    ToolsListFailed(String),
    #[error("authentication required but server didn't advertise OAuth metadata — add an Authorization header (e.g. \"Authorization=Bearer <token>\") and retry")]
    OauthRequired,
    #[error("oauth flow failed: {0}")]
    OauthFlow(String),
    #[error("cancelled by user")]
    Cancelled,
}

#[derive(Debug)]
pub struct DraftServerConfig {
    pub name: String,
    pub transport: DraftTransport,
    pub lifecycle_seconds: i32,
    pub draft_session: String,
}

#[derive(Debug)]
pub enum DraftTransport {
    Stdio {
        command: String,
        args: Vec<String>,
        env: Vec<DraftEnvEntry>,
    },
    Http {
        url: String,
        auth: HttpAuth,
        headers: Vec<(String, String)>,
    },
}

#[derive(Debug)]
pub struct DraftEnvEntry {
    pub name: String,
    pub value: Zeroizing<String>,
    pub is_secret: bool,
}

#[derive(Debug)]
pub enum HttpAuth {
    None,
    Bearer(Zeroizing<String>),
}

#[derive(Debug, Clone)]
pub enum ProbeEvent {
    Phase(String),
    AuthorizeUrl(String),
}

pub type ProgressSink = Arc<dyn Fn(ProbeEvent) + Send + Sync>;

pub async fn probe(
    cfg: &DraftServerConfig,
    cancel: CancellationToken,
) -> Result<ProbeResult, ProbeError> {
    probe_with_progress(cfg, cancel, None).await
}

#[expect(
    clippy::too_many_lines,
    reason = "linear probe flow (connect → initialize → tools/list) reads clearer inline"
)]
pub async fn probe_with_progress(
    cfg: &DraftServerConfig,
    cancel: CancellationToken,
    progress: Option<ProgressSink>,
) -> Result<ProbeResult, ProbeError> {
    let emit = |p: ProbeEvent| {
        if let Some(s) = progress.as_ref() {
            s(p);
        }
    };

    let mut oauth_session: Option<String> = None;

    let client = match &cfg.transport {
        DraftTransport::Stdio { command, args, env } => {
            emit(ProbeEvent::Phase("Spawning child process...".into()));
            let resolved_env: Vec<(String, String)> = env
                .iter()
                .map(|e| (e.name.clone(), e.value.as_str().to_string()))
                .collect();
            spawn_with_phase_timeout(
                stdio::connect(command, args, &resolved_env, cancel.clone()),
                cancel.clone(),
            )
            .await?
        }
        DraftTransport::Http { url, auth, headers } => {
            emit(ProbeEvent::Phase("Connecting + initializing...".into()));
            // First attempt with whatever auth the user provided.
            let bearer = bearer_for(auth, headers);
            let first = http::connect(url, bearer.as_deref(), cancel.clone()).await;
            match first {
                Ok(c) => spawn_done(c, cancel.clone()).await?,
                Err(e) => {
                    let needs_oauth = matches!(&auth, HttpAuth::None)
                        && bearer.is_none()
                        && looks_like_auth_failure(&e);
                    if !needs_oauth {
                        return Err(map_connect_error(e));
                    }
                    emit(ProbeEvent::Phase(
                        "Sign in required — starting OAuth...".into(),
                    ));
                    let draft = cfg.draft_session.clone();
                    run_oauth_login(url, &draft, cancel.clone(), &emit).await?;
                    oauth_session = Some(draft.clone());
                    // Retry via the runtime AuthClient path against the freshly
                    // stored draft credentials — same code path as a saved server.
                    emit(ProbeEvent::Phase(
                        "Re-initializing with OAuth token...".into(),
                    ));
                    match http::connect_oauth(url, &draft, cancel.clone())
                        .await
                        .map_err(map_connect_error)?
                    {
                        Some(c) => spawn_done(c, cancel.clone()).await?,
                        None => {
                            return Err(ProbeError::OauthFlow(
                                "credentials were not persisted after login".into(),
                            ))
                        }
                    }
                }
            }
        }
    };

    let peer_info = client.peer_info().ok_or_else(|| {
        ProbeError::InitRejected("server returned no peer_info after initialize".to_string())
    })?;
    let server_name = peer_info.server_info.name.clone();
    let server_version = peer_info.server_info.version.clone();

    emit(ProbeEvent::Phase("Fetching tools...".into()));
    let list_result = tokio::select! {
        biased;
        () = cancel.cancelled() => {
            let _ignored = client.cancel().await;
            return Err(ProbeError::Cancelled);
        }
        r = tokio::time::timeout(TOOLS_LIST_TIMEOUT, client.list_tools(None)) => match r {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                let _ignored = client.cancel().await;
                return Err(map_tools_error(e));
            }
            Err(_elapsed) => {
                let _ignored = client.cancel().await;
                return Err(ProbeError::ToolsListFailed(
                    format!("timed out after {TOOLS_LIST_TIMEOUT:?}")
                ));
            }
        },
    };

    let tools: Vec<ToolMeta> = list_result
        .tools
        .into_iter()
        .map(|t| ToolMeta {
            name: t.name.to_string(),
            description: t.description.map(|d| d.to_string()),
            input_schema: serde_json::to_value(&*t.input_schema).unwrap_or(Value::Null),
        })
        .collect();
    tracing::info!(
        server = %server_name,
        tool_count = tools.len(),
        tool_names = %tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(","),
        "probe: enumerated tools",
    );

    let _ignored = client.cancel().await;

    Ok(ProbeResult {
        server_name,
        server_version,
        tools,
        oauth_session,
    })
}

fn bearer_for(auth: &HttpAuth, headers: &[(String, String)]) -> Option<String> {
    if let HttpAuth::Bearer(t) = auth {
        // Strip a "Bearer " prefix the user may have pasted, mirroring the
        // header path below: rmcp's bearer_auth re-adds it, so passing it raw
        // would yield "Bearer Bearer <token>".
        let raw = t.as_str().strip_prefix("Bearer ").unwrap_or(t.as_str());
        return Some(raw.to_string());
    }
    headers.iter().find_map(|(k, v)| {
        if k.eq_ignore_ascii_case("authorization") {
            v.strip_prefix("Bearer ").map(str::to_string)
        } else {
            None
        }
    })
}

const AUTH_FAILURE_PATTERNS: &[&str] = &[
    "401",
    "403",
    "unauthorized",
    "forbidden",
    "auth required",
    "authentication required",
    "authentication failed",
    "not authenticated",
    "missing credentials",
    "invalid credentials",
    "api key",
    "invalid token",
    "invalid access token",
    "token expired",
    "expired token",
    "access denied",
];

pub fn message_looks_like_auth_failure(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    AUTH_FAILURE_PATTERNS
        .iter()
        .any(|needle| lower.contains(needle))
}

fn looks_like_auth_failure(e: &crate::daemon::upstream::manager::UpstreamError) -> bool {
    use crate::daemon::upstream::manager::UpstreamError;
    let s = match e {
        UpstreamError::InitFailed(m) => m.clone(),
        UpstreamError::Rmcp(r) => r.to_string(),
        _ => return false,
    };
    message_looks_like_auth_failure(&s)
}

async fn run_oauth_login(
    url: &str,
    draft: &str,
    cancel: CancellationToken,
    emit: &(dyn Fn(ProbeEvent) + Send + Sync),
) -> Result<(), ProbeError> {
    use rmcp::transport::auth::{OAuthState, StoredCredentials};

    let (listener, _port, redirect_uri) = oauth::bind_callback_listener()
        .await
        .map_err(|e| ProbeError::OauthFlow(e.to_string()))?;

    emit(ProbeEvent::Phase(
        "Resolving authorization server...".into(),
    ));
    // `None` ⇒ rmcp builds its own client for discovery + DCR + token exchange.
    let mut state = OAuthState::new(url, None).await.map_err(map_oauth_err)?;
    // start_authorization discovers metadata (PRM → direct, no endpoint
    // guessing) then dynamically registers a client. Servers without a
    // registration_endpoint fail here; rmcp's OAuthState / OAuthClientConfig
    // also supports a pre-registered client_id, but wiring that is a deliberate
    // deferral (documented in the README), not an oversight.
    state
        .start_authorization(&[], &redirect_uri, Some("Conduit"))
        .await
        .map_err(map_oauth_err)?;

    let authorize_url = state.get_authorization_url().await.map_err(map_oauth_err)?;
    emit(ProbeEvent::AuthorizeUrl(authorize_url.clone()));
    if let Err(open_err) = open::that(&authorize_url) {
        tracing::warn!(error = %open_err, "browser launch failed");
    }

    let (code, csrf) = oauth::wait_for_callback(listener, cancel)
        .await
        .map_err(|e| ProbeError::OauthFlow(e.to_string()))?;
    emit(ProbeEvent::Phase("Exchanging code for tokens...".into()));
    state
        .handle_callback(&code, &csrf)
        .await
        .map_err(map_oauth_err)?;

    let (client_id, token_response) = state.get_credentials().await.map_err(map_oauth_err)?;
    if token_response.is_none() {
        return Err(ProbeError::OauthFlow(
            "authorization completed but no token was returned".into(),
        ));
    }
    // rmcp re-requests the grant's scopes server-side on refresh, so an empty
    // scope list here is fine; the token was just minted, so "now" is its
    // receipt time for expiry tracking.
    let stored = StoredCredentials::new(
        client_id,
        token_response,
        Vec::new(),
        Some(now_epoch_secs()),
    );
    let draft = draft.to_string();
    tokio::task::spawn_blocking(move || secrets::store_oauth_cred(&draft, &stored))
        .await
        .map_err(|e| ProbeError::OauthFlow(e.to_string()))?
        .map_err(|e| ProbeError::OauthFlow(format!("store credentials: {e}")))?;
    Ok(())
}

fn map_oauth_err(e: rmcp::transport::auth::AuthError) -> ProbeError {
    use rmcp::transport::auth::AuthError;
    match e {
        AuthError::NoAuthorizationSupport => ProbeError::OauthRequired,
        AuthError::RegistrationFailed(msg) => ProbeError::OauthFlow(format!(
            "server does not support Dynamic Client Registration ({msg}); a \
             pre-registered client_id is not yet supported"
        )),
        other => ProbeError::OauthFlow(other.to_string()),
    }
}

fn now_epoch_secs() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

async fn spawn_with_phase_timeout<F, T>(fut: F, cancel: CancellationToken) -> Result<T, ProbeError>
where
    F: std::future::Future<Output = Result<T, crate::daemon::upstream::manager::UpstreamError>>,
{
    tokio::pin!(fut);
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(ProbeError::Cancelled),
        r = tokio::time::timeout(CONNECT_TIMEOUT, &mut fut) => match r {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(map_connect_error(e)),
            Err(_elapsed) => Err(ProbeError::InitTimeout(CONNECT_TIMEOUT)),
        },
    }
}

async fn spawn_done<T>(value: T, _cancel: CancellationToken) -> Result<T, ProbeError> {
    tokio::task::yield_now().await;
    Ok(value)
}

fn map_connect_error(e: crate::daemon::upstream::manager::UpstreamError) -> ProbeError {
    use crate::daemon::upstream::manager::UpstreamError;
    match e {
        UpstreamError::Io(io) => ProbeError::Connect(io.to_string()),
        UpstreamError::InitFailed(msg) => ProbeError::InitRejected(msg),
        UpstreamError::Rmcp(rmcp) => ProbeError::InitRejected(rmcp.to_string()),
        other => ProbeError::Connect(other.to_string()),
    }
}

fn map_tools_error(e: ServiceError) -> ProbeError {
    ProbeError::ToolsListFailed(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_for_strips_pasted_prefix() {
        let prefixed = HttpAuth::Bearer(Zeroizing::new("Bearer abc123".to_string()));
        assert_eq!(bearer_for(&prefixed, &[]).as_deref(), Some("abc123"));
        let raw = HttpAuth::Bearer(Zeroizing::new("xyz789".to_string()));
        assert_eq!(bearer_for(&raw, &[]).as_deref(), Some("xyz789"));
    }

    #[test]
    fn auth_failure_detector_matches_common_signals() {
        for s in [
            "HTTP 401",
            "401 Unauthorized",
            "Unauthorized",
            "Auth required, when send initialize request",
            "Authentication required",
            "API key invalid",
            "Invalid api key. Please check your API key.",
            "Forbidden",
            "403 Forbidden",
            "token expired",
            "Access Denied",
        ] {
            assert!(
                message_looks_like_auth_failure(s),
                "expected detector to match: {s}"
            );
        }
    }

    #[test]
    fn auth_failure_detector_ignores_unrelated_errors() {
        for s in [
            "connection refused",
            "EOF",
            "tools/list timed out",
            "JSON parse error at column 3",
        ] {
            assert!(
                !message_looks_like_auth_failure(s),
                "expected detector NOT to match: {s}"
            );
        }
    }
}
