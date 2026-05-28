//! Keyring-backed `CredentialStore` for rmcp's OAuth runtime.
//!
//! rmcp loads the access token before every request and persists refreshed
//! tokens back through this store. `load` caches in memory so steady-state
//! requests don't hit the OS keyring; keyring I/O runs on `spawn_blocking`
//! because the keyring calls are synchronous and must not block the runtime
//! (thread-affinity contract).

use std::sync::RwLock;

use async_trait::async_trait;
use rmcp::transport::auth::{AuthError, CredentialStore, StoredCredentials};

use crate::daemon::secrets::{self, SecretsError};

#[derive(Debug)]
pub struct KeyringCredentialStore {
    account: String,
    cache: RwLock<Option<StoredCredentials>>,
}

impl KeyringCredentialStore {
    pub fn new(account: impl Into<String>) -> Self {
        Self {
            account: account.into(),
            cache: RwLock::new(None),
        }
    }
}

#[async_trait]
impl CredentialStore for KeyringCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        // A poisoned cache lock degrades to a keyring read, never a panic.
        if let Ok(guard) = self.cache.read() {
            if let Some(cred) = guard.as_ref() {
                return Ok(Some(cred.clone()));
            }
        }
        let account = self.account.clone();
        let loaded = tokio::task::spawn_blocking(move || secrets::fetch_oauth_cred(&account))
            .await
            .map_err(|e| AuthError::InternalError(e.to_string()))?;
        match loaded {
            Ok(cred) => {
                if let Ok(mut guard) = self.cache.write() {
                    *guard = Some(cred.clone());
                }
                Ok(Some(cred))
            }
            Err(SecretsError::NotFound) => Ok(None),
            // A corrupt/partial blob (e.g. an interrupted keyring write) is
            // unrecoverable but not fatal: treat it as "no credentials" so the
            // server falls through to NeedsAuth and re-login overwrites it,
            // rather than bricking the connect. Keyring I/O errors still
            // propagate — those are a real backend failure, not bad data.
            Err(e @ SecretsError::Serde(_)) => {
                tracing::warn!(
                    account = %self.account,
                    error = %e,
                    "stored OAuth credentials are unreadable; re-login required",
                );
                Ok(None)
            }
            Err(e) => Err(AuthError::InternalError(e.to_string())),
        }
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        if let Ok(mut guard) = self.cache.write() {
            *guard = Some(credentials.clone());
        }
        let account = self.account.clone();
        tokio::task::spawn_blocking(move || secrets::store_oauth_cred(&account, &credentials))
            .await
            .map_err(|e| AuthError::InternalError(e.to_string()))?
            .map_err(|e| AuthError::InternalError(e.to_string()))
    }

    async fn clear(&self) -> Result<(), AuthError> {
        if let Ok(mut guard) = self.cache.write() {
            *guard = None;
        }
        let account = self.account.clone();
        tokio::task::spawn_blocking(move || secrets::delete_oauth_cred(&account))
            .await
            .map_err(|e| AuthError::InternalError(e.to_string()))?
            .map_err(|e| AuthError::InternalError(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, reason = "test ergonomics")]

    use std::sync::Mutex;

    use keyring_core::{mock, set_default_store, unset_default_store};

    use super::*;

    // The keyring default store is a process-wide global; serialize the tests
    // that swap in the mock store so they don't clobber each other.
    static STORE_LOCK: Mutex<()> = Mutex::new(());

    fn sample_cred() -> StoredCredentials {
        serde_json::from_value(serde_json::json!({
            "client_id": "client-abc",
            "token_response": {
                "access_token": "tok-123",
                "token_type": "bearer",
                "expires_in": 3600,
                "refresh_token": "refresh-xyz",
                "scope": "read write"
            },
            "granted_scopes": ["read", "write"],
            "token_received_at": 1_700_000_000u64
        }))
        .expect("deserialize StoredCredentials")
    }

    fn run<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(f)
    }

    #[test]
    fn load_save_clear_round_trip() {
        let _guard = STORE_LOCK.lock().expect("lock");
        set_default_store(mock::Store::new().expect("mock store"));

        let store = KeyringCredentialStore::new("server-x");
        run(async {
            assert!(store.load().await.expect("load empty").is_none());

            let cred = sample_cred();
            store.save(cred.clone()).await.expect("save");
            let got = store.load().await.expect("load").expect("present");
            assert_eq!(
                serde_json::to_value(&got).unwrap(),
                serde_json::to_value(&cred).unwrap(),
                "stored blob must round-trip the full StoredCredentials shape"
            );

            store.clear().await.expect("clear");
            assert!(store.load().await.expect("load after clear").is_none());
        });

        unset_default_store();
    }

    #[test]
    fn cache_serves_after_keyring_entry_removed() {
        let _guard = STORE_LOCK.lock().expect("lock");
        set_default_store(mock::Store::new().expect("mock store"));

        let store = KeyringCredentialStore::new("server-y");
        run(async {
            store.save(sample_cred()).await.expect("save");
            // Delete the backing entry out from under the cache; a load must
            // still succeed from the in-memory copy.
            secrets::delete_oauth_cred("server-y").expect("delete");
            assert!(
                store.load().await.expect("load").is_some(),
                "cache should serve without touching the keyring"
            );
        });

        unset_default_store();
    }

    #[test]
    fn corrupt_blob_loads_as_none() {
        let _guard = STORE_LOCK.lock().expect("lock");
        set_default_store(mock::Store::new().expect("mock store"));

        // Write a non-JSON blob directly under the credential entry, simulating
        // an interrupted keyring write.
        keyring_core::Entry::new("conduit-oauth", "corrupt-srv:cred")
            .expect("entry")
            .set_password("{ not valid json")
            .expect("set garbage");

        let store = KeyringCredentialStore::new("corrupt-srv");
        run(async {
            assert!(
                store
                    .load()
                    .await
                    .expect("load must not error on a corrupt blob")
                    .is_none(),
                "a corrupt blob should degrade to None (re-login), not error"
            );
        });

        unset_default_store();
    }
}
