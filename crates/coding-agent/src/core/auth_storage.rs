//! On-disk persistence for OAuth tokens / API keys.
//!
//! Records are keyed by provider id and persisted to
//! `~/.hand/agent/auth.json` with Unix mode `0600` (owner read/write
//! only).
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
//! Field names are stable across compatible coding-agent
//! implementations so an `auth.json` written by one client can be read
//! by another pointing at the same file.
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
//!
//! Note that [`AuthStorage::set`] and [`AuthStorage::remove`] perform a
//! load-modify-save cycle and are **not** safe under concurrent writers,
//! whether in-process (multiple threads sharing one `&AuthStorage`) or
//! cross-process. Callers needing atomic read-modify-write must serialize
//! externally — e.g. wrap the storage in a `Mutex`.
//!
//! ## Security
//!
//! Records are stored in **plaintext**. The only protection is the
//! filesystem `0600` mode (owner-only on Unix). On Windows there is
//! currently no equivalent enforcement — a follow-up should add NTFS
//! ACL hardening or OS-keychain integration. Full-disk encryption,
//! when enabled, provides at-rest protection but does not protect the
//! file from other processes running as the same user.

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
    pub fn oauth(access: impl Into<String>, refresh: impl Into<String>, expires: i64) -> Self {
        Self::Oauth {
            access: access.into(),
            refresh: refresh.into(),
            expires,
            extra: serde_json::Map::new(),
        }
    }
}

/// Detect whether a raw token string looks like a Claude.ai SUBSCRIPTION
/// OAuth token rather than an API key.
///
/// Anthropic ships two token shapes for the `anthropic` provider:
/// - `sk-ant-api...` — programmatic API keys, intended for SDK / API use.
/// - `sk-ant-oat...` — OAuth tokens issued by the Claude.ai subscription
///   flow, intended for the official Claude.ai UI ONLY. Using them for
///   direct API calls violates Anthropic's TOS and can get the
///   account suspended.
///
/// The interactive mode uses this to warn the user once per session
/// before sending requests with a subscription token. Returns true for
/// any string starting with `sk-ant-oat`; the actual server-side
/// suffix (`01-`, etc.) may change, so we anchor on the prefix.
pub fn is_anthropic_subscription_token(token: &str) -> bool {
    token.starts_with("sk-ant-oat")
}

/// Detect whether an `AuthRecord` represents an Anthropic Claude.ai
/// subscription credential. Returns true for:
/// - Any `Oauth` record under the `anthropic` provider (the storage
///   layer only ever writes oauth records for the subscription flow).
/// - Any `ApiKey` record whose stored key starts with `sk-ant-oat`.
///
/// `provider_id` must be the provider's id (e.g. `"anthropic"`); the
/// check is a no-op for any other provider so a user with an OAuth
/// record under, say, `"google"` doesn't trigger the warning.
pub fn record_is_anthropic_subscription(provider_id: &str, record: &AuthRecord) -> bool {
    if provider_id != "anthropic" {
        return false;
    }
    match record {
        AuthRecord::Oauth { .. } => true,
        AuthRecord::ApiKey { key } => is_anthropic_subscription_token(key),
    }
}

/// Filesystem-backed credential store.
///
/// Cheap to construct — the on-disk file is opened lazily on each
/// [`load`](Self::load) / [`save`](Self::save) call. Mutating helpers
/// ([`set`](Self::set), [`remove`](Self::remove)) load, mutate, and save in
/// one shot; callers that need to batch many edits should use
/// [`load`](Self::load) + [`save`](Self::save) directly.
///
/// The struct also carries a process-local "runtime overrides" layer
/// — an in-memory map of `provider -> api_key` strings that takes
/// priority over the disk-backed records. Use
/// [`set_runtime_api_key`](Self::set_runtime_api_key) to inject a key
/// for the lifetime of a process (typical use: a hosted dev tool
/// passing the user's session token down to the agent without ever
/// writing it to disk). [`remove_runtime_api_key`](Self::remove_runtime_api_key)
/// drops the override; subsequent reads fall back to disk again.
///
/// Runtime overrides are SHARED across `Clone`s of `AuthStorage`
/// pointed at the same disk path — they live in an `Arc<Mutex<…>>`,
/// so cloning the storage cheaply still gives the caller the same
/// view of process-local credentials.
#[derive(Clone)]
pub struct AuthStorage {
    path: PathBuf,
    runtime_overrides: std::sync::Arc<std::sync::Mutex<HashMap<String, String>>>,
}

impl AuthStorage {
    /// Default location: `~/.hand/agent/auth.json`.
    ///
    /// Lives under the same `agent/` subdir as `settings.yaml` and matches
    /// the TS reference (`~/.pi/agent/auth.json`) so the two ports stay
    /// wire-compatible if pointed at a shared layout.
    pub fn default_path() -> Result<PathBuf, AuthStorageError> {
        let home = dirs::home_dir().ok_or(AuthStorageError::NoHomeDir)?;
        Ok(home.join(".hand").join("agent").join("auth.json"))
    }

    /// Construct with the default path.
    pub fn new() -> Result<Self, AuthStorageError> {
        Ok(Self {
            path: Self::default_path()?,
            runtime_overrides: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
        })
    }

    /// Construct pointing at an explicit path. Intended for tests and for
    /// callers that override the location via env var.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            runtime_overrides: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
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
    pub fn save(&self, records: &HashMap<String, AuthRecord>) -> Result<(), AuthStorageError> {
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(|source| AuthStorageError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let body =
            serde_json::to_string_pretty(records).map_err(|source| AuthStorageError::Json {
                path: self.path.clone(),
                source,
            })?;

        // Atomic write: stage in the same directory, then rename. Same
        // pattern as the F30 settings migration path.
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        let mut tmp = NamedTempFile::new_in(parent).map_err(|source| AuthStorageError::Io {
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
        tmp.persist(&self.path).map_err(|e| AuthStorageError::Io {
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

    /// Insert or replace the record for one provider. Loads, mutates, saves.
    /// Best-effort: NOT safe under concurrent writers (in-process or
    /// cross-process). Callers that need atomic read-modify-write must
    /// serialize externally — e.g., wrap the `AuthStorage` in a `Mutex`.
    pub fn set(&self, provider: &str, record: AuthRecord) -> Result<(), AuthStorageError> {
        let mut records = self.load()?;
        records.insert(provider.to_string(), record);
        self.save(&records)
    }

    /// Drop the record for one provider. Idempotent — removing an absent
    /// provider is a no-op and does not touch the file. Same concurrency
    /// caveats as [`set`](Self::set).
    pub fn remove(&self, provider: &str) -> Result<(), AuthStorageError> {
        let mut records = self.load()?;
        if records.remove(provider).is_some() {
            self.save(&records)?;
        }
        Ok(())
    }

    /// Look up a redacted status for one provider — `configured: true`
    /// when a credential exists (runtime override OR disk record),
    /// `false` otherwise. The `source` discriminator carries which
    /// layer supplied the credential: `Runtime` when a process-local
    /// override is set, `Stored` when only the disk record exists.
    ///
    /// The returned `AuthStatus` carries NO secret material — its
    /// JSON serialisation never contains the api key, access token,
    /// or refresh token. UI consumers can ship the value to a logger
    /// without leaking credentials.
    pub fn get_auth_status(&self, provider: &str) -> AuthStatus {
        if self.has_runtime_override(provider) {
            return AuthStatus {
                configured: true,
                source: Some(AuthSource::Runtime),
            };
        }
        let configured = matches!(self.get(provider), Ok(Some(_)));
        AuthStatus {
            configured,
            source: if configured {
                Some(AuthSource::Stored)
            } else {
                None
            },
        }
    }

    /// Inject a process-local API key override for `provider`. Takes
    /// priority over any `auth.json` record on subsequent
    /// [`get_api_key`](Self::get_api_key) calls. Useful for hosted
    /// integrations that hand the agent a per-request credential
    /// without persisting it to disk.
    pub fn set_runtime_api_key(&self, provider: &str, key: impl Into<String>) {
        let mut overrides = self
            .runtime_overrides
            .lock()
            .expect("runtime_overrides mutex poisoned");
        overrides.insert(provider.to_string(), key.into());
    }

    /// Drop a process-local API key override for `provider`. Idempotent
    /// — removing an absent override is a no-op. Subsequent reads fall
    /// back to the disk record.
    pub fn remove_runtime_api_key(&self, provider: &str) {
        let mut overrides = self
            .runtime_overrides
            .lock()
            .expect("runtime_overrides mutex poisoned");
        overrides.remove(provider);
    }

    /// Resolve the effective API key for one provider. Order of
    /// precedence:
    /// 1. Process-local runtime override (set via
    ///    [`set_runtime_api_key`](Self::set_runtime_api_key)).
    /// 2. The on-disk record's ApiKey field (resolved through the
    ///    config-value pipeline so `!command` and env-var lookups
    ///    work transparently).
    /// 3. None when neither layer has a credential.
    ///
    /// OAuth records are NOT resolved by this method — callers handle
    /// the OAuth refresh dance separately.
    pub fn get_api_key(&self, provider: &str) -> Option<String> {
        if let Some(rt) = self
            .runtime_overrides
            .lock()
            .expect("runtime_overrides mutex poisoned")
            .get(provider)
            .cloned()
        {
            return Some(rt);
        }
        match self.get(provider).ok().flatten() {
            Some(AuthRecord::ApiKey { key }) => {
                crate::core::resolve_config_value::resolve_config_value(&key)
            }
            _ => None,
        }
    }

    fn has_runtime_override(&self, provider: &str) -> bool {
        self.runtime_overrides
            .lock()
            .expect("runtime_overrides mutex poisoned")
            .contains_key(provider)
    }
}

/// Redacted view of a provider's credential. Always safe to serialise
/// into logs or user-facing diagnostics — carries no secret material.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct AuthStatus {
    /// True iff some credential record exists for the provider.
    pub configured: bool,
    /// Which layer supplied the credential (currently only `Stored`
    /// since hand has no runtime-override or env-resolve layers yet).
    /// `None` when `configured` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<AuthSource>,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AuthSource {
    /// Loaded from the on-disk `auth.json` file.
    Stored,
    /// Set via [`AuthStorage::set_runtime_api_key`] for the lifetime
    /// of the process — takes priority over `Stored`.
    Runtime,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn storage_in(dir: &TempDir) -> AuthStorage {
        AuthStorage::at(dir.path().join("auth.json"))
    }

    // ===== Anthropic subscription detection =====

    /// `sk-ant-oat...` tokens are Claude.ai subscription OAuth tokens.
    /// Using them for direct API calls violates Anthropic TOS. The
    /// detector anchors on the `sk-ant-oat` prefix so the rule is
    /// stable across server-side suffix changes (`oat01-`, `oat02-`).
    #[test]
    fn is_anthropic_subscription_token_matches_oat_prefix() {
        assert!(is_anthropic_subscription_token("sk-ant-oat01-AAA-BBB"));
        assert!(is_anthropic_subscription_token("sk-ant-oat02-future-shape"));
        // Any oat-prefixed string — even a hypothetical un-versioned one —
        // must trigger the warning. False positives are acceptable; false
        // negatives are not.
        assert!(is_anthropic_subscription_token("sk-ant-oat-XXXX"));
    }

    /// Normal API keys (`sk-ant-api...`) must NOT trigger the warning.
    #[test]
    fn is_anthropic_subscription_token_rejects_api_keys() {
        assert!(!is_anthropic_subscription_token("sk-ant-api03-test"));
        assert!(!is_anthropic_subscription_token("sk-ant-api04-future"));
        assert!(!is_anthropic_subscription_token("sk-test"));
        assert!(!is_anthropic_subscription_token(""));
        // Defensive: prefix-similar but distinct strings stay out of the
        // trap. The prefix is `sk-ant-oat` exactly.
        assert!(!is_anthropic_subscription_token("sk-ant-other-flow"));
    }

    /// An OAuth record under the anthropic provider is ALWAYS a
    /// subscription credential — the Claude.ai OAuth flow is the only
    /// path that writes OAuth records under that provider key.
    #[test]
    fn record_is_anthropic_subscription_flags_oauth_under_anthropic() {
        let record = AuthRecord::oauth("a", "r", 0);
        assert!(record_is_anthropic_subscription("anthropic", &record));
        // Same record under a different provider must not trigger —
        // other providers have legitimate OAuth flows.
        assert!(!record_is_anthropic_subscription("google", &record));
        assert!(!record_is_anthropic_subscription("openai", &record));
    }

    /// API-key records under anthropic only trigger when the stored key
    /// is itself a subscription token.
    #[test]
    fn record_is_anthropic_subscription_flags_oat_api_key_under_anthropic() {
        let sub_key = AuthRecord::api_key("sk-ant-oat01-leaked");
        assert!(record_is_anthropic_subscription("anthropic", &sub_key));

        let api_key = AuthRecord::api_key("sk-ant-api03-real");
        assert!(!record_is_anthropic_subscription("anthropic", &api_key));

        // Same subscription-shaped key under a non-anthropic provider —
        // unusual but possible (someone copied it into the wrong slot);
        // we only warn for anthropic.
        let sub_key = AuthRecord::api_key("sk-ant-oat01-leaked");
        assert!(!record_is_anthropic_subscription("google", &sub_key));
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
        assert_eq!(all.get("openai"), Some(&AuthRecord::api_key("sk-1")),);
        assert_eq!(all.get("anthropic"), Some(&AuthRecord::api_key("sk-2")),);
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
        assert!(matches!(err, AuthStorageError::Json { .. }), "got: {err:?}",);
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
        s.set("anthropic", AuthRecord::oauth("a", "r", 1_700_000_000_000))
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
        assert_eq!(mode & 0o777, 0o600, "expected 0600, got {:o}", mode & 0o777,);
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

    /// `get_auth_status` returns a redacted view: `configured: true`
    /// with `source: stored` when a record exists, and the serialised
    /// JSON contains NEITHER the api key string NOR the OAuth tokens.
    /// Callers can log the value safely.
    #[test]
    fn get_auth_status_redacts_secrets() {
        let dir = TempDir::new().unwrap();
        let s = storage_in(&dir);
        s.set("anthropic", AuthRecord::api_key("secret-api-key"))
            .unwrap();
        s.set(
            "openai",
            AuthRecord::oauth("secret-access", "secret-refresh", 1_700_000_000_000),
        )
        .unwrap();

        let anthropic = s.get_auth_status("anthropic");
        let openai = s.get_auth_status("openai");
        assert!(anthropic.configured);
        assert_eq!(anthropic.source, Some(AuthSource::Stored));
        assert!(openai.configured);
        assert_eq!(openai.source, Some(AuthSource::Stored));

        let anthropic_json = serde_json::to_string(&anthropic).unwrap();
        let openai_json = serde_json::to_string(&openai).unwrap();
        assert!(
            !anthropic_json.contains("secret-api-key"),
            "API key leaked into auth status JSON: {anthropic_json}"
        );
        assert!(
            !openai_json.contains("secret-access"),
            "OAuth access token leaked: {openai_json}"
        );
        assert!(
            !openai_json.contains("secret-refresh"),
            "OAuth refresh token leaked: {openai_json}"
        );
    }

    /// Runtime override takes priority over the disk-backed record.
    /// `get_api_key` returns the override; `get_auth_status` reports
    /// `source: runtime`.
    #[test]
    fn runtime_override_beats_stored_api_key() {
        let dir = TempDir::new().unwrap();
        let s = storage_in(&dir);
        s.set("anthropic", AuthRecord::api_key("stored-key")).unwrap();
        s.set_runtime_api_key("anthropic", "runtime-key");

        assert_eq!(s.get_api_key("anthropic").as_deref(), Some("runtime-key"));
        let status = s.get_auth_status("anthropic");
        assert!(status.configured);
        assert_eq!(status.source, Some(AuthSource::Runtime));
    }

    /// Removing a runtime override falls back to the disk-stored
    /// record on the next read; `get_auth_status` reverts to
    /// `source: stored`.
    #[test]
    fn remove_runtime_override_reverts_to_stored() {
        let dir = TempDir::new().unwrap();
        let s = storage_in(&dir);
        s.set("anthropic", AuthRecord::api_key("stored-key")).unwrap();
        s.set_runtime_api_key("anthropic", "runtime-key");
        s.remove_runtime_api_key("anthropic");

        assert_eq!(s.get_api_key("anthropic").as_deref(), Some("stored-key"));
        let status = s.get_auth_status("anthropic");
        assert!(status.configured);
        assert_eq!(status.source, Some(AuthSource::Stored));
    }

    /// `Clone`s of `AuthStorage` share the runtime-override layer:
    /// setting an override on one handle is visible from another
    /// pointed at the same disk path. The override map lives behind
    /// `Arc<Mutex<…>>`, not the path.
    #[test]
    fn runtime_overrides_are_shared_across_clones() {
        let dir = TempDir::new().unwrap();
        let a = storage_in(&dir);
        let b = a.clone();
        a.set_runtime_api_key("openai", "from-a");
        assert_eq!(b.get_api_key("openai").as_deref(), Some("from-a"));
    }

    /// An unconfigured provider returns `{configured: false}` with no
    /// `source` field (serde skip-if-none keeps the JSON compact).
    #[test]
    fn get_auth_status_unconfigured_provider_has_no_source() {
        let dir = TempDir::new().unwrap();
        let s = storage_in(&dir);
        let status = s.get_auth_status("never-set");
        assert!(!status.configured);
        assert_eq!(status.source, None);
        let json = serde_json::to_string(&status).unwrap();
        // skip_serializing_if for None means `source` should be absent.
        assert!(
            !json.contains("\"source\""),
            "expected `source` to be omitted from JSON for unconfigured, got: {json}"
        );
    }
}
