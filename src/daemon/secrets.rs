// OS keyring wrapper. Per-server env at `conduit/server/<server>/env/<uuid>`.
// OAuth credentials at `conduit-oauth/<account>:cred` (one JSON blob).

use keyring_core::{Entry, Error as KeyringError};
use rmcp::transport::auth::StoredCredentials;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum SecretsError {
    #[error("keyring backend error: {0}")]
    Backend(#[from] KeyringError),
    #[error("secret not found")]
    NotFound,
    #[error("credential serialization error: {0}")]
    Serde(#[from] serde_json::Error),
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

// ── OAuth credential storage ───────────────────────────────────────────────
// One JSON blob per account under service "conduit-oauth", account
// "<id>:cred", holding rmcp's `StoredCredentials` (client_id + token_response +
// granted scopes). During the wizard the blob is written under a draft UUID; on
// Save it is promoted to the canonical server name and the draft entry deleted.

const OAUTH_SERVICE: &str = "conduit-oauth";

fn oauth_entry(account: &str) -> Result<Entry, SecretsError> {
    Ok(Entry::new(OAUTH_SERVICE, account)?)
}

fn cred_account(account: &str) -> String {
    format!("{account}:cred")
}

pub fn store_oauth_cred(account: &str, cred: &StoredCredentials) -> Result<(), SecretsError> {
    let blob = serde_json::to_string(cred)?;
    oauth_entry(&cred_account(account))?.set_password(&blob)?;
    Ok(())
}

pub fn fetch_oauth_cred(account: &str) -> Result<StoredCredentials, SecretsError> {
    match oauth_entry(&cred_account(account))?.get_password() {
        Ok(blob) => Ok(serde_json::from_str(&blob)?),
        Err(KeyringError::NoEntry) => Err(SecretsError::NotFound),
        Err(other) => Err(SecretsError::Backend(other)),
    }
}

/// Delete the credential blob. Idempotent. This is what rmcp's
/// `CredentialStore::clear` maps onto — it must not touch legacy entries.
pub fn delete_oauth_cred(account: &str) -> Result<(), SecretsError> {
    delete_entry(&cred_account(account))
}

/// Copy the credential blob from a draft account to the canonical server name,
/// then delete the draft. Store-before-delete so a crash mid-flight leaves the
/// canonical entry valid.
pub fn promote_oauth_tokens(draft: &str, server: &str) -> Result<(), SecretsError> {
    match fetch_oauth_cred(draft) {
        Ok(cred) => store_oauth_cred(server, &cred)?,
        // Reaching here with no draft blob means login persisted creds but they
        // vanished before Save (e.g. crash + restart between the two). The save
        // proceeds without OAuth and the server re-prompts via NeedsAuth — but
        // it's an anomaly, so surface it rather than swallowing silently.
        Err(SecretsError::NotFound) => {
            tracing::warn!(
                draft = %draft,
                server = %server,
                "promote_oauth_tokens: draft credentials missing — server will require re-auth",
            );
        }
        Err(e) => return Err(e),
    }
    delete_oauth_cred(draft)
}

/// Delete every OAuth keyring entry for an account: the credential blob plus any
/// stale pre-migration entries. Used on server removal and draft purge.
pub fn delete_oauth_tokens(account: &str) -> Result<(), SecretsError> {
    delete_oauth_cred(account)?;
    delete_legacy_oauth_entries(account)
}

/// One-time cleanup of the pre-migration 4-entry scheme
/// (`:access` / `:refresh` / `:client_secret` / `:meta`). Idempotent.
pub fn delete_legacy_oauth_entries(account: &str) -> Result<(), SecretsError> {
    for suffix in ["access", "refresh", "client_secret", "meta"] {
        delete_entry(&format!("{account}:{suffix}"))?;
    }
    Ok(())
}

fn delete_entry(account: &str) -> Result<(), SecretsError> {
    match oauth_entry(account)?.delete_credential() {
        Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
        Err(other) => Err(SecretsError::Backend(other)),
    }
}
