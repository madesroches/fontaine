//! OAuth token storage for streamable HTTP MCP servers: rmcp's `CredentialStore` and
//! `StateStore` traits over the `[mcp]` table of `credentials.toml`.
//!
//! Entries are keyed by the server name, matching `[mcp.servers]`, and each entry records the
//! resource URL its credentials were issued for. A declaration whose URL differs from the stored
//! one never receives the token, so a project-scope definition reusing a user-scope server's name
//! cannot ride on its login.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rmcp::transport::auth::{
    AuthError, CredentialRefreshGuard, CredentialStore, StateStore, StoredAuthorizationState,
    StoredCredentials,
};

use crate::config::ConfigError;
use crate::credentials;

/// The auth column of the `/mcp` listing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpAuth {
    /// A usable token is stored for this HTTP server.
    Ok,
    /// An HTTP server with no usable stored token: it fails until `fontaine mcp login <name>`.
    Required,
    /// Not an HTTP server: authentication does not apply.
    Na,
}

/// The resource URL the stored credentials for `name` were issued for, if any. `None` when no
/// entry is stored; a stored entry without one predates URL binding and is treated as unbound.
pub fn stored_resource(data_dir: &Path, name: &str) -> Result<Option<String>, ConfigError> {
    Ok(credentials::mcp_get(data_dir, name)?.and_then(|entry| entry.resource))
}

/// Whether a usable token exists for the MCP server `name`: the entry's credentials JSON parses
/// and carries a token response. A malformed entry is simply a missing login.
pub fn has_credentials(data_dir: &Path, name: &str) -> bool {
    let Ok(Some(entry)) = credentials::mcp_get(data_dir, name) else {
        return false;
    };
    let Some(json) = entry.credentials else {
        return false;
    };
    serde_json::from_str::<StoredCredentials>(&json)
        .ok()
        .and_then(|stored| stored.token_response)
        .is_some()
}

fn store_error(context: &str, error: ConfigError) -> AuthError {
    AuthError::CredentialStoreError(format!(
        "{context}: {}",
        fontaine_tools::error_chain::chain(&error)
    ))
}

/// Takes the file lock off the async threads: it blocks while another process holds it, and a
/// current-thread runtime probing several servers would otherwise stall behind a refresh.
async fn lock_blocking(data_dir: &Path) -> Result<credentials::Lock, AuthError> {
    let data_dir = data_dir.to_path_buf();
    tokio::task::spawn_blocking(move || credentials::lock(&data_dir))
        .await
        .map_err(|e| AuthError::CredentialStoreError(format!("lock task failed: {e}")))?
        .map_err(|e| store_error("cannot lock credentials.toml", e))
}

/// The lock coordination shared with the refresh guard.
type RefreshLock = Arc<Mutex<Option<credentials::Lock>>>;

/// File-backed `CredentialStore` for one MCP server. `resource` is the URL written into the
/// entry on save, so the binding follows the declaration the manager was built for.
pub struct FileCredentialStore {
    data_dir: PathBuf,
    name: String,
    resource: String,
    /// The lock the refresh guard holds across load → token request → save. While it is present,
    /// load and save operate under it instead of taking the lock per call, which is what keeps a
    /// concurrent `fontaine mcp login` and an agent-driven refresh from clobbering each other.
    refresh_lock: RefreshLock,
}

impl FileCredentialStore {
    pub fn new(data_dir: &Path, name: &str, resource: &str) -> FileCredentialStore {
        FileCredentialStore {
            data_dir: data_dir.to_path_buf(),
            name: name.to_string(),
            resource: resource.to_string(),
            refresh_lock: Arc::new(Mutex::new(None)),
        }
    }

    /// Runs `op` under the refresh guard's lock when one is held, else under a fresh lock.
    async fn locked<T>(
        &self,
        op: impl FnOnce(&credentials::Lock) -> Result<T, ConfigError> + Send,
    ) -> Result<T, AuthError> {
        {
            let guard = fontaine_tools::lock::lock(&self.refresh_lock, "MCP refresh lock");
            if let Some(held) = guard.as_ref() {
                return op(held).map_err(|e| store_error("credentials.toml", e));
            }
        }
        let lock = lock_blocking(&self.data_dir).await?;
        op(&lock).map_err(|e| store_error("credentials.toml", e))
    }

    #[cfg(test)]
    fn refresh_guard_held(&self) -> bool {
        fontaine_tools::lock::lock(&self.refresh_lock, "MCP refresh lock").is_some()
    }
}

#[async_trait::async_trait]
impl CredentialStore for FileCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        // Writers replace the file atomically, so a read needs no lock; this also means a load
        // under the refresh guard cannot double-acquire.
        let entry = credentials::mcp_get(&self.data_dir, &self.name)
            .map_err(|e| store_error("credentials.toml", e))?;
        let Some(json) = entry.and_then(|entry| entry.credentials) else {
            return Ok(None);
        };
        // A string this build cannot parse (written by a newer rmcp) is a missing login, not
        // a store failure: the server goes unauthenticated and the 401 names the login path.
        Ok(serde_json::from_str(&json).ok())
    }

    async fn save(&self, stored: StoredCredentials) -> Result<(), AuthError> {
        let json = serde_json::to_string(&stored)
            .map_err(|e| AuthError::CredentialStoreError(e.to_string()))?;
        self.locked(|lock| {
            let mut entry =
                credentials::mcp_get_locked(&self.data_dir, &self.name, lock)?.unwrap_or_default();
            entry.credentials = Some(json);
            entry.resource = Some(self.resource.clone());
            credentials::mcp_put_locked(&self.data_dir, &self.name, entry, lock)
        })
        .await
    }

    async fn clear(&self) -> Result<(), AuthError> {
        self.locked(|lock| {
            let Some(mut entry) = credentials::mcp_get_locked(&self.data_dir, &self.name, lock)?
            else {
                return Ok(());
            };
            entry.credentials = None;
            entry.resource = None;
            credentials::mcp_put_locked(&self.data_dir, &self.name, entry, lock)
        })
        .await
    }

    /// Wraps the file lock, which the manager holds across its whole load → token request →
    /// save sequence; load and save above see it held and do not reacquire.
    async fn acquire_refresh_guard(&self) -> Result<Option<CredentialRefreshGuard>, AuthError> {
        let lock = lock_blocking(&self.data_dir).await?;
        *fontaine_tools::lock::lock(&self.refresh_lock, "MCP refresh lock") = Some(lock);
        Ok(Some(CredentialRefreshGuard::new(ReleaseRefreshLock(
            self.refresh_lock.clone(),
        ))))
    }
}

/// Clears the store's refresh slot when the manager drops the guard, so a later load or save
/// takes the file lock per call again.
struct ReleaseRefreshLock(RefreshLock);

impl Drop for ReleaseRefreshLock {
    fn drop(&mut self) {
        *fontaine_tools::lock::lock(&self.0, "MCP refresh lock") = None;
    }
}

/// File-backed `StateStore` for one MCP server's PKCE flow state. The trait keys its calls by
/// csrf token; the entry holds one state, and the csrf token serialized inside it is what a load
/// or delete matches against.
pub struct FileStateStore {
    data_dir: PathBuf,
    name: String,
}

impl FileStateStore {
    pub fn new(data_dir: &Path, name: &str) -> FileStateStore {
        FileStateStore {
            data_dir: data_dir.to_path_buf(),
            name: name.to_string(),
        }
    }
}

#[async_trait::async_trait]
impl StateStore for FileStateStore {
    async fn save(
        &self,
        _csrf_token: &str,
        state: StoredAuthorizationState,
    ) -> Result<(), AuthError> {
        let json = serde_json::to_string(&state)
            .map_err(|e| AuthError::CredentialStoreError(e.to_string()))?;
        let lock = lock_blocking(&self.data_dir).await?;
        let mut entry = credentials::mcp_get_locked(&self.data_dir, &self.name, &lock)
            .map_err(|e| store_error("credentials.toml", e))?
            .unwrap_or_default();
        entry.state = Some(json);
        credentials::mcp_put_locked(&self.data_dir, &self.name, entry, &lock)
            .map_err(|e| store_error("credentials.toml", e))
    }

    async fn load(&self, csrf_token: &str) -> Result<Option<StoredAuthorizationState>, AuthError> {
        let lock = lock_blocking(&self.data_dir).await?;
        let entry = credentials::mcp_get_locked(&self.data_dir, &self.name, &lock)
            .map_err(|e| store_error("credentials.toml", e))?;
        let Some(json) = entry.and_then(|entry| entry.state) else {
            return Ok(None);
        };
        Ok(serde_json::from_str::<StoredAuthorizationState>(&json)
            .ok()
            .filter(|state| state.csrf_token == csrf_token))
    }

    async fn delete(&self, csrf_token: &str) -> Result<(), AuthError> {
        let lock = lock_blocking(&self.data_dir).await?;
        let Some(mut entry) = credentials::mcp_get_locked(&self.data_dir, &self.name, &lock)
            .map_err(|e| store_error("credentials.toml", e))?
        else {
            return Ok(());
        };
        let matches = entry
            .state
            .as_deref()
            .and_then(|json| serde_json::from_str::<StoredAuthorizationState>(json).ok())
            .is_some_and(|state| state.csrf_token == csrf_token);
        if matches {
            entry.state = None;
            credentials::mcp_put_locked(&self.data_dir, &self.name, entry, &lock)
                .map_err(|e| store_error("credentials.toml", e))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        (dir, path)
    }

    fn stored(client_id: &str) -> StoredCredentials {
        StoredCredentials::new(client_id.to_string(), None, Vec::new(), None)
    }

    #[tokio::test]
    async fn the_credential_store_round_trips_through_the_file() {
        let (_dir, dir) = world();
        let store = FileCredentialStore::new(&dir, "github", "https://h/mcp");
        assert!(store.load().await.unwrap().is_none());
        store.save(stored("client-1")).await.unwrap();
        let loaded = store.load().await.unwrap().expect("stored");
        assert_eq!(loaded.client_id, "client-1");

        let entry = credentials::mcp_get(&dir, "github").unwrap().unwrap();
        assert_eq!(entry.resource.as_deref(), Some("https://h/mcp"));
        assert!(!has_credentials(&dir, "github"), "no token response stored");
        assert!(stored_resource(&dir, "github").unwrap().is_some());

        store.clear().await.unwrap();
        assert!(store.load().await.unwrap().is_none());
        // Clearing drops the last field, and an empty entry goes with it.
        assert!(credentials::mcp_get(&dir, "github").unwrap().is_none());
    }

    #[tokio::test]
    async fn has_credentials_reads_the_token_response() {
        let (_dir, dir) = world();
        credentials::mcp_put(
            &dir,
            "github",
            credentials::McpEntry {
                credentials: Some(
                    r#"{"client_id":"c","token_response":{"access_token":"a","token_type":"Bearer"}}"#
                        .to_string(),
                ),
                state: None,
                resource: Some("https://h/mcp".to_string()),
            },
        )
        .unwrap();
        assert!(has_credentials(&dir, "github"));
        credentials::mcp_remove(&dir, "github").unwrap();
        assert!(!has_credentials(&dir, "github"));
    }

    #[tokio::test]
    async fn a_save_under_the_refresh_guard_does_not_reacquire_the_lock() {
        let (_dir, dir) = world();
        let store = FileCredentialStore::new(&dir, "github", "https://h/mcp");
        let guard = store
            .acquire_refresh_guard()
            .await
            .unwrap()
            .expect("a guard");
        assert!(store.refresh_guard_held());
        // A deadlock would hang the test: the guard holds the file lock and save must run under
        // it instead of taking the lock again.
        store.save(stored("client-1")).await.unwrap();
        drop(guard);
        assert!(!store.refresh_guard_held());
        assert_eq!(store.load().await.unwrap().unwrap().client_id, "client-1");
    }

    #[tokio::test]
    async fn a_malformed_credentials_string_is_a_missing_login_and_survives_a_save() {
        let (_dir, dir) = world();
        credentials::mcp_put(
            &dir,
            "github",
            credentials::McpEntry {
                credentials: Some("{not json".to_string()),
                state: None,
                resource: Some("https://old/mcp".to_string()),
            },
        )
        .unwrap();
        let store = FileCredentialStore::new(&dir, "github", "https://h/mcp");
        assert!(store.load().await.unwrap().is_none());
        assert!(!has_credentials(&dir, "github"));
        store.save(stored("client-1")).await.unwrap();
        let entry = credentials::mcp_get(&dir, "github").unwrap().unwrap();
        assert_eq!(entry.resource.as_deref(), Some("https://h/mcp"));
        assert!(entry.credentials.unwrap().contains("client-1"));
    }

    #[tokio::test]
    async fn the_state_store_keys_by_the_csrf_token_inside_the_entry() {
        let (_dir, dir) = world();
        let store = FileStateStore::new(&dir, "github");
        let state: StoredAuthorizationState =
            serde_json::from_str(r#"{"pkce_verifier":"v","csrf_token":"csrf-1","created_at":0}"#)
                .unwrap();
        store.save("csrf-1", state.clone()).await.unwrap();
        assert!(store.load("csrf-1").await.unwrap().is_some());
        assert!(store.load("csrf-other").await.unwrap().is_none());
        store.delete("csrf-other").await.unwrap();
        assert!(store.load("csrf-1").await.unwrap().is_some());
        store.delete("csrf-1").await.unwrap();
        assert!(store.load("csrf-1").await.unwrap().is_none());
    }

    #[test]
    fn the_url_binding_reads_the_stored_resource() {
        let (_dir, dir) = world();
        assert!(stored_resource(&dir, "github").unwrap().is_none());
        credentials::mcp_put(
            &dir,
            "github",
            credentials::McpEntry {
                credentials: None,
                state: None,
                resource: Some("https://h/mcp".to_string()),
            },
        )
        .unwrap();
        assert_eq!(
            stored_resource(&dir, "github").unwrap().as_deref(),
            Some("https://h/mcp")
        );
    }
}
