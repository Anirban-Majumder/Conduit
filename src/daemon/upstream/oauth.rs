//! OAuth: PKCE (RFC 7636 S256) + DCR (RFC 7591) + callback + refresh.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, Mutex};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

// 64 bytes → 86-char base64url verifier (RFC 7636 mandates 43–128).
const VERIFIER_BYTES: usize = 64;
const STATE_BYTES: usize = 32;

#[derive(Debug)]
pub struct Pkce {
    pub verifier: Zeroizing<String>,
    pub challenge: String,
}

impl Pkce {
    /// Fresh PKCE pair (verifier + S256 challenge).
    pub fn generate() -> Self {
        let mut bytes = [0u8; VERIFIER_BYTES];
        rand::rng().fill_bytes(&mut bytes);
        let verifier = URL_SAFE_NO_PAD.encode(bytes);

        let mut hasher = Sha256::new();
        hasher.update(verifier.as_bytes());
        let digest = hasher.finalize();
        let challenge = URL_SAFE_NO_PAD.encode(digest);

        Self {
            verifier: Zeroizing::new(verifier),
            challenge,
        }
    }

    #[must_use]
    pub const fn method() -> &'static str {
        "S256"
    }
}

/// Fresh state nonce (32 random bytes, base64url). Bound to one flow.
#[must_use]
pub fn fresh_state() -> Zeroizing<String> {
    let mut bytes = [0u8; STATE_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    Zeroizing::new(URL_SAFE_NO_PAD.encode(bytes))
}

/// Byte-for-byte state comparison.
#[must_use]
pub fn state_matches(expected: &str, received: &str) -> bool {
    expected == received
}

/// Build the authorize URL with PKCE + state + scope + (optional) resource params.
///
/// Matches the param set emitted by `modelcontextprotocol/typescript-sdk`'s
/// `startAuthorization`: only RFC 8707 `resource`, no Auth0-native `audience`.
/// Sending both has been observed to cause Cloudflare's OAuth provider to bind
/// the token's `aud` claim differently than the resource server validates.
#[must_use]
pub fn build_authorize_url(
    authorization_endpoint: &str,
    client_id: &str,
    redirect_uri: &str,
    challenge: &str,
    state: &str,
    scopes: Option<&str>,
    resource: Option<&str>,
) -> String {
    let mut params: Vec<(&str, &str)> = vec![
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
    ];
    if let Some(scope) = scopes {
        params.push(("scope", scope));
    }
    if let Some(r) = resource {
        params.push(("resource", r));
    }

    let mut query = String::new();
    for (i, (k, v)) in params.into_iter().enumerate() {
        if i > 0 {
            query.push('&');
        }
        query.push_str(k);
        query.push('=');
        push_url_encoded(&mut query, v);
    }

    let sep = if authorization_endpoint.contains('?') {
        '&'
    } else {
        '?'
    };
    format!("{authorization_endpoint}{sep}{query}")
}

fn push_url_encoded(out: &mut String, s: &str) {
    use std::fmt::Write as _;
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char);
            }
            _ => {
                let _ignored = write!(out, "%{b:02X}");
            }
        }
    }
}

/// Dynamic client registration request body.
///
/// Field set mirrors `modelcontextprotocol/typescript-sdk` (the reference MCP
/// OAuth client). Some auth servers (Auth0, Cloudflare workers-oauth-provider)
/// register clients with empty allowed scopes when `scope` is omitted here,
/// which then silently downgrades the authorize-time scope grant.
#[derive(Debug, Clone, Serialize)]
pub struct DcrRequest<'a> {
    pub client_name: &'a str,
    pub client_uri: &'a str,
    pub redirect_uris: Vec<String>,
    pub grant_types: Vec<&'a str>,
    pub response_types: Vec<&'a str>,
    pub token_endpoint_auth_method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

impl DcrRequest<'_> {
    #[must_use]
    pub fn public_pkce_client(redirect_uri: String, scope: Option<String>) -> Self {
        Self {
            client_name: "MCP Router",
            client_uri: "https://github.com/anthropics/mcp-router",
            redirect_uris: vec![redirect_uri],
            grant_types: vec!["authorization_code", "refresh_token"],
            response_types: vec!["code"],
            token_endpoint_auth_method: "none",
            scope,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct DcrResponse {
    pub client_id: String,
    #[serde(default)]
    pub client_secret: Option<String>,
}

/// Successful token-endpoint response.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub token_type: Option<String>,
    /// Granted scopes per RFC 6749 §5.1. Differs from the requested set
    /// when the authorization server narrows the grant.
    #[serde(default)]
    pub scope: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OauthChallenge {
    pub realm: Option<String>,
    pub resource_metadata_url: Option<String>,
}

/// Parse a `WWW-Authenticate: Bearer ...` header.
#[must_use]
pub fn parse_www_authenticate(value: &str) -> Option<OauthChallenge> {
    let lower = value.trim().to_ascii_lowercase();
    if !lower.starts_with("bearer") {
        return None;
    }
    let mut realm = None;
    let mut resource_metadata_url = None;
    for raw in value
        .trim_start_matches(|c: char| c.is_ascii_alphabetic())
        .split(',')
    {
        let raw = raw.trim();
        let Some((k, v)) = raw.split_once('=') else {
            continue;
        };
        let k = k.trim().to_ascii_lowercase();
        let v = v.trim().trim_matches('"');
        if k == "realm" {
            realm = Some(v.to_string());
        } else if k == "resource_metadata" || k == "resource" {
            resource_metadata_url = Some(v.to_string());
        }
    }
    Some(OauthChallenge {
        realm,
        resource_metadata_url,
    })
}

#[derive(Debug, Error)]
pub enum OauthError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("discovery doc has no authorization_endpoint")]
    MissingAuthorizationEndpoint,
    #[error("discovery doc has no token_endpoint")]
    MissingTokenEndpoint,
    #[error("dcr rejected: {0}")]
    DcrRejected(String),
    #[error("token endpoint rejected: {0}")]
    TokenRejected(String),
    #[error("state CSRF mismatch")]
    StateMismatch,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("listener never received callback within {0:?}")]
    CallbackTimeout(Duration),
    #[error("cancelled by user")]
    Cancelled,
    #[error("browser launch failed: {0}")]
    BrowserLaunch(String),
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthorizationServerMetadata {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    #[serde(default)]
    pub registration_endpoint: Option<String>,
    #[serde(default)]
    pub scopes_supported: Vec<String>,
    /// The protected resource identifier (RFC 9728 `resource`). Passed as
    /// `audience` + `resource` on the authorize URL. Auth0-backed servers
    /// (Atlassian, Okta) silently narrow tokens to identity scopes without it.
    #[serde(default)]
    pub resource: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProtectedResourceMetadata {
    #[serde(default)]
    authorization_servers: Vec<String>,
    #[serde(default)]
    scopes_supported: Vec<String>,
    #[serde(default)]
    resource: Option<String>,
}

/// Resolve the protected-resource doc to the chosen `AuthorizationServerMetadata`.
pub async fn fetch_authorization_server_metadata(
    http: &reqwest::Client,
    resource_metadata_url: &str,
) -> Result<AuthorizationServerMetadata, OauthError> {
    let prm: ProtectedResourceMetadata = http
        .get(resource_metadata_url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let prm_scopes = prm.scopes_supported;
    let prm_resource = prm.resource;
    let authority = prm
        .authorization_servers
        .into_iter()
        .next()
        .ok_or(OauthError::MissingAuthorizationEndpoint)?;
    let well_known = if authority.ends_with('/') {
        format!("{authority}.well-known/oauth-authorization-server")
    } else {
        format!("{authority}/.well-known/oauth-authorization-server")
    };
    let mut asm: AuthorizationServerMetadata = http
        .get(&well_known)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    // RFC 9728: protected-resource metadata is authoritative for which scopes
    // grant access to this specific resource. Fall back to the auth server's
    // global list only if the resource doesn't advertise its own.
    if !prm_scopes.is_empty() {
        asm.scopes_supported = prm_scopes;
    }
    if asm.resource.is_none() {
        asm.resource = prm_resource;
    }
    Ok(asm)
}

/// POST the DCR body and return the response. Missing `client_secret` → public PKCE.
pub async fn register_client(
    http: &reqwest::Client,
    registration_endpoint: &str,
    redirect_uri: &str,
    scope: Option<String>,
) -> Result<DcrResponse, OauthError> {
    let body = DcrRequest::public_pkce_client(redirect_uri.to_string(), scope);
    let resp = http.post(registration_endpoint).json(&body).send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let txt = resp.text().await.unwrap_or_default();
        return Err(OauthError::DcrRejected(format!("{status}: {txt}")));
    }
    Ok(resp.json::<DcrResponse>().await?)
}

/// Bind `127.0.0.1:0`; returns `(listener, port, redirect_uri)`.
pub async fn bind_callback_listener() -> Result<(TcpListener, u16, String), OauthError> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");
    Ok((listener, port, redirect_uri))
}

/// Accept one redirect, validate state, reply with HTML. 5-minute hard cap.
pub async fn wait_for_callback(
    listener: TcpListener,
    expected_state: &str,
    cancel: CancellationToken,
) -> Result<String, OauthError> {
    let deadline = Duration::from_secs(300);
    let accepted = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(OauthError::Cancelled),
        r = tokio::time::timeout(deadline, listener.accept()) => {
            r.map_err(|_elapsed| OauthError::CallbackTimeout(deadline))?
        }
    };
    let (mut stream, _addr) = accepted?;
    let (rd, mut wr) = stream.split();
    let mut reader = BufReader::new(rd);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).await?;
    let query = parse_callback_query(&request_line).unwrap_or_default();
    let state_received = query.get("state").cloned().unwrap_or_default();
    let code = query.get("code").cloned().unwrap_or_default();

    let (status, body) = if state_received != expected_state {
        (
            "400 Bad Request",
            "<h1>Sign-in failed</h1><p>Security check failed (state mismatch). You can close this tab.</p>",
        )
    } else if code.is_empty() {
        (
            "400 Bad Request",
            "<h1>Sign-in failed</h1><p>No authorization code returned. You can close this tab.</p>",
        )
    } else {
        (
            "200 OK",
            "<h1>Signed in</h1><p>You can close this tab and return to Conduit.</p>",
        )
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    wr.write_all(response.as_bytes()).await.ok();
    wr.flush().await.ok();

    if state_received != expected_state {
        return Err(OauthError::StateMismatch);
    }
    if code.is_empty() {
        return Err(OauthError::TokenRejected("no code in callback".into()));
    }
    Ok(code)
}

fn parse_callback_query(request_line: &str) -> Option<HashMap<String, String>> {
    let mut parts = request_line.split_whitespace();
    let _method = parts.next()?;
    let path = parts.next()?;
    let query = path.split_once('?')?.1;
    let mut out = HashMap::new();
    for kv in query.split('&') {
        let Some((k, v)) = kv.split_once('=') else {
            continue;
        };
        out.insert(url_decode(k), url_decode(v));
    }
    Some(out)
}

fn url_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut iter = s.bytes();
    while let Some(b) = iter.next() {
        match b {
            b'+' => out.push(' '),
            b'%' => {
                let hi = iter.next().and_then(|c| (c as char).to_digit(16));
                let lo = iter.next().and_then(|c| (c as char).to_digit(16));
                if let (Some(h), Some(l)) = (hi, lo) {
                    let combined = h * 16 + l;
                    let byte = u8::try_from(combined).unwrap_or(b'?');
                    out.push(char::from(byte));
                } else {
                    out.push(b as char);
                }
            }
            other => out.push(other as char),
        }
    }
    out
}

/// POST `code` + `code_verifier` to the token endpoint.
pub async fn exchange_code_for_tokens(
    http: &reqwest::Client,
    token_endpoint: &str,
    client_id: &str,
    client_secret: Option<&str>,
    redirect_uri: &str,
    code: &str,
    code_verifier: &str,
    resource: Option<&str>,
) -> Result<TokenResponse, OauthError> {
    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", client_id),
        ("code_verifier", code_verifier),
    ];
    if client_secret.is_some() {
        // Basic-auth header set below; don't also pass client_secret in the form.
        form.retain(|(k, _)| *k != "client_id");
    }
    // RFC 8707: if the client passed `resource` to the authorize endpoint, it
    // must also pass it to the token endpoint, or the issued token may not be
    // bound to the target resource. Sentry rejects unbound tokens; Auth0 doesn't.
    if let Some(r) = resource {
        form.push(("resource", r));
    }
    let mut req = http
        .post(token_endpoint)
        .header(reqwest::header::ACCEPT, "application/json")
        .form(&form);
    if let Some(secret) = client_secret {
        req = req.basic_auth(client_id, Some(secret));
    }
    let resp = req.send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let txt = resp.text().await.unwrap_or_default();
        return Err(OauthError::TokenRejected(format!("{status}: {txt}")));
    }
    Ok(resp.json::<TokenResponse>().await?)
}

/// Refresh-token exchange.
pub async fn refresh_tokens(
    http: &reqwest::Client,
    token_endpoint: &str,
    client_id: &str,
    client_secret: Option<&str>,
    refresh_token: &str,
    resource: Option<&str>,
) -> Result<TokenResponse, OauthError> {
    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
    ];
    if client_secret.is_none() {
        form.push(("client_id", client_id));
    }
    if let Some(r) = resource {
        form.push(("resource", r));
    }
    let mut req = http
        .post(token_endpoint)
        .header(reqwest::header::ACCEPT, "application/json")
        .form(&form);
    if let Some(secret) = client_secret {
        req = req.basic_auth(client_id, Some(secret));
    }
    let resp = req.send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let txt = resp.text().await.unwrap_or_default();
        return Err(OauthError::TokenRejected(format!("{status}: {txt}")));
    }
    Ok(resp.json::<TokenResponse>().await?)
}

type RefreshSender = broadcast::Sender<Result<String, String>>;

#[derive(Default)]
pub struct RefreshCoordinator {
    inflight: Mutex<HashMap<String, RefreshSender>>,
}

impl std::fmt::Debug for RefreshCoordinator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RefreshCoordinator").finish_non_exhaustive()
    }
}

impl RefreshCoordinator {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Some → caller owns the refresh; None → follower, call `wait_for(server)`.
    pub async fn try_acquire(&self, server: &str) -> Option<RefreshSender> {
        let mut map = self.inflight.lock().await;
        if map.contains_key(server) {
            return None;
        }
        let (tx, _rx) = broadcast::channel(1);
        let stored = tx.clone();
        map.insert(server.to_string(), stored);
        drop(map);
        Some(tx)
    }

    /// Subscribe to the current in-flight refresh for `server`. Returns Err
    /// if no refresh is currently in flight.
    pub async fn wait_for(
        &self,
        server: &str,
    ) -> Option<broadcast::Receiver<Result<String, String>>> {
        let map = self.inflight.lock().await;
        map.get(server).map(broadcast::Sender::subscribe)
    }

    /// Signal that the in-flight refresh for `server` is finished; drops the
    /// sender so the entry can be re-acquired.
    pub async fn release(&self, server: &str) {
        let mut map = self.inflight.lock().await;
        map.remove(server);
    }
}

/// Drop draft-session keyring entries left over from abandoned wizards.
pub fn purge_draft_oauth_tokens(known_drafts: &[String]) -> usize {
    let mut count = 0;
    for d in known_drafts {
        if uuid::Uuid::parse_str(d).is_ok()
            && crate::daemon::secrets::delete_oauth_tokens(d).is_ok()
        {
            count += 1;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifier_is_86_chars_base64url() {
        let p = Pkce::generate();
        assert_eq!(p.verifier.len(), 86);
        // base64url alphabet: A-Z a-z 0-9 - _
        for c in p.verifier.chars() {
            assert!(
                c.is_ascii_alphanumeric() || c == '-' || c == '_',
                "got '{c}'"
            );
        }
    }

    #[test]
    fn challenge_is_sha256_of_verifier_base64url() {
        let p = Pkce::generate();
        let mut hasher = Sha256::new();
        hasher.update(p.verifier.as_bytes());
        let expected = URL_SAFE_NO_PAD.encode(hasher.finalize());
        assert_eq!(p.challenge, expected);
        assert_eq!(Pkce::method(), "S256");
    }

    #[test]
    fn two_generated_pkce_pairs_differ() {
        let a = Pkce::generate();
        let b = Pkce::generate();
        assert_ne!(a.verifier.as_str(), b.verifier.as_str());
        assert_ne!(a.challenge, b.challenge);
    }

    #[test]
    fn state_matches_is_strict() {
        let s = fresh_state();
        assert!(state_matches(s.as_str(), s.as_str()));
        assert!(!state_matches(s.as_str(), "garbage"));
        assert!(!state_matches(s.as_str(), ""));
    }

    #[test]
    fn authorize_url_has_all_required_params() {
        let url = build_authorize_url(
            "https://auth.example.com/authorize",
            "client_abc",
            "http://127.0.0.1:5000/callback",
            "CHAL",
            "STATE",
            Some("read write"),
            None,
        );
        assert!(url.starts_with("https://auth.example.com/authorize?"));
        for needle in [
            "response_type=code",
            "client_id=client_abc",
            "code_challenge=CHAL",
            "code_challenge_method=S256",
            "state=STATE",
            "scope=read%20write",
        ] {
            assert!(url.contains(needle), "missing {needle} in {url}");
        }
        assert!(!url.contains("resource="));
        assert!(!url.contains("audience="));
    }

    #[test]
    fn authorize_url_emits_only_resource_no_audience() {
        let url = build_authorize_url(
            "https://auth.example.com/authorize",
            "cid",
            "http://127.0.0.1:5000/callback",
            "C",
            "S",
            None,
            Some("https://mcp.example.com/v1/mcp"),
        );
        assert!(url.contains("resource=https%3A%2F%2Fmcp.example.com%2Fv1%2Fmcp"));
        assert!(
            !url.contains("audience="),
            "audience must NOT be emitted — matches TS SDK; sending both can break Cloudflare workers-oauth-provider"
        );
    }

    #[test]
    fn authorize_url_preserves_existing_query() {
        let url = build_authorize_url(
            "https://auth.example.com/authorize?tenant=mcp",
            "cid",
            "http://127.0.0.1:5000/callback",
            "C",
            "S",
            None,
            None,
        );
        assert!(url.contains("tenant=mcp&"));
        assert!(url.contains("&client_id=cid"));
    }

    #[test]
    fn dcr_public_pkce_client_matches_sdk_shape() {
        let req = DcrRequest::public_pkce_client(
            "http://127.0.0.1:5000/callback".to_string(),
            Some("read write".to_string()),
        );
        assert_eq!(req.client_name, "MCP Router");
        assert_eq!(req.token_endpoint_auth_method, "none");
        assert!(req.grant_types.contains(&"authorization_code"));
        assert!(req.grant_types.contains(&"refresh_token"));
        assert!(req.response_types.contains(&"code"));
        assert_eq!(req.scope.as_deref(), Some("read write"));
        let body = serde_json::to_string(&req).unwrap();
        assert!(body.contains("\"client_uri\""));
        assert!(body.contains("\"response_types\""));
        assert!(body.contains("\"scope\":\"read write\""));
    }

    #[test]
    fn dcr_public_pkce_client_omits_scope_when_none() {
        let req = DcrRequest::public_pkce_client(
            "http://127.0.0.1:5000/callback".to_string(),
            None,
        );
        let body = serde_json::to_string(&req).unwrap();
        assert!(
            !body.contains("\"scope\""),
            "scope field must be omitted entirely (not serialized as null) when None",
        );
    }

    #[test]
    fn token_response_parses_minimal() {
        let body = r#"{"access_token":"abc","token_type":"bearer"}"#;
        let parsed: TokenResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.access_token, "abc");
        assert!(parsed.refresh_token.is_none());
    }

    #[test]
    fn authorization_server_metadata_parses_scopes_supported() {
        let body = r#"{
            "authorization_endpoint": "https://auth.example.com/authorize",
            "token_endpoint": "https://auth.example.com/token",
            "scopes_supported": ["read:jira-work", "write:jira-work", "offline_access"]
        }"#;
        let parsed: AuthorizationServerMetadata = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.scopes_supported.len(), 3);
        assert!(parsed.scopes_supported.iter().any(|s| s == "offline_access"));
    }

    #[test]
    fn authorization_server_metadata_scopes_default_empty() {
        let body = r#"{
            "authorization_endpoint": "https://auth.example.com/authorize",
            "token_endpoint": "https://auth.example.com/token"
        }"#;
        let parsed: AuthorizationServerMetadata = serde_json::from_str(body).unwrap();
        assert!(parsed.scopes_supported.is_empty());
    }

    #[test]
    fn protected_resource_metadata_parses_scopes_supported() {
        let body = r#"{
            "authorization_servers": ["https://auth.example.com/tenant"],
            "scopes_supported": ["read:me", "read:jira-work"]
        }"#;
        let parsed: ProtectedResourceMetadata = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.scopes_supported, vec!["read:me", "read:jira-work"]);
        assert_eq!(parsed.authorization_servers.len(), 1);
    }

    #[test]
    fn token_response_parses_with_refresh_and_expiry() {
        let body = r#"{"access_token":"a","refresh_token":"r","expires_in":3600}"#;
        let parsed: TokenResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.access_token, "a");
        assert_eq!(parsed.refresh_token.as_deref(), Some("r"));
        assert_eq!(parsed.expires_in, Some(3600));
    }
}
