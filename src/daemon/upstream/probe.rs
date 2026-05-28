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
    /// Set when the probe ran OAuth — `draft_session_uuid` for keyring promotion on Save.
    pub oauth_session: Option<String>,
    /// Set when OAuth ran — used to populate `servers.oauth_*`.
    pub oauth_authority: Option<String>,
    pub oauth_client_id: Option<String>,
    pub oauth_scopes: Option<String>,
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
    reason = "OAuth pivot inlined for one-shot flow"
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
    let mut oauth_authority: Option<String> = None;
    let mut oauth_client_id: Option<String> = None;
    let mut oauth_scopes: Option<String> = None;

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
                        "Sign in required — fetching OAuth metadata...".into(),
                    ));
                    let Some(challenge) = fetch_oauth_challenge(url, cancel.clone()).await else {
                        return Err(ProbeError::OauthRequired);
                    };
                    let (meta, callback_listener, redirect_uri, pkce, state, dcr) =
                        run_oauth_pre(url, &challenge, cancel.clone(), &emit).await?;
                    let scope_param = if meta.scopes_supported.is_empty() {
                        None
                    } else {
                        Some(meta.scopes_supported.join(" "))
                    };
                    // RFC 8707: Use resource from PRM/AS metadata, or fall back to MCP URL.
                    let resource_indicator = meta
                        .resource
                        .clone()
                        .or_else(|| challenge.resource.clone())
                        .unwrap_or_else(|| url.to_string());
                    tracing::info!(
                        scope_count = meta.scopes_supported.len(),
                        scopes = scope_param.as_deref().unwrap_or("<none>"),
                        resource = %resource_indicator,
                        "OAuth: requesting advertised scopes",
                    );
                    let authorize_url = oauth::build_authorize_url(
                        &meta.authorization_endpoint,
                        &dcr.client_id,
                        &redirect_uri,
                        &pkce.challenge,
                        state.as_str(),
                        scope_param.as_deref(),
                        Some(&resource_indicator),
                    );
                    emit(ProbeEvent::AuthorizeUrl(authorize_url.clone()));
                    if let Err(open_err) = open::that(&authorize_url) {
                        tracing::warn!(error = %open_err, "browser launch failed");
                    }
                    let code =
                        oauth::wait_for_callback(callback_listener, state.as_str(), cancel.clone())
                            .await
                            .map_err(|err| ProbeError::OauthFlow(err.to_string()))?;
                    emit(ProbeEvent::Phase("Exchanging code for tokens...".into()));
                    let tokens = oauth::exchange_code_for_tokens(
                        &reqwest::Client::new(),
                        &meta.token_endpoint,
                        &dcr.client_id,
                        dcr.client_secret.as_deref(),
                        &redirect_uri,
                        &code,
                        pkce.verifier.as_str(),
                        Some(&resource_indicator),
                    )
                    .await
                    .map_err(|err| ProbeError::OauthFlow(err.to_string()))?;
                    tracing::info!(
                        granted_scope = tokens.scope.as_deref().unwrap_or("<unspecified>"),
                        has_refresh_token = tokens.refresh_token.is_some(),
                        "OAuth: token grant received",
                    );
                    // Store tokens + meta under draft session.
                    let draft = cfg.draft_session.clone();
                    secrets::store_oauth_access(&draft, &tokens.access_token)
                        .map_err(|e| ProbeError::OauthFlow(format!("store access token: {e}")))?;
                    if let Some(rt) = &tokens.refresh_token {
                        secrets::store_oauth_refresh(&draft, rt).map_err(|e| {
                            ProbeError::OauthFlow(format!("store refresh token: {e}"))
                        })?;
                    }
                    if let Some(secret) = &dcr.client_secret {
                        secrets::store_oauth_client_secret(&draft, secret).map_err(|e| {
                            ProbeError::OauthFlow(format!("store client secret: {e}"))
                        })?;
                    }
                    let expires_at_unix = {
                        use std::time::{SystemTime, UNIX_EPOCH};
                        let now = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
                            .unwrap_or(0);
                        now + i64::try_from(tokens.expires_in.unwrap_or(3600)).unwrap_or(3600)
                    };
                    secrets::store_oauth_meta(
                        &draft,
                        &secrets::OauthMeta {
                            token_endpoint: meta.token_endpoint.clone(),
                            client_id: dcr.client_id.clone(),
                            expires_at_unix,
                            resource: meta.resource.clone(),
                        },
                    )
                    .map_err(|e| ProbeError::OauthFlow(format!("store oauth meta: {e}")))?;
                    oauth_session = Some(draft);
                    oauth_authority = Some(meta.authorization_endpoint.clone());
                    oauth_client_id = Some(dcr.client_id.clone());
                    oauth_scopes = scope_param;
                    // Retry rmcp with the freshly-minted bearer.
                    emit(ProbeEvent::Phase(
                        "Re-initializing with OAuth token...".into(),
                    ));
                    let retry =
                        http::connect(url, Some(&tokens.access_token), cancel.clone()).await;
                    spawn_done(retry.map_err(map_connect_error)?, cancel.clone()).await?
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
        oauth_authority,
        oauth_client_id,
        oauth_scopes,
    })
}

fn bearer_for(auth: &HttpAuth, headers: &[(String, String)]) -> Option<String> {
    if let HttpAuth::Bearer(t) = auth {
        return Some(t.as_str().to_string());
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

async fn fetch_oauth_challenge(
    url: &str,
    cancel: CancellationToken,
) -> Option<oauth::OauthChallenge> {
    let client = reqwest::Client::new();
    let req = tokio::select! {
        () = cancel.cancelled() => return None,
        r = client.get(url).send() => r.ok()?,
    };
    if req.status().as_u16() != 401 {
        return None;
    }
    let www = req.headers().get("www-authenticate")?.to_str().ok()?;
    oauth::parse_www_authenticate(www)
}

struct PreOauth {}

type OauthPre = (
    oauth::AuthorizationServerMetadata,
    tokio::net::TcpListener,
    String,
    oauth::Pkce,
    Zeroizing<String>,
    oauth::DcrResponse,
);

/// Check whether two URLs share the same origin (scheme + host + port).
fn is_same_origin(a: &str, b: &str) -> bool {
    let (Ok(a), Ok(b)) = (reqwest::Url::parse(a), reqwest::Url::parse(b)) else {
        return false;
    };
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}

fn resource_metadata_candidates(mcp_url: &str, challenge: &oauth::OauthChallenge) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(advertised) = challenge.resource_metadata_url.as_deref() {
        // RFC 9728 §3.1: resource_metadata URL MUST be on the same origin as
        // the protected resource. Reject cross-origin values to prevent SSRF.
        if is_same_origin(mcp_url, advertised) {
            out.push(advertised.to_string());
        } else {
            tracing::warn!(
                advertised = %advertised,
                mcp_url = %mcp_url,
                "ignoring cross-origin resource_metadata URL (RFC 9728 §3.1)"
            );
        }
    }
    if let Ok(parsed) = reqwest::Url::parse(mcp_url) {
        let origin = format!(
            "{}://{}",
            parsed.scheme(),
            parsed.host_str().unwrap_or_default()
        );
        let port_suffix = parsed.port().map(|p| format!(":{p}")).unwrap_or_default();
        let path = parsed.path().trim_start_matches('/').trim_end_matches('/');
        if !path.is_empty() {
            out.push(format!(
                "{origin}{port_suffix}/.well-known/oauth-protected-resource/{path}"
            ));
        }
        out.push(format!(
            "{origin}{port_suffix}/.well-known/oauth-protected-resource"
        ));
    }
    out.sort();
    out.dedup();
    out
}

async fn first_authorization_metadata(
    http: &reqwest::Client,
    candidates: &[String],
) -> Result<oauth::AuthorizationServerMetadata, ProbeError> {
    let mut last_err: Option<String> = None;
    for candidate in candidates {
        match oauth::fetch_authorization_server_metadata(http, candidate).await {
            Ok(meta) => return Ok(meta),
            Err(e) => last_err = Some(format!("{candidate}: {e}")),
        }
    }
    Err(ProbeError::OauthFlow(format!(
        "no OAuth protected-resource metadata found (tried {} candidates: {})",
        candidates.len(),
        last_err.unwrap_or_else(|| "none".into())
    )))
}

async fn run_oauth_pre(
    mcp_url: &str,
    challenge: &oauth::OauthChallenge,
    cancel: CancellationToken,
    emit: &(dyn Fn(ProbeEvent) + Send + Sync),
) -> Result<OauthPre, ProbeError> {
    let http = reqwest::Client::new();
    emit(ProbeEvent::Phase(
        "Resolving authorization server...".into(),
    ));

    // Strategy 1: Protected Resource Metadata → Authorization Server (RFC 9728 + RFC 8414).
    let resource_candidates = resource_metadata_candidates(mcp_url, challenge);
    let meta = match first_authorization_metadata(&http, &resource_candidates).await {
        Ok(m) => m,
        Err(prm_err) => {
            tracing::debug!(error = %prm_err, "PRM chain failed, trying direct well-known");
            // Strategy 2: Direct well-known on MCP origin (RFC 8414 + OpenID Connect).
            match oauth::fetch_auth_server_metadata_direct(&http, mcp_url).await {
                Ok(m) => m,
                Err(direct_err) => {
                    tracing::debug!(error = %direct_err, "direct well-known failed, trying fallback endpoints");
                    // Strategy 3: Generate fallback endpoints from the MCP URL.
                    oauth::generate_fallback_endpoints(mcp_url).ok_or_else(|| {
                        ProbeError::OauthFlow(format!(
                            "OAuth discovery failed (PRM: {prm_err}; direct: {direct_err})"
                        ))
                    })?
                }
            }
        }
    };
    let (listener, _port, redirect_uri) = oauth::bind_callback_listener()
        .await
        .map_err(|e| ProbeError::OauthFlow(e.to_string()))?;
    let pkce = oauth::Pkce::generate();
    let state = oauth::fresh_state();
    let dcr_scope = if meta.scopes_supported.is_empty() {
        None
    } else {
        Some(meta.scopes_supported.join(" "))
    };
    let dcr = if let Some(reg) = &meta.registration_endpoint {
        emit(ProbeEvent::Phase("Registering OAuth client...".into()));
        oauth::register_client(&http, reg, &redirect_uri, dcr_scope)
            .await
            .map_err(|e| ProbeError::OauthFlow(e.to_string()))?
    } else {
        return Err(ProbeError::OauthFlow(
            "server has no registration_endpoint and manual client_id is not yet supported".into(),
        ));
    };
    let _ = (PreOauth {}, cancel);
    Ok((meta, listener, redirect_uri, pkce, state, dcr))
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
