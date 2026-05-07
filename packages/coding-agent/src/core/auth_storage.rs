//! On-disk persistence for OAuth tokens / API keys.
//!
//! Mirrors `pi-mono/packages/coding-agent/src/core/auth-storage.ts`. Records
//! are keyed by provider id and persisted to `~/.hand/auth.json` with Unix
//! mode `0600` (owner read/write only).
//!
//! ## Wire format
//!
//! The JSON file is a flat object: `{ <provider_id>: <AuthRecord>, ... }`.
//! Each [`AuthRecord`] is a discriminated union on the `type` field:
//!
//! - `{"type": "api_key", "key": "sk-..."}` — manually entered API key.
//! - `{"type": "oauth", "access": "...", "refresh": "...", "expires": <ms>,
//!   ...extra}` — credentials from an OAuth flow. `expires` is unix
//!   milliseconds. Provider-specific extras (e.g. `account`, `email`) are
//!   preserved as opaque JSON in [`AuthRecord::Oauth::extra`].
//!
//! These field names match the TypeScript reference exactly so that
//! `pi-coding-agent` and `hand` can read each other's `auth.json` if a user
//! points them at the same file.
//!
//! ## Persistence
//!
//! [`AuthStorage::save`] writes atomically via [`tempfile::NamedTempFile`]:
//! the JSON is staged to a sibling tmp file, then renamed into place. After
//! the rename succeeds, file mode is forced to `0o600` on Unix (no-op on
//! other platforms). Parent directories are created as needed.
//!
//! ## Concurrency
//!
//! Unlike the TypeScript reference this layer does **not** acquire a file
//! lock — it's a pure read/write surface. Higher-level code that needs
//! refresh-token serialisation must coordinate externally. The atomic
//! rename still protects against torn writes from a single process.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;
use thiserror::Error;

/// Errors raised by [`AuthStorage`] operations.
#[derive(Debug, Error)]
pub enum AuthStorageError {
    /// `dirs::home_dir()` returned `None` while resolving the default path.
    #[error("home directory not found")]
    NoHomeDir,
    /// Filesystem I/O error.
    #[error("io error at {path}: {source}", path = .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// JSON parse or emit error.
    #[error("json error at {path}: {source}", path = .path.display())]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    /// File contents are syntactically valid JSON but not the expected
    /// `Record<provider, AuthRecord>` shape.
    #[error("invalid auth file at {path}: {reason}", path = .path.display())]
    Invalid { path: PathBuf, reason: String },
}

/// Per-provider credential record.
///
/// Discriminated by the `type` field (`"api_key"` or `"oauth"`) — matches
/// the TS reference's `AuthCredential` union exactly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthRecord {
    /// Manually entered API key.
    ApiKey {
        /// The API key value, or a `${ENV_VAR}` reference resolved by
        /// upstream code.
        key: String,
    },
    /// Credentials issued by an OAuth flow.
    Oauth {
        /// Current access token.
        access: String,
        /// Refresh token, used to mint new access tokens after expiry.
        refresh: String,
        /// Unix milliseconds at which `access` expires.
        expires: i64,
        /// Provider-specific fields (account id, scopes, etc.) preserved
        /// verbatim. Round-trips through serde without loss.
        #[serde(flatten)]
        extra: serde_json::Map<String, serde_json::Value>,
    },
}

impl AuthRecord {
    /// Convenience constructor for an API key record.
    pub fn api_key(key: impl Into<String>) -> Self {
        Self::ApiKey { key: key.into() }
    }

    /// Convenience constructor for an OAuth record without extra fields.
    pub fn oauth(
        access: impl Into<String>,
        refresh: impl Into<String>,
        expires: i64,
    ) -> Self {
        Self::Oauth {
            access: access.into(),
            refresh: refresh.into(),
            expires,
            extra: serde_json::Map::new(),
        }
    }
}

/// Filesystem-backed credential store.
///
/// Cheap to construct — the on-disk file is opened lazily on each
/// [`load`](Self::load) / [`save`](Self::save) call. Mutating helpers
/// ([`set`](Self::set), [`remove`](Self::remove)) load, mutate, and save in
/// one shot; callers that need to batch many edits should use
/// [`load`](Self::load) + [`save`](Self::save) directly.
pub struct AuthStorage {
    path: PathBuf,
}

impl AuthStorage {
    /// Default location: `~/.hand/auth.json`.
    pub fn default_path() -> Result<PathBuf, AuthStorageError> {
        let home = dirs::home_dir().ok_or(AuthStorageError::NoHomeDir)?;
        Ok(home.join(".hand").join("auth.json"))
    }

    /// Construct with the default path.
    pub fn new() -> Result<Self, AuthStorageError> {
        Ok(Self {
            path: Self::default_path()?,
        })
    }

    /// Construct pointing at an explicit path. Intended for tests and for
    /// callers that override the location via env var.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Path of the underlying JSON file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load all records from disk. A missing file is **not** an error and
    /// returns an empty map. Malformed JSON or a non-object root yields an
    /// error so the caller can decide whether to surface or recover.
    pub fn load(&self) -> Result<HashMap<String, AuthRecord>, AuthStorageError> {
        let raw = match fs::read_to_string(&self.path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
            Err(source) => {
                return Err(AuthStorageError::Io {
                    path: self.path.clone(),
                    source,
                });
            }
        };

        if raw.trim().is_empty() {
            return Ok(HashMap::new());
        }

        // Parse to a generic `Value` first so we can produce a focused
        // `Invalid` error for the "not an object" case rather than a raw
        // serde error talking about `expected struct`.
        let value: serde_json::Value =
            serde_json::from_str(&raw).map_err(|source| AuthStorageError::Json {
                path: self.path.clone(),
                source,
            })?;
        if !value.is_object() {
            return Err(AuthStorageError::Invalid {
                path: self.path.clone(),
                reason: "expected a JSON object at the root".into(),
            });
        }

        serde_json::from_value(value).map_err(|source| AuthStorageError::Json {
            path: self.path.clone(),
            source,
        })
    }

    /// Persist `records` to disk atomically and force mode `0600` on Unix.
    ///
    /// Creates the parent directory if it doesn't exist.
    pub fn save(
        &self,
        records: &HashMap<String, AuthRecord>,
    ) -> Result<(), AuthStorageError> {
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(|source| AuthStorageError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let body = serde_json::to_string_pretty(records).map_err(|source| {
            AuthStorageError::Json {
                path: self.path.clone(),
                source,
            }
        })?;

        // Atomic write: stage in the same directory, then rename. Same
        // pattern as the F30 settings migration path.
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        let mut tmp =
            NamedTempFile::new_in(parent).map_err(|source| AuthStorageError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        tmp.write_all(body.as_bytes())
            .map_err(|source| AuthStorageError::Io {
                path: tmp.path().to_path_buf(),
                source,
            })?;
        tmp.as_file()
            .sync_all()
            .map_err(|source| AuthStorageError::Io {
                path: tmp.path().to_path_buf(),
                source,
            })?;
        tmp.persist(&self.path)
            .map_err(|e| AuthStorageError::Io {
                path: self.path.clone(),
                source: e.error,
            })?;

        // Force 0600 on Unix. The temp file may have been created with the
        // process umask (commonly 0644); we tighten it post-rename so the
        // visible file is never world-readable. No-op on Windows.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&self.path)
                .map_err(|source| AuthStorageError::Io {
                    path: self.path.clone(),
                    source,
                })?
                .permissions();
            perms.set_mode(0o600);
            fs::set_permissions(&self.path, perms).map_err(|source| AuthStorageError::Io {
                path: self.path.clone(),
                source,
            })?;
        }

        Ok(())
    }

    /// Look up the record for one provider. `Ok(None)` if the file is
    /// missing or the provider isn't present.
    pub fn get(&self, provider: &str) -> Result<Option<AuthRecord>, AuthStorageError> {
        Ok(self.load()?.remove(provider))
    }

    /// Insert or replace the record for one provider. Loads, mutates,
    /// saves.
    pub fn set(&self, provider: &str, record: AuthRecord) -> Result<(), AuthStorageError> {
        let mut records = self.load()?;
        records.insert(provider.to_string(), record);
        self.save(&records)
    }

    /// Drop the record for one provider. Idempotent — removing an absent
    /// provider is a no-op (still triggers a save so the file's mode is
    /// re-asserted).
    pub fn remove(&self, provider: &str) -> Result<(), AuthStorageError> {
        let mut records = self.load()?;
        if records.remove(provider).is_some() {
            self.save(&records)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn storage_in(dir: &TempDir) -> AuthStorage {
        AuthStorage::at(dir.path().join("auth.json"))
    }

    #[test]
    fn load_missing_file_returns_empty_map() {
        let dir = TempDir::new().unwrap();
        let s = storage_in(&dir);
        let map = s.load().unwrap();
        assert!(map.is_empty());
    }

    #[test]
    fn save_then_load_round_trips_api_key() {
        let dir = TempDir::new().unwrap();
        let s = storage_in(&dir);
        let mut input = HashMap::new();
        input.insert("openai".to_string(), AuthRecord::api_key("sk-test"));
        s.save(&input).unwrap();
        let loaded = s.load().unwrap();
        assert_eq!(loaded, input);
    }

    #[test]
    fn set_then_get_returns_record() {
        let dir = TempDir::new().unwrap();
        let s = storage_in(&dir);
        s.set("openai", AuthRecord::api_key("sk-1")).unwrap();
        let got = s.get("openai").unwrap();
        assert_eq!(got, Some(AuthRecord::api_key("sk-1")));
        // Unknown provider is `None`, not an error.
        assert!(s.get("missing").unwrap().is_none());
    }

    #[test]
    fn remove_drops_provider() {
        let dir = TempDir::new().unwrap();
        let s = storage_in(&dir);
        s.set("openai", AuthRecord::api_key("sk-1")).unwrap();
        s.remove("openai").unwrap();
        assert!(s.get("openai").unwrap().is_none());
        // Removing again is a no-op.
        s.remove("openai").unwrap();
    }

    #[test]
    fn multiple_providers_coexist() {
        let dir = TempDir::new().unwrap();
        let s = storage_in(&dir);
        s.set("openai", AuthRecord::api_key("sk-1")).unwrap();
        s.set("anthropic", AuthRecord::api_key("sk-2")).unwrap();
        let all = s.load().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(
            all.get("openai"),
            Some(&AuthRecord::api_key("sk-1")),
        );
        assert_eq!(
            all.get("anthropic"),
            Some(&AuthRecord::api_key("sk-2")),
        );
    }

    #[test]
    fn oauth_record_round_trip_preserves_all_fields() {
        let dir = TempDir::new().unwrap();
        let s = storage_in(&dir);
        let mut extra = serde_json::Map::new();
        extra.insert(
            "account".to_string(),
            serde_json::Value::String("user@example.com".into()),
        );
        let rec = AuthRecord::Oauth {
            access: "access-xyz".into(),
            refresh: "refresh-xyz".into(),
            expires: 1_700_000_000_000,
            extra,
        };
        s.set("anthropic", rec.clone()).unwrap();
        let got = s.get("anthropic").unwrap().unwrap();
        assert_eq!(got, rec);
    }

    #[test]
    fn malformed_file_returns_json_error() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("auth.json");
        fs::write(&path, "{not json").unwrap();
        let s = AuthStorage::at(&path);
        let err = s.load().unwrap_err();
        assert!(
            matches!(err, AuthStorageError::Json { .. }),
            "got: {err:?}",
        );
    }

    #[test]
    fn non_object_root_returns_invalid_error() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("auth.json");
        fs::write(&path, "[]").unwrap();
        let s = AuthStorage::at(&path);
        let err = s.load().unwrap_err();
        assert!(
            matches!(err, AuthStorageError::Invalid { .. }),
            "got: {err:?}",
        );
    }

    #[test]
    fn save_creates_parent_directory() {
        let dir = TempDir::new().unwrap();
        let nested = dir.path().join("missing").join("dir").join("auth.json");
        let s = AuthStorage::at(&nested);
        s.set("openai", AuthRecord::api_key("sk-1")).unwrap();
        assert!(nested.exists());
        let got = s.get("openai").unwrap();
        assert_eq!(got, Some(AuthRecord::api_key("sk-1")));
    }

    #[test]
    fn empty_file_treated_as_empty_map() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("auth.json");
        fs::write(&path, "").unwrap();
        let s = AuthStorage::at(&path);
        let map = s.load().unwrap();
        assert!(map.is_empty());
    }

    #[test]
    fn wire_shape_matches_typescript_reference() {
        // Pin the on-disk JSON shape so it stays interoperable with
        // pi-coding-agent. Specifically: top-level is `{provider: rec}`,
        // discriminator key is "type", api_key uses "key", oauth uses
        // "access" / "refresh" / "expires" (ms).
        let dir = TempDir::new().unwrap();
        let s = storage_in(&dir);
        s.set("openai", AuthRecord::api_key("sk-1")).unwrap();
        s.set(
            "anthropic",
            AuthRecord::oauth("a", "r", 1_700_000_000_000),
        )
        .unwrap();
        let raw = fs::read_to_string(s.path()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();

        let openai = &v["openai"];
        assert_eq!(openai["type"], "api_key");
        assert_eq!(openai["key"], "sk-1");

        let anthropic = &v["anthropic"];
        assert_eq!(anthropic["type"], "oauth");
        assert_eq!(anthropic["access"], "a");
        assert_eq!(anthropic["refresh"], "r");
        assert_eq!(anthropic["expires"], 1_700_000_000_000_i64);
    }

    #[cfg(unix)]
    #[test]
    fn unix_mode_is_0600_after_save() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let s = storage_in(&dir);
        s.set("openai", AuthRecord::api_key("sk-1")).unwrap();
        let mode = fs::metadata(s.path()).unwrap().permissions().mode();
        // mode() returns the full st_mode; mask to permission bits.
        assert_eq!(
            mode & 0o777,
            0o600,
            "expected 0600, got {:o}",
            mode & 0o777,
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_mode_reasserted_on_subsequent_save() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let s = storage_in(&dir);
        s.set("openai", AuthRecord::api_key("sk-1")).unwrap();
        // Loosen the perms to simulate a tampered file.
        let mut perms = fs::metadata(s.path()).unwrap().permissions();
        perms.set_mode(0o644);
        fs::set_permissions(s.path(), perms).unwrap();
        // A second save should clamp it back to 0600.
        s.set("openai", AuthRecord::api_key("sk-2")).unwrap();
        let mode = fs::metadata(s.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
