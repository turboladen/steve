//! File-based OAuth credential persistence.
//!
//! Stores [`StoredCredentials`] as JSON in a single file per MCP server,
//! enabling token reuse across application restarts.

use std::path::PathBuf;

use async_trait::async_trait;
use rmcp::transport::auth::{AuthError, CredentialStore, StoredCredentials};

/// Persists OAuth credentials to a JSON file on disk.
///
/// Each remote MCP server gets its own `FileCredentialStore` pointed at a
/// unique path under the Steve data directory.
pub struct FileCredentialStore {
    path: PathBuf,
}

impl FileCredentialStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

#[async_trait]
impl CredentialStore for FileCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        match tokio::fs::read(&self.path).await {
            Ok(bytes) => {
                let creds: StoredCredentials = serde_json::from_slice(&bytes).map_err(|e| {
                    AuthError::InternalError(format!(
                        "failed to parse credentials from {}: {e}",
                        self.path.display()
                    ))
                })?;
                Ok(Some(creds))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(AuthError::InternalError(format!(
                "failed to read credentials from {}: {e}",
                self.path.display()
            ))),
        }
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        // Ensure the parent directory exists.
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                AuthError::InternalError(format!(
                    "failed to create credential directory {}: {e}",
                    parent.display()
                ))
            })?;
        }

        let json = serde_json::to_vec_pretty(&credentials).map_err(|e| {
            AuthError::InternalError(format!("failed to serialize credentials: {e}"))
        })?;

        tokio::fs::write(&self.path, json).await.map_err(|e| {
            AuthError::InternalError(format!(
                "failed to write credentials to {}: {e}",
                self.path.display()
            ))
        })?;

        // Restrict file permissions to owner-only on Unix (credentials are sensitive).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            tokio::fs::set_permissions(&self.path, perms).await.map_err(|e| {
                AuthError::InternalError(format!(
                    "failed to set credential file permissions: {e}"
                ))
            })?;
        }

        Ok(())
    }

    async fn clear(&self) -> Result<(), AuthError> {
        match tokio::fs::remove_file(&self.path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(AuthError::InternalError(format!(
                "failed to remove credentials at {}: {e}",
                self.path.display()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal `StoredCredentials` for testing.
    fn test_credentials() -> StoredCredentials {
        StoredCredentials {
            client_id: "test-client-id".to_string(),
            token_response: None,
            granted_scopes: vec!["read".to_string(), "write".to_string()],
            token_received_at: Some(1700000000),
        }
    }

    #[tokio::test]
    async fn load_returns_none_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileCredentialStore::new(dir.path().join("nonexistent.json"));
        assert!(store.load().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn save_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileCredentialStore::new(dir.path().join("creds.json"));

        let creds = test_credentials();
        store.save(creds.clone()).await.unwrap();

        let loaded = store.load().await.unwrap().expect("should load saved credentials");
        assert_eq!(loaded.client_id, "test-client-id");
        assert_eq!(loaded.granted_scopes, vec!["read", "write"]);
        assert_eq!(loaded.token_received_at, Some(1700000000));
        assert!(loaded.token_response.is_none());
    }

    #[tokio::test]
    async fn clear_removes_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileCredentialStore::new(dir.path().join("creds.json"));

        store.save(test_credentials()).await.unwrap();
        assert!(store.load().await.unwrap().is_some());

        store.clear().await.unwrap();
        assert!(store.load().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn clear_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileCredentialStore::new(dir.path().join("nonexistent.json"));
        // Clearing a non-existent file should succeed.
        store.clear().await.unwrap();
    }

    #[tokio::test]
    async fn save_creates_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileCredentialStore::new(dir.path().join("nested/deep/creds.json"));

        store.save(test_credentials()).await.unwrap();
        let loaded = store.load().await.unwrap().expect("should load from nested path");
        assert_eq!(loaded.client_id, "test-client-id");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn save_sets_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let store = FileCredentialStore::new(dir.path().join("creds.json"));

        store.save(test_credentials()).await.unwrap();

        let metadata = std::fs::metadata(dir.path().join("creds.json")).unwrap();
        let mode = metadata.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "credential file should be owner-read/write only");
    }

    #[tokio::test]
    async fn load_returns_error_on_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.json");
        tokio::fs::write(&path, b"not valid json").await.unwrap();

        let store = FileCredentialStore::new(path);
        let result = store.load().await;
        assert!(result.is_err());
    }
}
