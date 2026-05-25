// OS keyring wrapper for upstream MCP server secret env vars + OAuth tokens.
// Spec §Storage > Secrets and §Add MCP server flow > Secrets handling.
// Keyed at `mcp-router/server/<server>/env/<uuid>`.

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
    let service = format!("mcp-router/server/{server}/env");
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
