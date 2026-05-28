// OS keyring wrapper. Per-server env at `conduit/server/<server>/env/<uuid>`.
// OAuth tokens at `conduit-oauth/<account>:{access|refresh|client_secret}`.

use keyring_core::{Entry, Error as KeyringError};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum SecretsError {
    #[error("keyring backend error: {0}")]
    Backend(#[from] KeyringError),
    #[error("secret not found")]
    NotFound,
}

fn entry(server: &str, uuid: &str) -> Result<Entry, SecretsError> {
    let service = format!("conduit/server/{server}/env");
    Ok(Entry::new(&service, uuid)?)
}

/// Store a secret value in the OS keyring, returning a fresh UUID to persist
/// in `servers.env_json` under `value_or_ref`.
pub fn store(server: &str, value: &str) -> Result<String, SecretsError> {
    let uuid = Uuid::new_v4().to_string();
    let e = entry(server, &uuid)?;
    e.set_password(value)?;
    Ok(uuid)
}

/// Fetch a previously-stored secret by (server, uuid).
pub fn fetch(server: &str, uuid: &str) -> Result<String, SecretsError> {
    let e = entry(server, uuid)?;
    match e.get_password() {
        Ok(v) => Ok(v),
        Err(KeyringError::NoEntry) => Err(SecretsError::NotFound),
        Err(other) => Err(SecretsError::Backend(other)),
    }
}

/// Delete a secret. Idempotent: deleting a missing entry returns Ok.
pub fn delete(server: &str, uuid: &str) -> Result<(), SecretsError> {
    let e = entry(server, uuid)?;
    match e.delete_credential() {
        Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
        Err(other) => Err(SecretsError::Backend(other)),
    }
}

// ── OAuth token storage ───────────────────────────────────────────────────
// Stored under service "conduit-oauth" with account suffixes:
//   "<server>:access" / "<server>:refresh" / "<server>:client_secret"
// During the wizard's step 2b, tokens are written under a draft UUID; on Save
// they're promoted to the canonical server name and the draft entries deleted.

const OAUTH_SERVICE: &str = "conduit-oauth";

fn oauth_entry(account: &str) -> Result<Entry, SecretsError> {
    Ok(Entry::new(OAUTH_SERVICE, account)?)
}

pub fn store_oauth_access(account: &str, token: &str) -> Result<(), SecretsError> {
    oauth_entry(&format!("{account}:access"))?.set_password(token)?;
    Ok(())
}

pub fn store_oauth_refresh(account: &str, token: &str) -> Result<(), SecretsError> {
    oauth_entry(&format!("{account}:refresh"))?.set_password(token)?;
    Ok(())
}

pub fn store_oauth_client_secret(account: &str, secret: &str) -> Result<(), SecretsError> {
    oauth_entry(&format!("{account}:client_secret"))?.set_password(secret)?;
    Ok(())
}

pub fn fetch_oauth_client_secret(account: &str) -> Result<String, SecretsError> {
    fetch_keyring(&format!("{account}:client_secret"))
}

pub fn fetch_oauth_access(account: &str) -> Result<String, SecretsError> {
    fetch_keyring(&format!("{account}:access"))
}

pub fn fetch_oauth_refresh(account: &str) -> Result<String, SecretsError> {
    fetch_keyring(&format!("{account}:refresh"))
}

pub fn store_oauth_meta(account: &str, meta: &OauthMeta) -> Result<(), SecretsError> {
    let blob = serde_json::to_string(meta).unwrap_or_else(|_| "{}".into());
    oauth_entry(&format!("{account}:meta"))?.set_password(&blob)?;
    Ok(())
}

pub fn fetch_oauth_meta(account: &str) -> Result<OauthMeta, SecretsError> {
    let blob = fetch_keyring(&format!("{account}:meta"))?;
    serde_json::from_str(&blob).map_err(|_e| SecretsError::NotFound)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OauthMeta {
    pub token_endpoint: String,
    pub client_id: String,
    pub expires_at_unix: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
}

fn fetch_keyring(account: &str) -> Result<String, SecretsError> {
    match oauth_entry(account)?.get_password() {
        Ok(v) => Ok(v),
        Err(KeyringError::NoEntry) => Err(SecretsError::NotFound),
        Err(other) => Err(SecretsError::Backend(other)),
    }
}

/// Promote draft-namespaced OAuth entries to the canonical server-named ones.
///
/// Order: store new entries BEFORE deleting drafts so a crash mid-flight
/// leaves the canonical entries valid.
pub fn promote_oauth_tokens(draft: &str, server: &str) -> Result<(), SecretsError> {
    let access = fetch_oauth_access(draft).ok();
    let refresh = fetch_oauth_refresh(draft).ok();
    let client_secret = fetch_oauth_client_secret(draft).ok();
    let meta = fetch_oauth_meta(draft).ok();
    tracing::info!(
        draft = %draft,
        server = %server,
        has_access = access.is_some(),
        has_refresh = refresh.is_some(),
        has_client_secret = client_secret.is_some(),
        has_meta = meta.is_some(),
        "promote_oauth_tokens: read from draft",
    );
    // Store meta first — it's the most critical for refresh. If the process
    // crashes mid-promotion, having meta means the refresh path can work.
    if let Some(m) = &meta {
        store_oauth_meta(server, m)?;
    }
    if let Some(a) = &access {
        store_oauth_access(server, a)?;
    }
    if let Some(r) = &refresh {
        store_oauth_refresh(server, r)?;
    }
    if let Some(s) = &client_secret {
        store_oauth_client_secret(server, s)?;
    }
    delete_oauth_tokens(draft)?;
    tracing::info!(server = %server, "promote_oauth_tokens: completed");
    Ok(())
}

pub fn delete_oauth_tokens(account: &str) -> Result<(), SecretsError> {
    for suffix in ["access", "refresh", "client_secret", "meta"] {
        let full = format!("{account}:{suffix}");
        match oauth_entry(&full)?.delete_credential() {
            Ok(()) | Err(KeyringError::NoEntry) => {}
            Err(other) => return Err(SecretsError::Backend(other)),
        }
    }
    Ok(())
}
