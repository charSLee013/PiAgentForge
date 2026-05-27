//! Auth storage — persistent credential storage for API keys and OAuth tokens.
//!
//! Mirrors `packages/coding-agent/src/core/auth-storage.ts`
//!
//! Credentials are stored in `~/.pi/auth.json` as a JSON object keyed by
//! provider name.  Each value is either an `api_key` or `oauth` entry,
//! differentiated by the `"type"` field.
//!
//! File locking via `fs2` prevents race conditions when multiple pi instances
//! try to read or write credentials concurrently.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;

use fs2::FileExt;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors that can occur during auth-storage operations.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// Underlying filesystem error (also covers lock failures).
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    /// JSON serialisation / deserialisation error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

// ---------------------------------------------------------------------------
// CredentialEntry (the per-provider value)
// ---------------------------------------------------------------------------

/// A stored credential for a single provider.
///
/// Serde internally-tagged representation matches the TS auth-storage format:
///
/// ```json
/// { "openai": { "type": "api_key", "key": "sk-…" } }
/// { "anthropic": { "type": "oauth", "access_token": "…", … } }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CredentialEntry {
    /// A static API key.
    #[serde(rename = "api_key")]
    ApiKey {
        /// The API key value.
        key: String,
    },
    /// OAuth credentials (access token with optional refresh support).
    #[serde(rename = "oauth")]
    OAuth {
        /// The OAuth access token.
        access_token: String,
        /// Refresh token, if the provider supports token refresh.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        refresh_token: Option<String>,
        /// Unix-timestamp (seconds) after which the access token expires.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expires_at: Option<i64>,
        /// Token type (e.g. "Bearer").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token_type: Option<String>,
        /// Provider-specific account identifier (e.g. OpenAI Codex account ID).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account_id: Option<String>,
    },
}

impl CredentialEntry {
    /// Return the effective API key — the `key` field for `ApiKey`, the
    /// `access_token` for `OAuth`.
    pub fn api_key(&self) -> &str {
        match self {
            CredentialEntry::ApiKey { key } => key.as_str(),
            CredentialEntry::OAuth { access_token, .. } => access_token.as_str(),
        }
    }
}

// ---------------------------------------------------------------------------
// AuthStorage
// ---------------------------------------------------------------------------

/// Credential storage backed by `~/.pi/auth.json`.
///
/// Uses `fs2` advisory file locking for safe concurrent access.
#[derive(Debug)]
pub struct AuthStorage {
    /// Path to the JSON file.
    path: PathBuf,
    /// In-memory credential map, keyed by provider name.
    credentials: HashMap<String, CredentialEntry>,
}

impl AuthStorage {
    /// Default path: `~/.pi/auth.json`.
    fn default_path() -> PathBuf {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        home.join(".pi").join("auth.json")
    }

    /// Load credentials from the default auth file (`~/.pi/auth.json`).
    ///
    /// If the file does not exist, returns an empty storage (no error).
    /// The file is read under a shared lock so other readers are not blocked.
    pub fn load() -> Result<Self, AuthError> {
        Self::load_from(Self::default_path())
    }

    /// Load credentials from an explicit path (useful for tests).
    pub fn load_from(path: PathBuf) -> Result<Self, AuthError> {
        match fs::File::open(&path) {
            Ok(file) => {
                fs2::FileExt::lock_shared(&file)?;
                let mut buf = String::new();
                (&file).read_to_string(&mut buf)?;
                // Lock released when `file` is dropped.
                let credentials: HashMap<String, CredentialEntry> =
                    serde_json::from_str(&buf)?;
                Ok(Self { path, credentials })
            }
            Err(_) => Ok(Self {
                path,
                credentials: HashMap::new(),
            }),
        }
    }

    /// Save credentials to the auth file under an exclusive lock.
    ///
    /// Creates the parent directory (`~/.pi`) if it does not exist.
    /// On Unix, sets file permissions to `0o600`.
    pub fn save(&self) -> Result<(), AuthError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&self.credentials)?;
        let file = fs::File::create(&self.path)?;
        file.lock_exclusive()?;
        (&file).write_all(json.as_bytes())?;
        file.sync_all()?;

        // Restrict permissions on Unix.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))?;
        }

        Ok(())
    }

    /// Get the stored API key for `provider`.
    ///
    /// Returns the `key` field for `ApiKey` entries or `access_token` for
    /// `OAuth` entries.
    pub fn get_api_key(&self, provider: &str) -> Option<String> {
        self.credentials.get(provider).map(|e| e.api_key().to_string())
    }

    /// Set a static API key for `provider`.
    pub fn set_api_key(&mut self, provider: &str, key: &str) {
        self.credentials.insert(
            provider.to_string(),
            CredentialEntry::ApiKey {
                key: key.to_string(),
            },
        );
    }

    /// Set OAuth credentials for `provider`.
    ///
    /// Accepts the individual fields so callers do not need to depend on
    /// the `pi-oauth` crate just to construct an entry.
    pub fn set_oauth(
        &mut self,
        provider: &str,
        access_token: &str,
        refresh_token: Option<String>,
        expires_at: Option<i64>,
        token_type: &str,
        account_id: Option<String>,
    ) {
        self.credentials.insert(
            provider.to_string(),
            CredentialEntry::OAuth {
                access_token: access_token.to_string(),
                refresh_token,
                expires_at,
                token_type: Some(token_type.to_string()),
                account_id,
            },
        );
    }

    /// Remove the credential entry for `provider`.
    pub fn remove(&mut self, provider: &str) {
        self.credentials.remove(provider);
    }

    /// Check whether `provider` has stored credentials.
    pub fn has(&self, provider: &str) -> bool {
        self.credentials.contains_key(provider)
    }

    /// List all providers that have stored credentials.
    pub fn providers(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.credentials.keys().cloned().collect();
        keys.sort();
        keys
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Create a temporary directory and return it plus an auth.json path inside.
    fn scratch_path() -> (TempDir, PathBuf) {
        let dir = TempDir::new().expect("tempdir creation failed");
        let path = dir.path().join("auth.json");
        (dir, path)
    }

    // ── Business logic (no file I/O) ───────────────────────────────────

    #[test]
    fn test_set_and_get_api_key_entry() {
        let (_td, path) = scratch_path();
        let mut storage = AuthStorage {
            path,
            credentials: HashMap::new(),
        };

        storage.set_api_key("openai", "sk-test-123");
        assert_eq!(storage.get_api_key("openai"), Some("sk-test-123".into()));
        assert!(storage.has("openai"));
    }

    #[test]
    fn test_remove_credential() {
        let (_td, path) = scratch_path();
        let mut storage = AuthStorage {
            path,
            credentials: HashMap::new(),
        };

        storage.set_api_key("openai", "sk-test");
        assert!(storage.has("openai"));
        storage.remove("openai");
        assert!(!storage.has("openai"));
    }

    #[test]
    fn test_providers_list_is_sorted() {
        let (_td, path) = scratch_path();
        let mut storage = AuthStorage {
            path,
            credentials: HashMap::new(),
        };

        storage.set_api_key("z-provider", "z-key");
        storage.set_api_key("a-provider", "a-key");
        storage.set_api_key("m-provider", "m-key");

        let providers = storage.providers();
        assert_eq!(providers, vec!["a-provider", "m-provider", "z-provider"]);
    }

    #[test]
    fn test_oauth_entry_api_key_returns_access_token() {
        let (_td, path) = scratch_path();
        let mut storage = AuthStorage {
            path,
            credentials: HashMap::new(),
        };

        storage.set_oauth("anthropic", "ant-access", Some("ant-refresh".into()), Some(9999999999), "Bearer", None);
        assert_eq!(storage.get_api_key("anthropic"), Some("ant-access".into()));
    }

    #[test]
    fn test_get_api_key_nonexistent_provider() {
        let (_td, path) = scratch_path();
        let storage = AuthStorage {
            path,
            credentials: HashMap::new(),
        };
        assert_eq!(storage.get_api_key("nonexistent"), None);
    }

    // ── File persistence tests ─────────────────────────────────────────

    #[test]
    fn test_save_writes_correct_json() {
        let (_td, path) = scratch_path();
        let mut storage = AuthStorage {
            path: path.clone(),
            credentials: HashMap::new(),
        };

        storage.set_api_key("openai", "sk-save-test");
        storage.save().expect("save should succeed");

        // Read the raw file and verify structure.
        let content = fs::read_to_string(&path).expect("file should exist after save");
        let parsed: HashMap<String, CredentialEntry> =
            serde_json::from_str(&content).expect("valid JSON");
        assert_eq!(parsed.len(), 1);
        match parsed.get("openai").unwrap() {
            CredentialEntry::ApiKey { key } => assert_eq!(key, "sk-save-test"),
            _ => panic!("expected ApiKey variant"),
        }
    }

    #[test]
    fn test_load_from_explicit_path() {
        let (_td, path) = scratch_path();
        // Write a valid auth file manually.
        let json = r#"{"test-provider":{"type":"api_key","key":"from-file"}}"#;
        fs::write(&path, json).expect("write sample file");

        let storage = AuthStorage::load_from(path).expect("load_from should succeed");
        assert_eq!(storage.get_api_key("test-provider"), Some("from-file".into()));
    }

    #[test]
    fn test_load_from_nonexistent_returns_empty() {
        let dir = TempDir::new().expect("tempdir creation failed");
        let path = dir.path().join("does-not-exist.json");
        let storage = AuthStorage::load_from(path).expect("load_from on missing file should return empty");
        assert!(storage.credentials.is_empty());
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let (_td, path) = scratch_path();
        {
            let mut storage = AuthStorage {
                path: path.clone(),
                credentials: HashMap::new(),
            };
            storage.set_api_key("openai", "sk-roundtrip");
            storage.set_oauth(
                "anthropic",
                "ant-access-token",
                Some("ant-refresh-token".into()),
                Some(9999999999),
                "Bearer",
                None,
            );
            storage.save().expect("save");
        }
        // Load back using explicit path.
        let storage = AuthStorage::load_from(path).expect("reload");
        assert_eq!(storage.get_api_key("openai"), Some("sk-roundtrip".into()));
        match storage.credentials.get("anthropic").unwrap() {
            CredentialEntry::OAuth {
                access_token,
                refresh_token,
                expires_at,
                token_type,
                ..
            } => {
                assert_eq!(access_token, "ant-access-token");
                assert_eq!(refresh_token.as_deref(), Some("ant-refresh-token"));
                assert_eq!(*expires_at, Some(9999999999));
                assert_eq!(token_type.as_deref(), Some("Bearer"));
            }
            _ => panic!("expected OAuth variant"),
        }
    }

    #[test]
    fn test_default_path_looks_reasonable() {
        let path = AuthStorage::default_path();
        let path_str = path.to_string_lossy();
        assert!(path_str.contains(".pi"), "path should contain .pi: {path_str}");
        assert!(path_str.ends_with("auth.json"), "path should end with auth.json: {path_str}");
    }

    #[test]
    fn test_load_default_path_does_not_panic() {
        // This should never panic — missing file → empty storage, IO error → Err.
        let _ = AuthStorage::load();
    }
}
