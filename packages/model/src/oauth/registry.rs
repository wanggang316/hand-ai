//! OAuth provider registry and on-disk credential storage.
//!
//! Storage layout (JSON, pretty-printed):
//! ```json
//! {
//!   "providers": {
//!     "anthropic":     { "provider_id": "anthropic",     ... },
//!     "openai-codex":  { "provider_id": "openai-codex",  ... },
//!     "github-copilot":{ "provider_id": "github-copilot",... }
//!   }
//! }
//! ```
//!
//! Writes are atomic (temp file + rename) to avoid leaving the file in a
//! half-written state if the process is killed mid-flush.
//!
//! On Unix, the parent directory is forced to mode `0700` and the storage
//! file to `0600` so other local users cannot read persisted tokens.
//! Concurrent `save()` / `remove()` calls on the same `OAuthRegistry`
//! instance are serialized through an internal `tokio::sync::Mutex` so the
//! load-modify-write cycle stays atomic from the perspective of an in-process
//! caller.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::sync::Mutex;

use super::anthropic::AnthropicOAuthProvider;
use super::github_copilot::GithubCopilotOAuthProvider;
use super::openai_codex::OpenAiCodexOAuthProvider;
use super::types::{OAuthAuthInfo, OAuthError, OAuthProvider, OAuthProviderId};

/// On-disk schema. Wrapped in a struct so we can extend the file format
/// without breaking older readers.
#[derive(Debug, Default, Serialize, Deserialize)]
struct StorageFile {
    #[serde(default)]
    providers: HashMap<String, OAuthAuthInfo>,
}

/// Registry of OAuth providers and the credential store backing them.
pub struct OAuthRegistry {
    providers: HashMap<OAuthProviderId, Box<dyn OAuthProvider>>,
    storage_path: PathBuf,
    /// Serializes save/remove cycles so two tasks racing through
    /// `load → mutate → write` don't drop one another's edits.
    write_lock: Mutex<()>,
}

impl OAuthRegistry {
    /// Build a registry with all built-in providers and the default storage
    /// path (`~/.hand-ai/oauth.json`). Falls back to the current directory if
    /// the home directory cannot be resolved.
    pub fn new() -> Self {
        Self::with_storage_path(default_storage_path())
    }

    /// Build a registry with all built-in providers and a custom storage
    /// path (handy for tests).
    pub fn with_storage_path(path: PathBuf) -> Self {
        let mut providers: HashMap<OAuthProviderId, Box<dyn OAuthProvider>> = HashMap::new();
        providers.insert(
            OAuthProviderId::Anthropic,
            Box::new(AnthropicOAuthProvider::new()),
        );
        providers.insert(
            OAuthProviderId::OpenAICodex,
            Box::new(OpenAiCodexOAuthProvider::new()),
        );
        providers.insert(
            OAuthProviderId::GithubCopilot,
            Box::new(GithubCopilotOAuthProvider::new()),
        );
        Self {
            providers,
            storage_path: path,
            write_lock: Mutex::new(()),
        }
    }

    /// Return the provider implementation for `id`, if registered.
    pub fn get(&self, id: OAuthProviderId) -> Option<&dyn OAuthProvider> {
        self.providers.get(&id).map(|b| b.as_ref())
    }

    /// All registered provider ids, in the canonical (Anthropic, OpenAI,
    /// GitHub) order.
    pub fn ids(&self) -> Vec<OAuthProviderId> {
        let mut out = Vec::with_capacity(self.providers.len());
        for id in [
            OAuthProviderId::Anthropic,
            OAuthProviderId::OpenAICodex,
            OAuthProviderId::GithubCopilot,
        ] {
            if self.providers.contains_key(&id) {
                out.push(id);
            }
        }
        out
    }

    /// Path the registry persists to.
    pub fn storage_path(&self) -> &Path {
        &self.storage_path
    }

    /// Load the credential store from disk. Missing file is not an error and
    /// returns an empty map.
    pub async fn load(&self) -> Result<HashMap<OAuthProviderId, OAuthAuthInfo>, OAuthError> {
        self.load_inner().await
    }

    async fn load_inner(&self) -> Result<HashMap<OAuthProviderId, OAuthAuthInfo>, OAuthError> {
        let bytes = match fs::read(&self.storage_path).await {
            Ok(b) => b,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
            Err(err) => return Err(OAuthError::Io(err)),
        };
        if bytes.is_empty() {
            return Ok(HashMap::new());
        }
        let file: StorageFile = serde_json::from_slice(&bytes)?;
        let mut out = HashMap::with_capacity(file.providers.len());
        for (_, info) in file.providers {
            out.insert(info.provider_id, info);
        }
        Ok(out)
    }

    /// Persist `info`, merging with any existing on-disk records.
    pub async fn save(&self, info: &OAuthAuthInfo) -> Result<(), OAuthError> {
        let _guard = self.write_lock.lock().await;
        let mut map = self.load_inner().await?;
        map.insert(info.provider_id, info.clone());
        self.write_all(&map).await
    }

    /// Remove the record for `id` if present.
    pub async fn remove(&self, id: OAuthProviderId) -> Result<(), OAuthError> {
        let _guard = self.write_lock.lock().await;
        let mut map = self.load_inner().await?;
        map.remove(&id);
        self.write_all(&map).await
    }

    async fn write_all(
        &self,
        map: &HashMap<OAuthProviderId, OAuthAuthInfo>,
    ) -> Result<(), OAuthError> {
        if let Some(parent) = self.storage_path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).await?;
            // Restrict the directory to the owner only. We do this every
            // write (not just on creation) so existing trees that pre-date
            // this hardening get tightened on first save.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let perms = std::fs::Permissions::from_mode(0o700);
                fs::set_permissions(parent, perms).await?;
            }
        }

        // Serialize with stable string keys so the file is human-inspectable.
        let mut providers = HashMap::with_capacity(map.len());
        for (id, info) in map {
            providers.insert(id.as_str().to_string(), info.clone());
        }
        let file = StorageFile { providers };
        let json = serde_json::to_vec_pretty(&file)?;

        // Atomic write: write to a sibling temp file, then rename.
        let tmp = tmp_path(&self.storage_path);
        fs::write(&tmp, &json).await?;
        // Tighten permissions on the temp file *before* the rename so there
        // is no observable window where the file exists at the final path
        // with default (umask-derived) permissions.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            fs::set_permissions(&tmp, perms).await?;
        }
        fs::rename(&tmp, &self.storage_path).await?;
        // Re-apply 0600 after rename: on some filesystems the rename can
        // change permission bits, and pre-existing files may have been
        // world-readable from before this hardening landed.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            fs::set_permissions(&self.storage_path, perms).await?;
        }
        Ok(())
    }
}

impl Default for OAuthRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn default_storage_path() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        home.join(".hand-ai").join("oauth.json")
    } else {
        PathBuf::from(".hand-ai").join("oauth.json")
    }
}

fn tmp_path(target: &Path) -> PathBuf {
    let mut name = target
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".tmp");
    target.with_file_name(name)
}
