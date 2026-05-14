//! Model registry — manages built-in and custom models.
//!
//! Mirrors `pi-mono/packages/coding-agent/src/core/model-registry.ts`. Owns
//! the catalog of models available to a session: built-in models from the
//! `model` crate's static catalog, plus custom models / per-provider /
//! per-model overrides loaded from `~/.hand/agent/models.json`. Provides
//! auth-aware queries used by [`crate::core::model_resolver`] and the RPC
//! dispatcher to surface a usable subset of the catalog.
//!
//! ## Iteration order
//!
//! [`ModelRegistry::all`] returns models sorted by `(provider.as_str(), id)`.
//! The order is stable across rebuilds with the same inputs so RPC handlers
//! such as `cycle_model` produce a predictable rotation.
//!
//! ## models.json shape
//!
//! ```jsonc
//! {
//!   "providers": {
//!     "openai": {
//!       "baseUrl": "https://example.com",      // optional override
//!       "apiKey": "OPENAI_KEY_VAR",            // env var name or literal
//!       "headers": { "X-Foo": "bar" },         // request headers
//!       "models": [ /* ModelDefinition */ ],   // optional custom models
//!       "modelOverrides": { /* per-model */ }  // optional overrides
//!     }
//!   }
//! }
//! ```
//!
//! ## Concurrency
//!
//! [`ModelRegistry`] is `Send + Sync` (built from owned data) but mutating
//! methods like [`ModelRegistry::register_provider`] / [`refresh`] are not
//! reentrant. Wrap in a `Mutex` if multiple threads need to mutate it.

use crate::core::auth_storage::{AuthRecord, AuthStorage};
use crate::core::resolve_config_value::{
    resolve_config_value_or_throw, resolve_config_value_uncached, resolve_headers_or_throw,
};
use model::{Compat, Cost, InputType, Model};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

// =============================================================================
// On-disk schema (models.json)
// =============================================================================

/// Per-model override specifying partial fields to merge over a built-in
/// model. Mirrors the TS `ModelOverrideSchema`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelOverride {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub reasoning: Option<bool>,
    #[serde(default)]
    pub thinking_level_map: Option<HashMap<String, Option<String>>>,
    #[serde(default)]
    pub input: Option<Vec<InputType>>,
    #[serde(default)]
    pub cost: Option<PartialCost>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
    #[serde(default)]
    pub headers: Option<HashMap<String, String>>,
    #[serde(default)]
    pub compat: Option<Compat>,
}

/// Partial cost override. Each field is optional; missing fields keep the
/// base model's value.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PartialCost {
    #[serde(default)]
    pub input: Option<f64>,
    #[serde(default)]
    pub output: Option<f64>,
    #[serde(default)]
    pub cache_read: Option<f64>,
    #[serde(default)]
    pub cache_write: Option<f64>,
}

/// Custom model definition that produces a brand-new `Model<Api>`. Mirrors the
/// TS `ModelDefinitionSchema`. Required fields are minimal because most
/// downstream defaults are sensible for local providers (Ollama, LM Studio).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelDefinition {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub api: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub reasoning: Option<bool>,
    #[serde(default)]
    pub thinking_level_map: Option<HashMap<String, Option<String>>>,
    #[serde(default)]
    pub input: Option<Vec<InputType>>,
    #[serde(default)]
    pub cost: Option<Cost>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
    #[serde(default)]
    pub headers: Option<HashMap<String, String>>,
    #[serde(default)]
    pub compat: Option<Compat>,
}

/// Provider entry inside `models.json`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderConfig {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub api: Option<String>,
    #[serde(default)]
    pub headers: Option<HashMap<String, String>>,
    #[serde(default)]
    pub compat: Option<Compat>,
    #[serde(default)]
    pub auth_header: Option<bool>,
    #[serde(default)]
    pub models: Option<Vec<ModelDefinition>>,
    #[serde(default)]
    pub model_overrides: Option<HashMap<String, ModelOverride>>,
}

/// Top-level shape of `models.json`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelsConfig {
    pub providers: HashMap<String, ProviderConfig>,
}

// =============================================================================
// Internal state types
// =============================================================================

/// Provider override (baseUrl / compat) applied on top of built-in models.
#[derive(Debug, Clone, Default)]
struct ProviderOverride {
    base_url: Option<String>,
    compat: Option<Compat>,
}

/// Per-provider request-time auth/headers. These never appear on `Model`
/// itself; they're applied at request build time by
/// [`ModelRegistry::api_key_and_headers`].
#[derive(Debug, Clone, Default)]
struct ProviderRequestConfig {
    api_key: Option<String>,
    headers: Option<HashMap<String, String>>,
    auth_header: Option<bool>,
}

/// Result of resolving a model's API key + request headers.
#[derive(Debug, Clone)]
pub enum ResolvedRequestAuth {
    /// Auth resolved successfully. `api_key` and `headers` may both be
    /// `None` if the model is auth-less (rare, but valid for fully open
    /// local providers).
    Ok {
        api_key: Option<String>,
        headers: Option<HashMap<String, String>>,
    },
    /// Auth resolution failed — typically a missing env var or shell command
    /// that exited non-zero. The reason is human-readable.
    Err { reason: String },
}

/// Auth-state summary for a provider, mirroring TS `AuthStatus`.
///
/// `source` follows the TS string enum exactly so wire-compatible diagnostic
/// surfaces stay aligned across implementations.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthStatus {
    pub configured: bool,
    pub source: Option<AuthSource>,
    pub label: Option<String>,
}

/// Where a provider's auth credentials live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthSource {
    /// Auth record persisted in `auth.json`.
    Stored,
    /// Runtime override (e.g. `--api-key`). Not yet wired in this port.
    Runtime,
    /// Environment variable.
    Environment,
    /// Fallback resolver returned a value. Not yet wired in this port.
    Fallback,
    /// Literal API key set in `models.json`.
    ModelsJsonKey,
    /// `!command` API key in `models.json`.
    ModelsJsonCommand,
}

impl AuthSource {
    /// Stable string form used by the TS reference.
    pub fn as_str(self) -> &'static str {
        match self {
            AuthSource::Stored => "stored",
            AuthSource::Runtime => "runtime",
            AuthSource::Environment => "environment",
            AuthSource::Fallback => "fallback",
            AuthSource::ModelsJsonKey => "models_json_key",
            AuthSource::ModelsJsonCommand => "models_json_command",
        }
    }
}

// =============================================================================
// Errors
// =============================================================================

/// Errors raised by [`ModelRegistry`] mutating operations. Read paths surface
/// problems via [`ModelRegistry::error`] rather than `Result`.
#[derive(Debug, Error)]
pub enum ModelRegistryError {
    /// Invalid `models.json` schema or semantic constraint violation.
    #[error("invalid models.json: {0}")]
    InvalidConfig(String),
    /// Bad `register_provider` input.
    #[error("invalid provider config for {provider}: {reason}")]
    InvalidProviderConfig { provider: String, reason: String },
}

// =============================================================================
// Built-in display names
// =============================================================================

/// Human-readable display names for built-in providers. Mirrors the TS
/// `BUILT_IN_PROVIDER_DISPLAY_NAMES` constant.
fn built_in_display_name(provider: &str) -> Option<&'static str> {
    Some(match provider {
        "anthropic" => "Anthropic",
        "amazon-bedrock" => "Amazon Bedrock",
        "azure-openai-responses" => "Azure OpenAI Responses",
        "cerebras" => "Cerebras",
        "cloudflare-ai-gateway" => "Cloudflare AI Gateway",
        "cloudflare-workers-ai" => "Cloudflare Workers AI",
        "deepseek" => "DeepSeek",
        "fireworks" => "Fireworks",
        "google" => "Google Gemini",
        "google-vertex" => "Google Vertex AI",
        "groq" => "Groq",
        "huggingface" => "Hugging Face",
        "kimi-coding" => "Kimi For Coding",
        "mistral" => "Mistral",
        "minimax" => "MiniMax",
        "minimax-cn" => "MiniMax (China)",
        "moonshotai" => "Moonshot AI",
        "moonshotai-cn" => "Moonshot AI (China)",
        "opencode" => "OpenCode Zen",
        "opencode-go" => "OpenCode Go",
        "openai" => "OpenAI",
        "openrouter" => "OpenRouter",
        "vercel-ai-gateway" => "Vercel AI Gateway",
        "xai" => "xAI",
        "zai" => "ZAI",
        "xiaomi" => "Xiaomi MiMo",
        "xiaomi-token-plan-cn" => "Xiaomi MiMo Token Plan (China)",
        "xiaomi-token-plan-ams" => "Xiaomi MiMo Token Plan (Amsterdam)",
        "xiaomi-token-plan-sgp" => "Xiaomi MiMo Token Plan (Singapore)",
        _ => return None,
    })
}

// =============================================================================
// Provider registration input (extension API)
// =============================================================================

/// Extension-supplied provider configuration for
/// [`ModelRegistry::register_provider`]. Mirrors the TS `ProviderConfigInput`
/// with two simplifications:
/// - `streamSimple` is omitted: this Rust port does not register dynamic
///   `Api` providers on `model::Client` — extensions that need to provide a
///   transport layer hook into `model::ApiProviderRegistry` directly.
/// - `oauth` is omitted for the same reason: dynamic OAuth provider
///   registration is not exposed yet. The data shape is preserved here so a
///   later port can extend it without breaking callers.
#[derive(Debug, Clone, Default)]
pub struct ProviderConfigInput {
    pub name: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub api: Option<String>,
    pub headers: Option<HashMap<String, String>>,
    pub auth_header: Option<bool>,
    pub models: Option<Vec<ProviderConfigInputModel>>,
}

/// Per-model entry inside [`ProviderConfigInput`]. Required fields match the
/// TS reference; defaults are caller-supplied.
#[derive(Debug, Clone)]
pub struct ProviderConfigInputModel {
    pub id: String,
    pub name: String,
    pub api: Option<String>,
    pub base_url: Option<String>,
    pub reasoning: bool,
    pub thinking_level_map: Option<HashMap<String, Option<String>>>,
    pub input: Vec<InputType>,
    pub cost: Cost,
    pub context_window: u64,
    pub max_tokens: u64,
    pub headers: Option<HashMap<String, String>>,
    pub compat: Option<Compat>,
}

// =============================================================================
// ModelRegistry
// =============================================================================

/// Aggregate, sorted catalog of [`Model`]s available to a session.
pub struct ModelRegistry {
    auth_storage: Option<AuthStorage>,
    models_json_path: Option<PathBuf>,
    models: Vec<Model>,
    provider_request_configs: HashMap<String, ProviderRequestConfig>,
    /// Per-(provider, modelId) request-time headers keyed `"provider:id"`.
    model_request_headers: HashMap<String, HashMap<String, String>>,
    registered_providers: HashMap<String, ProviderConfigInput>,
    load_error: Option<String>,
}

impl std::fmt::Debug for ModelRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelRegistry")
            .field("models_json_path", &self.models_json_path)
            .field("model_count", &self.models.len())
            .field(
                "provider_request_configs",
                &self.provider_request_configs.keys().collect::<Vec<_>>(),
            )
            .field(
                "registered_providers",
                &self.registered_providers.keys().collect::<Vec<_>>(),
            )
            .field("load_error", &self.load_error)
            .finish()
    }
}

impl Clone for ModelRegistry {
    fn clone(&self) -> Self {
        Self {
            auth_storage: None,
            models_json_path: self.models_json_path.clone(),
            models: self.models.clone(),
            provider_request_configs: self.provider_request_configs.clone(),
            model_request_headers: self.model_request_headers.clone(),
            registered_providers: self.registered_providers.clone(),
            load_error: self.load_error.clone(),
        }
    }
}

impl ModelRegistry {
    /// Default `models.json` location: `~/.hand/agent/models.json`. Returns
    /// `None` if the home directory can't be resolved.
    pub fn default_models_json_path() -> Option<PathBuf> {
        Some(
            dirs::home_dir()?
                .join(".hand")
                .join("agent")
                .join("models.json"),
        )
    }

    /// Build a registry that loads custom models from `~/.hand/agent/models.json`
    /// if it exists. Errors during load are recorded and surfaced via
    /// [`Self::error`]; built-in models remain available regardless.
    pub fn create(auth_storage: AuthStorage) -> Self {
        Self::with_path(auth_storage, Self::default_models_json_path())
    }

    /// Build a registry with an explicit `models.json` path. Pass `None` for
    /// "in-memory only" — no disk reads, no load errors.
    pub fn with_path(auth_storage: AuthStorage, models_json_path: Option<PathBuf>) -> Self {
        let mut s = Self {
            auth_storage: Some(auth_storage),
            models_json_path,
            models: Vec::new(),
            provider_request_configs: HashMap::new(),
            model_request_headers: HashMap::new(),
            registered_providers: HashMap::new(),
            load_error: None,
        };
        s.load_models();
        s
    }

    /// Build an in-memory registry — built-in catalog only, no `models.json`.
    pub fn in_memory(auth_storage: AuthStorage) -> Self {
        Self::with_path(auth_storage, None)
    }

    /// Build a registry from a [`model::Client`] without binding an
    /// [`AuthStorage`]. Preserves the original surface used by
    /// [`crate::core::agent_session::AgentSession`]: many call sites still
    /// drive the registry as a pure read-through view of the static catalog.
    /// Auth-aware queries ([`Self::has_configured_auth`], [`Self::is_using_oauth`])
    /// will report no configured auth in this mode.
    pub fn build(_client: &model::Client) -> Self {
        let mut s = Self {
            auth_storage: None,
            models_json_path: None,
            models: Vec::new(),
            provider_request_configs: HashMap::new(),
            model_request_headers: HashMap::new(),
            registered_providers: HashMap::new(),
            load_error: None,
        };
        s.load_models();
        s
    }

    /// Reload the catalog from disk, replaying registered providers on top.
    pub fn refresh(&mut self) {
        self.provider_request_configs.clear();
        self.model_request_headers.clear();
        self.load_error = None;
        self.load_models();
        // Replay registrations so dynamically-contributed models survive.
        let registrations: Vec<(String, ProviderConfigInput)> = self
            .registered_providers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (provider, config) in registrations {
            self.apply_provider_config(&provider, &config);
        }
    }

    /// Load error from the most recent `models.json` read, if any.
    pub fn error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    /// All models in stable sorted order.
    pub fn all(&self) -> &[Model] {
        &self.models
    }

    /// Models that have auth configured. Fast — does not refresh OAuth tokens.
    pub fn available(&self) -> Vec<Model> {
        self.models
            .iter()
            .filter(|m| self.has_configured_auth(m))
            .cloned()
            .collect()
    }

    /// Look up a model by `(provider, id)` exact match.
    pub fn find(&self, provider: &str, id: &str) -> Option<&Model> {
        self.models
            .iter()
            .find(|m| m.provider.as_str() == provider && m.id == id)
    }

    /// Find the next model after `current` in the iteration order. Wraps to
    /// the first model when at the end. Returns `None` if `current` isn't
    /// present or the registry is empty.
    pub fn next(&self, current: &Model) -> Option<&Model> {
        if self.models.is_empty() {
            return None;
        }
        let idx = self
            .models
            .iter()
            .position(|m| m.provider.as_str() == current.provider.as_str() && m.id == current.id)?;
        Some(&self.models[(idx + 1) % self.models.len()])
    }

    /// Number of models in the registry.
    pub fn len(&self) -> usize {
        self.models.len()
    }

    /// Whether the registry has no models.
    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }

    // ---------------------------------------------------------------------
    // Auth-aware queries
    // ---------------------------------------------------------------------

    /// Whether `model.provider` has any form of auth configured. Mirrors the
    /// TS `hasConfiguredAuth`. Checks, in order:
    /// 1. The bound [`AuthStorage`] (stored credential, env var, runtime
    ///    override, fallback resolver).
    /// 2. `models.json` provider entry has an `apiKey`.
    pub fn has_configured_auth(&self, model: &Model) -> bool {
        self.has_provider_auth_configured(model.provider.as_str())
    }

    /// Like [`has_configured_auth`] but keyed by raw provider id — used by
    /// query paths that don't have a `Model` in hand.
    pub fn has_provider_auth_configured(&self, provider: &str) -> bool {
        if self.has_auth_storage_credential(provider) {
            return true;
        }
        self.provider_request_configs
            .get(provider)
            .and_then(|c| c.api_key.as_ref())
            .is_some()
    }

    fn has_auth_storage_credential(&self, provider: &str) -> bool {
        let Some(auth) = self.auth_storage.as_ref() else {
            return false;
        };
        if matches!(auth.get(provider), Ok(Some(_))) {
            return true;
        }
        // Env var fallback (anthropic, openai, ...). Mirrors TS `getEnvApiKey`.
        model::env_api_keys::get_env_api_key_by_str(provider).is_some()
    }

    /// Whether the credential for `model.provider` is an OAuth record.
    pub fn is_using_oauth(&self, model: &Model) -> bool {
        let Some(auth) = self.auth_storage.as_ref() else {
            return false;
        };
        matches!(
            auth.get(model.provider.as_str()),
            Ok(Some(AuthRecord::Oauth { .. }))
        )
    }

    /// Pi-mono parity (issue #3686-adjacent): whether the credential
    /// configured for `model.provider` is an Anthropic Claude.ai
    /// SUBSCRIPTION credential rather than an API key. Pi-mono uses
    /// this to render a one-time "you're using a subscription token
    /// for API calls — that violates Anthropic's TOS" warning in
    /// interactive mode.
    ///
    /// Wider net than [`Self::is_using_oauth`]: this also catches the
    /// case where a user pasted an `sk-ant-oat...` OAuth token into
    /// the ApiKey slot of auth.json. Returns true for:
    /// - Any OAuth record under the `anthropic` provider, OR
    /// - Any ApiKey record under `anthropic` whose value starts with
    ///   `sk-ant-oat`.
    /// False for every other provider so a Google or OpenAI OAuth
    /// record doesn't trigger an irrelevant warning.
    pub fn is_anthropic_subscription_credential(&self, model: &Model) -> bool {
        let provider = model.provider.as_str();
        if provider != "anthropic" {
            return false;
        }
        let Some(auth) = self.auth_storage.as_ref() else {
            return false;
        };
        match auth.get(provider) {
            Ok(Some(record)) => {
                crate::core::auth_storage::record_is_anthropic_subscription(provider, &record)
            }
            _ => false,
        }
    }

    /// Resolve API key + request headers for a model. Mirrors the TS async
    /// `getApiKeyAndHeaders`. Sync in Rust because [`AuthStorage`] is sync.
    pub fn api_key_and_headers(&self, model: &Model) -> ResolvedRequestAuth {
        match self.try_resolve_request_auth(model) {
            Ok((api_key, headers)) => ResolvedRequestAuth::Ok { api_key, headers },
            Err(reason) => ResolvedRequestAuth::Err { reason },
        }
    }

    #[allow(clippy::type_complexity)]
    fn try_resolve_request_auth(
        &self,
        model: &Model,
    ) -> Result<(Option<String>, Option<HashMap<String, String>>), String> {
        let provider = model.provider.as_str();
        let provider_config = self.provider_request_configs.get(provider);

        // 1. Prefer auth-storage credential when bound.
        let api_key_from_auth = self
            .auth_storage
            .as_ref()
            .and_then(|s| match s.get(provider) {
                Ok(Some(AuthRecord::ApiKey { key })) => Some(key),
                _ => None,
            })
            .or_else(|| model::env_api_keys::get_env_api_key_by_str(provider));

        let api_key = match api_key_from_auth {
            Some(k) => Some(k),
            None => match provider_config.and_then(|c| c.api_key.as_deref()) {
                Some(raw) => Some(
                    resolve_config_value_or_throw(
                        raw,
                        &format!("API key for provider \"{provider}\""),
                    )
                    .map_err(|e| e.to_string())?,
                ),
                None => None,
            },
        };

        let provider_headers = resolve_headers_or_throw(
            provider_config.and_then(|c| c.headers.as_ref()),
            &format!("provider \"{provider}\""),
        )
        .map_err(|e| e.to_string())?;

        let model_request_key = format!("{provider}:{}", model.id);
        let model_headers = resolve_headers_or_throw(
            self.model_request_headers.get(&model_request_key),
            &format!("model \"{provider}/{}\"", model.id),
        )
        .map_err(|e| e.to_string())?;

        let mut headers: Option<HashMap<String, String>> =
            if model.headers.is_some() || provider_headers.is_some() || model_headers.is_some() {
                let mut merged: HashMap<String, String> = HashMap::new();
                if let Some(h) = model.headers.as_ref() {
                    merged.extend(h.iter().map(|(k, v)| (k.clone(), v.clone())));
                }
                if let Some(h) = provider_headers {
                    merged.extend(h);
                }
                if let Some(h) = model_headers {
                    merged.extend(h);
                }
                Some(merged)
            } else {
                None
            };

        if provider_config.and_then(|c| c.auth_header).unwrap_or(false) {
            let Some(key) = api_key.as_ref() else {
                return Err(format!("No API key found for \"{provider}\""));
            };
            let h = headers.get_or_insert_with(HashMap::new);
            h.insert("Authorization".to_string(), format!("Bearer {key}"));
        }

        let headers = headers.filter(|h| !h.is_empty());
        Ok((api_key, headers))
    }

    /// Auth status for `provider`, including request-time auth from
    /// `models.json`. Does **not** execute `!command` config values.
    pub fn provider_auth_status(&self, provider: &str) -> AuthStatus {
        if let Some(auth) = self.auth_storage.as_ref()
            && matches!(auth.get(provider), Ok(Some(_)))
        {
            return AuthStatus {
                configured: true,
                source: Some(AuthSource::Stored),
                label: None,
            };
        }
        if model::env_api_keys::get_env_api_key_by_str(provider).is_some() {
            return AuthStatus {
                // Matches the TS reference exactly: env-var presence sets
                // `source` but not `configured`. Surfaced in diagnostics so
                // the user can see "we found `OPENAI_API_KEY`" without us
                // claiming the auth is fully wired.
                configured: false,
                source: Some(AuthSource::Environment),
                label: None,
            };
        }

        let Some(provider_api_key) = self
            .provider_request_configs
            .get(provider)
            .and_then(|c| c.api_key.as_ref())
        else {
            return AuthStatus::default();
        };

        if provider_api_key.starts_with('!') {
            return AuthStatus {
                configured: true,
                source: Some(AuthSource::ModelsJsonCommand),
                label: None,
            };
        }
        if std::env::var(provider_api_key).is_ok() {
            return AuthStatus {
                configured: true,
                source: Some(AuthSource::Environment),
                label: Some(provider_api_key.clone()),
            };
        }
        AuthStatus {
            configured: true,
            source: Some(AuthSource::ModelsJsonKey),
            label: None,
        }
    }

    /// Display name for a provider. Falls back to the registered provider's
    /// `name` field, then the built-in display table, then the raw id.
    pub fn provider_display_name(&self, provider: &str) -> String {
        if let Some(reg) = self.registered_providers.get(provider)
            && let Some(name) = reg.name.as_ref()
        {
            return name.clone();
        }
        if let Some(name) = built_in_display_name(provider) {
            return name.to_string();
        }
        provider.to_string()
    }

    /// Resolve the API key for a provider, preferring [`AuthStorage`] /
    /// env vars over `models.json`. Sync; bypasses the command-result cache
    /// (matches TS `getApiKeyForProvider`).
    pub fn api_key_for_provider(&self, provider: &str) -> Option<String> {
        if let Some(auth) = self.auth_storage.as_ref()
            && let Ok(Some(AuthRecord::ApiKey { key })) = auth.get(provider)
        {
            return Some(key);
        }
        if let Some(env) = model::env_api_keys::get_env_api_key_by_str(provider) {
            return Some(env);
        }
        let raw = self
            .provider_request_configs
            .get(provider)
            .and_then(|c| c.api_key.as_deref())?;
        resolve_config_value_uncached(raw)
    }

    // ---------------------------------------------------------------------
    // Provider registration (extension API)
    // ---------------------------------------------------------------------

    /// Register a provider dynamically — typically from an extension. With
    /// `models`: replaces the provider's models. Without `models`, `base_url`
    /// only: overrides existing models' URLs.
    pub fn register_provider(
        &mut self,
        provider: &str,
        config: ProviderConfigInput,
    ) -> Result<(), ModelRegistryError> {
        Self::validate_provider_config(provider, &config)?;
        self.apply_provider_config(provider, &config);
        self.upsert_registered_provider(provider, config);
        Ok(())
    }

    /// Drop a previously-registered provider. Reloads the catalog so any
    /// built-in models that the provider had overridden return to their
    /// original state. No-op if the provider was never registered.
    pub fn unregister_provider(&mut self, provider: &str) {
        if self.registered_providers.remove(provider).is_some() {
            self.refresh();
        }
    }

    fn upsert_registered_provider(&mut self, provider: &str, config: ProviderConfigInput) {
        match self.registered_providers.get_mut(provider) {
            None => {
                self.registered_providers
                    .insert(provider.to_string(), config);
            }
            Some(existing) => {
                if config.name.is_some() {
                    existing.name = config.name;
                }
                if config.base_url.is_some() {
                    existing.base_url = config.base_url;
                }
                if config.api_key.is_some() {
                    existing.api_key = config.api_key;
                }
                if config.api.is_some() {
                    existing.api = config.api;
                }
                if config.headers.is_some() {
                    existing.headers = config.headers;
                }
                if config.auth_header.is_some() {
                    existing.auth_header = config.auth_header;
                }
                if config.models.is_some() {
                    existing.models = config.models;
                }
            }
        }
    }

    fn validate_provider_config(
        provider: &str,
        config: &ProviderConfigInput,
    ) -> Result<(), ModelRegistryError> {
        let Some(models) = config.models.as_ref() else {
            return Ok(());
        };
        if models.is_empty() {
            return Ok(());
        }
        if config.base_url.is_none() {
            return Err(ModelRegistryError::InvalidProviderConfig {
                provider: provider.to_string(),
                reason: "\"base_url\" is required when defining models".to_string(),
            });
        }
        if config.api_key.is_none() {
            return Err(ModelRegistryError::InvalidProviderConfig {
                provider: provider.to_string(),
                reason: "\"api_key\" is required when defining models".to_string(),
            });
        }
        for m in models {
            let api = m.api.as_deref().or(config.api.as_deref());
            if api.is_none() {
                return Err(ModelRegistryError::InvalidProviderConfig {
                    provider: provider.to_string(),
                    reason: format!("model {}: no \"api\" specified", m.id),
                });
            }
        }
        Ok(())
    }

    fn apply_provider_config(&mut self, provider: &str, config: &ProviderConfigInput) {
        // Stash request-time auth/headers.
        if config.api_key.is_some() || config.headers.is_some() || config.auth_header.is_some() {
            self.provider_request_configs.insert(
                provider.to_string(),
                ProviderRequestConfig {
                    api_key: config.api_key.clone(),
                    headers: config.headers.clone(),
                    auth_header: config.auth_header,
                },
            );
        }

        match (config.models.as_ref(), &config.base_url) {
            (Some(models), Some(base_url)) if !models.is_empty() => {
                // Full replacement.
                self.models.retain(|m| m.provider.as_str() != provider);
                let provider_enum = match model::types::Provider::from_str(provider) {
                    Some(p) => p,
                    None => {
                        // Unknown provider id: skip the model contribution
                        // but keep the request-config side effects so auth
                        // for built-ins still resolves.
                        return;
                    }
                };
                for m in models {
                    let api = parse_api(m.api.as_deref().or(config.api.as_deref()));
                    let Some(api) = api else { continue };
                    let request_key = format!("{provider}:{}", m.id);
                    if let Some(h) = m.headers.as_ref().filter(|h| !h.is_empty()) {
                        self.model_request_headers.insert(request_key, h.clone());
                    } else {
                        self.model_request_headers.remove(&request_key);
                    }
                    self.models.push(Model {
                        id: m.id.clone(),
                        name: m.name.clone(),
                        api,
                        provider: provider_enum,
                        base_url: m.base_url.clone().unwrap_or_else(|| base_url.clone()),
                        reasoning: m.reasoning,
                        input: m.input.clone(),
                        cost: m.cost.clone(),
                        context_window: m.context_window,
                        max_tokens: m.max_tokens,
                        headers: None,
                        compat: m.compat.clone(),
                        thinking_level_map: m.thinking_level_map.clone(),
                    });
                }
                resort(&mut self.models);
            }
            (_, Some(base_url)) => {
                // Override-only: rewrite baseUrl on existing entries.
                for m in self.models.iter_mut() {
                    if m.provider.as_str() == provider {
                        m.base_url = base_url.clone();
                    }
                }
            }
            _ => {}
        }
    }

    // ---------------------------------------------------------------------
    // Build pipeline (load_models / loadCustomModels / parseModels)
    // ---------------------------------------------------------------------

    fn load_models(&mut self) {
        let custom = match self.models_json_path.clone() {
            Some(path) => self.load_custom_models(&path),
            None => CustomModelsResult::default(),
        };

        if let Some(err) = custom.error {
            self.load_error = Some(err);
        }

        let built_in = self.load_built_in_models(&custom.overrides, &custom.model_overrides);
        let combined = merge_custom_models(built_in, custom.models);
        self.models = combined;
        resort(&mut self.models);
    }

    fn load_built_in_models(
        &self,
        overrides: &HashMap<String, ProviderOverride>,
        model_overrides: &HashMap<String, HashMap<String, ModelOverride>>,
    ) -> Vec<Model> {
        let mut out = Vec::new();
        for provider_key in model::get_provider_keys() {
            let provider_override = overrides.get(&provider_key);
            let per_model = model_overrides.get(&provider_key);
            for mut m in model::get_models(&provider_key) {
                if let Some(po) = provider_override {
                    if let Some(base) = po.base_url.as_ref() {
                        m.base_url = base.clone();
                    }
                    m.compat = merge_compat(m.compat.as_ref(), po.compat.as_ref());
                }
                if let Some(per_model) = per_model
                    && let Some(o) = per_model.get(&m.id)
                {
                    m = apply_model_override(m, o);
                }
                out.push(m);
            }
        }
        out
    }

    fn load_custom_models(&mut self, path: &Path) -> CustomModelsResult {
        if !path.exists() {
            return CustomModelsResult::default();
        }

        let raw = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(err) => {
                return CustomModelsResult::with_error(format!(
                    "Failed to load models.json: {err}\n\nFile: {}",
                    path.display()
                ));
            }
        };

        let stripped = strip_json_comments(&raw);
        let config: ModelsConfig = match serde_json::from_str(&stripped) {
            Ok(c) => c,
            Err(err) => {
                return CustomModelsResult::with_error(format!(
                    "Failed to parse models.json: {err}\n\nFile: {}",
                    path.display()
                ));
            }
        };

        if let Err(err) = validate_config(&config) {
            return CustomModelsResult::with_error(format!(
                "Invalid models.json schema:\n  - {err}\n\nFile: {}",
                path.display()
            ));
        }

        let mut overrides: HashMap<String, ProviderOverride> = HashMap::new();
        let mut model_overrides: HashMap<String, HashMap<String, ModelOverride>> = HashMap::new();

        for (provider, p) in &config.providers {
            if p.base_url.is_some() || p.compat.is_some() {
                overrides.insert(
                    provider.clone(),
                    ProviderOverride {
                        base_url: p.base_url.clone(),
                        compat: p.compat.clone(),
                    },
                );
            }
            self.store_provider_request_config(provider, p);

            if let Some(mos) = p.model_overrides.as_ref() {
                model_overrides.insert(provider.clone(), mos.clone());
                for (model_id, mo) in mos {
                    self.store_model_headers(provider, model_id, mo.headers.as_ref());
                }
            }
        }

        let models = parse_models(&config, &mut self.model_request_headers);
        CustomModelsResult {
            models,
            overrides,
            model_overrides,
            error: None,
        }
    }

    fn store_provider_request_config(&mut self, provider: &str, p: &ProviderConfig) {
        if p.api_key.is_none() && p.headers.is_none() && p.auth_header.is_none() {
            return;
        }
        self.provider_request_configs.insert(
            provider.to_string(),
            ProviderRequestConfig {
                api_key: p.api_key.clone(),
                headers: p.headers.clone(),
                auth_header: p.auth_header,
            },
        );
    }

    fn store_model_headers(
        &mut self,
        provider: &str,
        model_id: &str,
        headers: Option<&HashMap<String, String>>,
    ) {
        let key = format!("{provider}:{model_id}");
        match headers {
            Some(h) if !h.is_empty() => {
                self.model_request_headers.insert(key, h.clone());
            }
            _ => {
                self.model_request_headers.remove(&key);
            }
        }
    }
}

// =============================================================================
// Free helpers
// =============================================================================

#[derive(Debug, Default)]
struct CustomModelsResult {
    models: Vec<Model>,
    overrides: HashMap<String, ProviderOverride>,
    model_overrides: HashMap<String, HashMap<String, ModelOverride>>,
    error: Option<String>,
}

impl CustomModelsResult {
    fn with_error(error: String) -> Self {
        Self {
            error: Some(error),
            ..Self::default()
        }
    }
}

fn resort(models: &mut [Model]) {
    models.sort_by(|a, b| {
        (a.provider.as_str(), a.id.as_str()).cmp(&(b.provider.as_str(), b.id.as_str()))
    });
}

/// Merge custom models into `built_in` by `(provider, id)`. Custom wins.
fn merge_custom_models(mut built_in: Vec<Model>, custom: Vec<Model>) -> Vec<Model> {
    for c in custom {
        if let Some(existing) = built_in
            .iter_mut()
            .find(|m| m.provider == c.provider && m.id == c.id)
        {
            *existing = c;
        } else {
            built_in.push(c);
        }
    }
    built_in
}

/// Strip `//` line comments and trailing commas. Mirrors the TS
/// `stripJsonComments` helper. Quoted string literals are left untouched.
fn strip_json_comments(input: &str) -> String {
    // Two passes: first strip `//` line comments, then trailing commas.
    // Both passes scan character-by-character so they preserve string
    // contents verbatim.
    let no_line_comments = strip_line_comments(input);
    strip_trailing_commas(&no_line_comments)
}

fn strip_line_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' {
            out.push(c);
            // Read string body verbatim, honoring escapes.
            while let Some(&n) = chars.peek() {
                chars.next();
                out.push(n);
                if n == '\\' {
                    if let Some(&esc) = chars.peek() {
                        chars.next();
                        out.push(esc);
                    }
                } else if n == '"' {
                    break;
                }
            }
        } else if c == '/' && chars.peek() == Some(&'/') {
            // Drop until newline.
            while let Some(&n) = chars.peek() {
                if n == '\n' {
                    break;
                }
                chars.next();
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn strip_trailing_commas(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' {
            out.push(c);
            i += 1;
            while i < chars.len() {
                let n = chars[i];
                out.push(n);
                i += 1;
                if n == '\\' && i < chars.len() {
                    out.push(chars[i]);
                    i += 1;
                } else if n == '"' {
                    break;
                }
            }
            continue;
        }
        if c == ',' {
            // Look ahead skipping whitespace.
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && (chars[j] == '}' || chars[j] == ']') {
                // Drop the comma; emit the whitespace + closer next loop.
                i += 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

fn merge_compat(base: Option<&Compat>, override_: Option<&Compat>) -> Option<Compat> {
    match (base, override_) {
        (None, None) => None,
        (None, Some(o)) => Some(o.clone()),
        (Some(b), None) => Some(b.clone()),
        // The TS reference does a structural deep-merge across compat
        // shapes. Our `Compat` is a tagged enum and there's no general
        // "deep merge two enum variants" without flattening to JSON. For
        // mismatched variants (e.g. base=OpenAICompletions, override=
        // AnthropicMessages) we adopt the override wholesale, matching
        // the TS reference's "spread later wins" semantics. For the same
        // variant we still adopt the override — preserving the override's
        // value bag — because every field on `Compat` variants is itself
        // optional and the override carries the merge intent.
        (Some(_), Some(o)) => Some(o.clone()),
    }
}

fn apply_model_override(mut m: Model, o: &ModelOverride) -> Model {
    if let Some(name) = o.name.clone() {
        m.name = name;
    }
    if let Some(reasoning) = o.reasoning {
        m.reasoning = reasoning;
    }
    if let Some(map) = o.thinking_level_map.clone() {
        // Merge: override keys win, base keys persist.
        let mut merged = m.thinking_level_map.unwrap_or_default();
        for (k, v) in map {
            merged.insert(k, v);
        }
        m.thinking_level_map = Some(merged);
    }
    if let Some(input) = o.input.clone() {
        m.input = input;
    }
    if let Some(cw) = o.context_window {
        m.context_window = cw;
    }
    if let Some(mt) = o.max_tokens {
        m.max_tokens = mt;
    }
    if let Some(c) = o.cost.as_ref() {
        m.cost = Cost {
            input: c.input.unwrap_or(m.cost.input),
            output: c.output.unwrap_or(m.cost.output),
            cache_read: c.cache_read.unwrap_or(m.cost.cache_read),
            cache_write: c.cache_write.unwrap_or(m.cost.cache_write),
        };
    }
    if o.compat.is_some() {
        m.compat = merge_compat(m.compat.as_ref(), o.compat.as_ref());
    }
    m
}

fn validate_config(config: &ModelsConfig) -> Result<(), String> {
    let built_in_providers: std::collections::HashSet<String> =
        model::get_provider_keys().into_iter().collect();

    for (provider_name, p) in &config.providers {
        let is_built_in = built_in_providers.contains(provider_name);
        let has_provider_api = p.api.is_some();
        let models = p.models.as_deref().unwrap_or(&[]);
        let has_model_overrides = p
            .model_overrides
            .as_ref()
            .map(|m| !m.is_empty())
            .unwrap_or(false);

        if models.is_empty() {
            if p.base_url.is_none()
                && p.headers.is_none()
                && p.compat.is_none()
                && !has_model_overrides
            {
                return Err(format!(
                    "Provider {provider_name}: must specify \"baseUrl\", \"headers\", \"compat\", \"modelOverrides\", or \"models\"."
                ));
            }
        } else if !is_built_in {
            if p.base_url.is_none() {
                return Err(format!(
                    "Provider {provider_name}: \"baseUrl\" is required when defining custom models."
                ));
            }
            if p.api_key.is_none() {
                return Err(format!(
                    "Provider {provider_name}: \"apiKey\" is required when defining custom models."
                ));
            }
        }

        for m in models {
            let has_model_api = m.api.is_some();
            if !has_provider_api && !has_model_api && !is_built_in {
                return Err(format!(
                    "Provider {provider_name}, model {}: no \"api\" specified. Set at provider or model level.",
                    m.id
                ));
            }
            if m.id.is_empty() {
                return Err(format!("Provider {provider_name}: model missing \"id\""));
            }
            if let Some(cw) = m.context_window
                && cw == 0
            {
                return Err(format!(
                    "Provider {provider_name}, model {}: invalid contextWindow",
                    m.id
                ));
            }
            if let Some(mt) = m.max_tokens
                && mt == 0
            {
                return Err(format!(
                    "Provider {provider_name}, model {}: invalid maxTokens",
                    m.id
                ));
            }
        }
    }
    Ok(())
}

fn parse_models(
    config: &ModelsConfig,
    model_request_headers: &mut HashMap<String, HashMap<String, String>>,
) -> Vec<Model> {
    let mut out: Vec<Model> = Vec::new();
    let built_in_providers: std::collections::HashSet<String> =
        model::get_provider_keys().into_iter().collect();
    let mut built_in_defaults_cache: HashMap<String, Option<(String, String)>> = HashMap::new();

    for (provider_name, p) in &config.providers {
        let model_defs = p.models.as_deref().unwrap_or(&[]);
        if model_defs.is_empty() {
            continue;
        }

        let provider_enum = match model::types::Provider::from_str(provider_name) {
            Some(p) => p,
            None => continue, // unknown providers can't materialize Model<Api>
        };

        let built_in_defaults: Option<(String, String)> = built_in_defaults_cache
            .entry(provider_name.clone())
            .or_insert_with(|| {
                if !built_in_providers.contains(provider_name) {
                    return None;
                }
                let models = model::get_models(provider_name);
                let first = models.first()?;
                Some((api_to_str(first.api).to_string(), first.base_url.clone()))
            })
            .clone();

        for m in model_defs {
            let api_str = m
                .api
                .clone()
                .or_else(|| p.api.clone())
                .or_else(|| built_in_defaults.as_ref().map(|(a, _)| a.clone()));
            let Some(api_str) = api_str else { continue };
            let Some(api) = parse_api(Some(&api_str)) else {
                continue;
            };

            let base_url = m
                .base_url
                .clone()
                .or_else(|| p.base_url.clone())
                .or_else(|| built_in_defaults.as_ref().map(|(_, b)| b.clone()));
            let Some(base_url) = base_url else { continue };

            let compat = merge_compat(p.compat.as_ref(), m.compat.as_ref());
            let request_key = format!("{provider_name}:{}", m.id);
            if let Some(h) = m.headers.as_ref().filter(|h| !h.is_empty()) {
                model_request_headers.insert(request_key, h.clone());
            }

            let cost = m.cost.clone().unwrap_or(Cost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            });

            out.push(Model {
                id: m.id.clone(),
                name: m.name.clone().unwrap_or_else(|| m.id.clone()),
                api,
                provider: provider_enum,
                base_url,
                reasoning: m.reasoning.unwrap_or(false),
                input: m.input.clone().unwrap_or_else(|| vec![InputType::Text]),
                cost,
                context_window: m.context_window.unwrap_or(128_000),
                max_tokens: m.max_tokens.unwrap_or(16_384),
                headers: None,
                compat,
                thinking_level_map: m.thinking_level_map.clone(),
            });
        }
    }
    out
}

fn parse_api(s: Option<&str>) -> Option<model::Api> {
    use model::Api;
    Some(match s? {
        "openai-completions" => Api::OpenAICompletions,
        "openai-responses" => Api::OpenAIResponses,
        "azure-openai-responses" => Api::AzureOpenAiResponses,
        "openai-codex-responses" => Api::OpenAICodexResponses,
        "anthropic-messages" => Api::AnthropicMessages,
        "bedrock-converse-stream" => Api::BedrockConverseStream,
        "google-generative-ai" => Api::GoogleGenerativeAi,
        "google-gemini-cli" => Api::GoogleGeminiCli,
        "google-vertex" => Api::GoogleVertex,
        "mistral-conversations" => Api::MistralConversations,
        "faux" => Api::Faux,
        _ => return None,
    })
}

fn api_to_str(api: model::Api) -> &'static str {
    use model::Api;
    match api {
        Api::OpenAICompletions => "openai-completions",
        Api::OpenAIResponses => "openai-responses",
        Api::AzureOpenAiResponses => "azure-openai-responses",
        Api::OpenAICodexResponses => "openai-codex-responses",
        Api::AnthropicMessages => "anthropic-messages",
        Api::BedrockConverseStream => "bedrock-converse-stream",
        Api::GoogleGenerativeAi => "google-generative-ai",
        Api::GoogleGeminiCli => "google-gemini-cli",
        Api::GoogleVertex => "google-vertex",
        Api::MistralConversations => "mistral-conversations",
        Api::Faux => "faux",
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn fake_model(provider: model::types::Provider, id: &str) -> Model {
        Model {
            id: id.into(),
            name: id.into(),
            api: model::Api::AnthropicMessages,
            provider,
            base_url: String::new(),
            reasoning: false,
            input: vec![InputType::Text],
            cost: Cost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_window: 200_000,
            max_tokens: 8192,
            headers: None,
            compat: None,
            thinking_level_map: None,
        }
    }

    fn auth(dir: &TempDir) -> AuthStorage {
        AuthStorage::at(dir.path().join("auth.json"))
    }

    fn registry_with_models_json(dir: &TempDir, body: &str) -> ModelRegistry {
        let path = dir.path().join("models.json");
        fs::write(&path, body).unwrap();
        ModelRegistry::with_path(auth(dir), Some(path))
    }

    // ---------- legacy build(&Client) surface preserved ----------

    #[test]
    fn build_from_default_client_returns_non_empty_registry() {
        let client = model::Client::new();
        let registry = ModelRegistry::build(&client);
        assert!(
            !registry.is_empty(),
            "registry built from default client must surface the static catalog"
        );
    }

    #[test]
    fn build_iteration_order_is_stable() {
        let client = model::Client::new();
        let a = ModelRegistry::build(&client);
        let b = ModelRegistry::build(&client);
        assert_eq!(a.len(), b.len());
        let a_keys: Vec<_> = a
            .all()
            .iter()
            .map(|m| (m.provider.as_str(), m.id.as_str()))
            .collect();
        let b_keys: Vec<_> = b
            .all()
            .iter()
            .map(|m| (m.provider.as_str(), m.id.as_str()))
            .collect();
        assert_eq!(a_keys, b_keys);
    }

    #[test]
    fn find_returns_known_model_by_provider_and_id() {
        let client = model::Client::new();
        let registry = ModelRegistry::build(&client);
        let probe = registry.all().first().expect("registry non-empty").clone();
        let found = registry.find(probe.provider.as_str(), &probe.id).unwrap();
        assert_eq!(found.id, probe.id);
        assert_eq!(found.provider.as_str(), probe.provider.as_str());
    }

    #[test]
    fn find_missing_returns_none() {
        let client = model::Client::new();
        let registry = ModelRegistry::build(&client);
        assert!(registry.find("nonexistent", "nope").is_none());
    }

    #[test]
    fn next_cycles_through_all_models_and_wraps() {
        let client = model::Client::new();
        let registry = ModelRegistry::build(&client);
        assert!(registry.len() >= 2);
        let first = &registry.all()[0];
        let second = &registry.all()[1];
        let next = registry.next(first).unwrap();
        assert_eq!(next.id, second.id);
        let last = registry.all().last().unwrap();
        let wrapped = registry.next(last).unwrap();
        assert_eq!(wrapped.id, first.id);
    }

    #[test]
    fn next_with_unknown_current_returns_none() {
        let client = model::Client::new();
        let registry = ModelRegistry::build(&client);
        let phantom = fake_model(model::types::Provider::Anthropic, "definitely-not-real");
        assert!(registry.next(&phantom).is_none());
    }

    // ---------- in_memory / create ----------

    #[test]
    fn in_memory_does_not_touch_disk() {
        let dir = TempDir::new().unwrap();
        let registry = ModelRegistry::in_memory(auth(&dir));
        // No file written, no error.
        assert!(registry.error().is_none());
        assert!(
            !registry.is_empty(),
            "built-in models should still be present"
        );
    }

    #[test]
    fn create_with_missing_models_json_is_clean() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("models.json");
        let registry = ModelRegistry::with_path(auth(&dir), Some(path));
        assert!(registry.error().is_none());
        assert!(!registry.is_empty());
    }

    // ---------- comment / trailing-comma stripping ----------

    #[test]
    fn strip_json_comments_removes_line_comments() {
        let s = r#"{
          "providers": {
            // hello
            "openai": { "baseUrl": "http://x" }
          }
        }"#;
        let cleaned = strip_json_comments(s);
        assert!(!cleaned.contains("hello"));
        let v: serde_json::Value = serde_json::from_str(&cleaned).unwrap();
        assert!(v["providers"]["openai"].is_object());
    }

    #[test]
    fn strip_json_comments_removes_trailing_commas() {
        let s = r#"{ "a": [1, 2, 3,], "b": { "c": 1, } }"#;
        let cleaned = strip_json_comments(s);
        let v: serde_json::Value = serde_json::from_str(&cleaned).unwrap();
        assert_eq!(v["a"][2], 3);
        assert_eq!(v["b"]["c"], 1);
    }

    /// A `//` line comment sitting between a trailing comma and the
    /// closing brace must not hide the comma from the trailing-comma
    /// pass. The two-pass order (comments first, then trailing commas)
    /// guarantees this — a single-pass regex would have missed it.
    #[test]
    fn strip_json_comments_handles_comment_between_comma_and_closer() {
        let s = "{ \"a\": 1, // trailing\n}";
        let cleaned = strip_json_comments(s);
        let v: serde_json::Value =
            serde_json::from_str(&cleaned).expect("must parse as plain JSON after stripping");
        assert_eq!(v["a"], 1);
    }

    #[test]
    fn strip_json_comments_preserves_strings_with_slashes() {
        let s = r#"{ "url": "https://example.com/a//b" }"#;
        let cleaned = strip_json_comments(s);
        let v: serde_json::Value = serde_json::from_str(&cleaned).unwrap();
        assert_eq!(v["url"], "https://example.com/a//b");
    }

    // ---------- custom model loading ----------

    #[test]
    fn loads_custom_model_for_built_in_provider() {
        let dir = TempDir::new().unwrap();
        // Use a built-in provider so api/baseUrl can be inherited.
        let body = r#"{
          "providers": {
            "openai": {
              "models": [
                { "id": "gpt-mini-custom", "name": "GPT Mini Custom" }
              ]
            }
          }
        }"#;
        let registry = registry_with_models_json(&dir, body);
        assert!(
            registry.error().is_none(),
            "load error: {:?}",
            registry.error()
        );
        let custom = registry.find("openai", "gpt-mini-custom").unwrap();
        assert_eq!(custom.name, "GPT Mini Custom");
    }

    #[test]
    fn invalid_models_json_yields_error_but_keeps_built_ins() {
        let dir = TempDir::new().unwrap();
        let body = r#"{ "providers": "not-an-object" }"#;
        let registry = registry_with_models_json(&dir, body);
        assert!(registry.error().is_some());
        assert!(!registry.is_empty(), "built-ins must still load");
    }

    #[test]
    fn validate_rejects_custom_provider_without_base_url() {
        let dir = TempDir::new().unwrap();
        let body = r#"{
          "providers": {
            "totally-custom-xyz": {
              "models": [{ "id": "m", "api": "openai-completions" }]
            }
          }
        }"#;
        let registry = registry_with_models_json(&dir, body);
        let err = registry.error().expect("expected validation error");
        assert!(err.contains("baseUrl"), "got: {err}");
    }

    #[test]
    fn validate_rejects_override_only_provider_with_no_overrides() {
        let dir = TempDir::new().unwrap();
        let body = r#"{
          "providers": { "openai": {} }
        }"#;
        let registry = registry_with_models_json(&dir, body);
        let err = registry.error().expect("expected validation error");
        assert!(err.contains("must specify"), "got: {err}");
    }

    #[test]
    fn provider_override_replaces_base_url_for_built_in_models() {
        let dir = TempDir::new().unwrap();
        let body = r#"{
          "providers": {
            "openai": { "baseUrl": "http://override.local" }
          }
        }"#;
        let registry = registry_with_models_json(&dir, body);
        assert!(
            registry.error().is_none(),
            "load error: {:?}",
            registry.error()
        );
        // Pick any openai model and check the baseUrl was rewritten.
        let m = registry
            .all()
            .iter()
            .find(|m| m.provider.as_str() == "openai")
            .unwrap();
        assert_eq!(m.base_url, "http://override.local");
    }

    #[test]
    fn per_model_override_merges_partial_cost() {
        let dir = TempDir::new().unwrap();
        // Pick a real openai model id from the catalog at runtime.
        let some_openai_id = model::get_models("openai")
            .first()
            .expect("openai catalog non-empty")
            .id
            .clone();
        let body = format!(
            r#"{{
              "providers": {{
                "openai": {{
                  "modelOverrides": {{
                    "{id}": {{ "cost": {{ "input": 999.0 }}, "name": "Renamed" }}
                  }}
                }}
              }}
            }}"#,
            id = some_openai_id
        );
        let registry = registry_with_models_json(&dir, &body);
        assert!(
            registry.error().is_none(),
            "load error: {:?}",
            registry.error()
        );
        let m = registry.find("openai", &some_openai_id).unwrap();
        assert_eq!(m.cost.input, 999.0);
        assert_eq!(m.name, "Renamed");
        // Other cost fields preserved (probably non-zero in catalog).
        // Either way, output is the catalog default — not 999.
        assert_ne!(m.cost.output, 999.0);
    }

    #[test]
    fn merge_custom_models_overrides_built_in_by_provider_and_id() {
        let mut built_in = vec![
            fake_model(model::types::Provider::OpenAI, "x"),
            fake_model(model::types::Provider::Anthropic, "y"),
        ];
        built_in[0].name = "old".to_string();
        let mut custom = fake_model(model::types::Provider::OpenAI, "x");
        custom.name = "new".to_string();
        let merged = merge_custom_models(built_in, vec![custom]);
        assert_eq!(merged.len(), 2);
        let updated = merged.iter().find(|m| m.id == "x").unwrap();
        assert_eq!(updated.name, "new");
    }

    // ---------- auth-aware queries ----------

    #[test]
    fn has_configured_auth_via_auth_storage_api_key() {
        let dir = TempDir::new().unwrap();
        let storage = AuthStorage::at(dir.path().join("auth.json"));
        storage.set("openai", AuthRecord::api_key("sk-1")).unwrap();
        let registry = ModelRegistry::in_memory(storage);
        let m = registry
            .all()
            .iter()
            .find(|m| m.provider.as_str() == "openai")
            .unwrap();
        assert!(registry.has_configured_auth(m));
    }

    #[test]
    fn has_configured_auth_false_when_unset() {
        let dir = TempDir::new().unwrap();
        let registry = ModelRegistry::in_memory(auth(&dir));
        // Pick any provider that does not have an env var set.
        let m = fake_model(model::types::Provider::CloudflareWorkersAi, "cf-1");
        assert!(!registry.has_configured_auth(&m));
    }

    #[test]
    fn has_configured_auth_via_models_json_api_key() {
        let dir = TempDir::new().unwrap();
        // Built-in provider entries with no models need at least one of
        // `baseUrl/headers/compat/modelOverrides` — pair `apiKey` with a
        // header override so we exercise the apiKey-via-models.json path
        // rather than the auth-storage path.
        let body = r#"{
          "providers": {
            "openai": {
              "apiKey": "literal-key",
              "headers": { "X-Foo": "bar" }
            }
          }
        }"#;
        let registry = registry_with_models_json(&dir, body);
        assert!(
            registry.error().is_none(),
            "load error: {:?}",
            registry.error()
        );
        let m = registry
            .all()
            .iter()
            .find(|m| m.provider.as_str() == "openai")
            .unwrap();
        assert!(registry.has_configured_auth(m));
    }

    #[test]
    fn is_using_oauth_detects_oauth_record() {
        let dir = TempDir::new().unwrap();
        let storage = AuthStorage::at(dir.path().join("auth.json"));
        storage
            .set("anthropic", AuthRecord::oauth("a", "r", 1_700_000_000_000))
            .unwrap();
        let registry = ModelRegistry::in_memory(storage);
        let m = registry
            .all()
            .iter()
            .find(|m| m.provider.as_str() == "anthropic")
            .unwrap();
        assert!(registry.is_using_oauth(m));
    }

    #[test]
    fn is_using_oauth_false_for_api_key_record() {
        let dir = TempDir::new().unwrap();
        let storage = AuthStorage::at(dir.path().join("auth.json"));
        storage.set("openai", AuthRecord::api_key("sk-1")).unwrap();
        let registry = ModelRegistry::in_memory(storage);
        let m = registry
            .all()
            .iter()
            .find(|m| m.provider.as_str() == "openai")
            .unwrap();
        assert!(!registry.is_using_oauth(m));
    }

    /// Pi-mono parity: `is_anthropic_subscription_credential` flags an
    /// OAuth record under `anthropic` — the canonical Claude.ai
    /// subscription path.
    #[test]
    fn is_anthropic_subscription_credential_flags_oauth_under_anthropic() {
        let dir = TempDir::new().unwrap();
        let storage = AuthStorage::at(dir.path().join("auth.json"));
        storage
            .set("anthropic", AuthRecord::oauth("a", "r", 1_700_000_000_000))
            .unwrap();
        let registry = ModelRegistry::in_memory(storage);
        let m = registry
            .all()
            .iter()
            .find(|m| m.provider.as_str() == "anthropic")
            .unwrap()
            .clone();
        assert!(registry.is_anthropic_subscription_credential(&m));
    }

    /// Wider net than is_using_oauth: an `sk-ant-oat...` token pasted
    /// into the ApiKey slot also trips the warning. Real-world case —
    /// users frequently confuse subscription tokens with API keys.
    #[test]
    fn is_anthropic_subscription_credential_flags_oat_api_key() {
        let dir = TempDir::new().unwrap();
        let storage = AuthStorage::at(dir.path().join("auth.json"));
        storage
            .set("anthropic", AuthRecord::api_key("sk-ant-oat01-pasted"))
            .unwrap();
        let registry = ModelRegistry::in_memory(storage);
        let m = registry
            .all()
            .iter()
            .find(|m| m.provider.as_str() == "anthropic")
            .unwrap()
            .clone();
        assert!(registry.is_anthropic_subscription_credential(&m));
    }

    /// A legitimate API key (`sk-ant-api...`) does NOT trigger the
    /// warning even under anthropic.
    #[test]
    fn is_anthropic_subscription_credential_false_for_real_api_key() {
        let dir = TempDir::new().unwrap();
        let storage = AuthStorage::at(dir.path().join("auth.json"));
        storage
            .set("anthropic", AuthRecord::api_key("sk-ant-api03-legit"))
            .unwrap();
        let registry = ModelRegistry::in_memory(storage);
        let m = registry
            .all()
            .iter()
            .find(|m| m.provider.as_str() == "anthropic")
            .unwrap()
            .clone();
        assert!(!registry.is_anthropic_subscription_credential(&m));
    }

    /// Same OAuth record under a different provider does NOT trigger —
    /// other providers have legitimate OAuth flows and shouldn't be
    /// flagged as anthropic-subscription.
    #[test]
    fn is_anthropic_subscription_credential_scoped_to_anthropic() {
        let dir = TempDir::new().unwrap();
        let storage = AuthStorage::at(dir.path().join("auth.json"));
        storage
            .set("google", AuthRecord::oauth("a", "r", 1_700_000_000_000))
            .unwrap();
        let registry = ModelRegistry::in_memory(storage);
        let m = registry
            .all()
            .iter()
            .find(|m| m.provider.as_str() == "google")
            .unwrap()
            .clone();
        assert!(!registry.is_anthropic_subscription_credential(&m));
    }

    #[test]
    fn provider_display_name_uses_built_in_table() {
        let dir = TempDir::new().unwrap();
        let registry = ModelRegistry::in_memory(auth(&dir));
        assert_eq!(registry.provider_display_name("openai"), "OpenAI");
        assert_eq!(registry.provider_display_name("anthropic"), "Anthropic");
        assert_eq!(registry.provider_display_name("zai"), "ZAI");
    }

    #[test]
    fn provider_display_name_falls_back_to_raw_id_for_unknown() {
        let dir = TempDir::new().unwrap();
        let registry = ModelRegistry::in_memory(auth(&dir));
        assert_eq!(
            registry.provider_display_name("brand-new-xyz"),
            "brand-new-xyz"
        );
    }

    #[test]
    fn provider_auth_status_reports_stored() {
        let dir = TempDir::new().unwrap();
        let storage = AuthStorage::at(dir.path().join("auth.json"));
        storage.set("openai", AuthRecord::api_key("sk-1")).unwrap();
        let registry = ModelRegistry::in_memory(storage);
        let s = registry.provider_auth_status("openai");
        assert!(s.configured);
        assert_eq!(s.source, Some(AuthSource::Stored));
    }

    #[test]
    fn provider_auth_status_reports_models_json_command() {
        let dir = TempDir::new().unwrap();
        let body = r#"{
          "providers": {
            "totally-custom-xyz": {
              "baseUrl": "http://x",
              "apiKey": "!echo unused",
              "models": [
                { "id": "m", "api": "openai-completions" }
              ]
            }
          }
        }"#;
        let registry = registry_with_models_json(&dir, body);
        let s = registry.provider_auth_status("totally-custom-xyz");
        assert!(s.configured);
        assert_eq!(s.source, Some(AuthSource::ModelsJsonCommand));
    }

    #[test]
    fn api_key_for_provider_prefers_auth_storage() {
        let dir = TempDir::new().unwrap();
        let storage = AuthStorage::at(dir.path().join("auth.json"));
        storage
            .set("openai", AuthRecord::api_key("sk-stored"))
            .unwrap();
        // Add a `models.json` literal that should be ignored.
        let path = dir.path().join("models.json");
        fs::write(
            &path,
            r#"{"providers":{"openai":{"apiKey":"literal-key"}}}"#,
        )
        .unwrap();
        let registry = ModelRegistry::with_path(storage, Some(path));
        assert_eq!(
            registry.api_key_for_provider("openai").as_deref(),
            Some("sk-stored")
        );
    }

    #[test]
    fn api_key_for_provider_falls_back_to_models_json_literal() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("models.json");
        fs::write(
            &path,
            r#"{"providers":{"totally-custom-xyz":{"baseUrl":"http://x","apiKey":"literal-key","models":[{"id":"m","api":"openai-completions"}]}}}"#,
        )
        .unwrap();
        let registry = ModelRegistry::with_path(auth(&dir), Some(path));
        assert_eq!(
            registry
                .api_key_for_provider("totally-custom-xyz")
                .as_deref(),
            Some("literal-key"),
        );
    }

    #[test]
    fn api_key_and_headers_returns_none_for_no_auth_no_headers() {
        let dir = TempDir::new().unwrap();
        let registry = ModelRegistry::in_memory(auth(&dir));
        let m = fake_model(model::types::Provider::CloudflareWorkersAi, "cf-1");
        match registry.api_key_and_headers(&m) {
            ResolvedRequestAuth::Ok { api_key, headers } => {
                assert!(api_key.is_none());
                assert!(headers.is_none());
            }
            ResolvedRequestAuth::Err { reason } => {
                panic!("expected Ok, got Err({reason})")
            }
        }
    }

    #[test]
    fn api_key_and_headers_propagates_auth_storage_key() {
        let dir = TempDir::new().unwrap();
        let storage = AuthStorage::at(dir.path().join("auth.json"));
        storage
            .set("openai", AuthRecord::api_key("sk-stored"))
            .unwrap();
        let registry = ModelRegistry::in_memory(storage);
        let m = registry
            .all()
            .iter()
            .find(|m| m.provider.as_str() == "openai")
            .unwrap();
        match registry.api_key_and_headers(m) {
            ResolvedRequestAuth::Ok { api_key, .. } => {
                assert_eq!(api_key.as_deref(), Some("sk-stored"));
            }
            ResolvedRequestAuth::Err { reason } => panic!("got Err: {reason}"),
        }
    }

    #[test]
    fn api_key_and_headers_with_auth_header_emits_authorization() {
        let dir = TempDir::new().unwrap();
        // Use a provider that exists in the `Provider` enum (`opencode`) so
        // a custom model definition can materialize. Using a brand-new
        // provider id would be dropped silently by `parse_models` because
        // `Model.provider` is a fixed enum in this Rust port.
        let body = r#"{
          "providers": {
            "opencode": {
              "baseUrl": "http://x",
              "apiKey": "literal-key",
              "authHeader": true,
              "models": [{ "id": "m", "api": "openai-completions" }]
            }
          }
        }"#;
        let registry = registry_with_models_json(&dir, body);
        assert!(
            registry.error().is_none(),
            "load error: {:?}",
            registry.error()
        );
        let m = registry
            .find("opencode", "m")
            .expect("custom model")
            .clone();
        match registry.api_key_and_headers(&m) {
            ResolvedRequestAuth::Ok { api_key, headers } => {
                assert_eq!(api_key.as_deref(), Some("literal-key"));
                let h = headers.expect("headers");
                assert_eq!(
                    h.get("Authorization").map(String::as_str),
                    Some("Bearer literal-key")
                );
            }
            ResolvedRequestAuth::Err { reason } => panic!("got Err: {reason}"),
        }
    }

    #[test]
    fn api_key_and_headers_auth_header_without_key_fails() {
        let dir = TempDir::new().unwrap();
        // Use `opencode` (no env-var path in the env-key resolver) so the
        // test is deterministic regardless of host env. Pair `authHeader`
        // with a `headers` override so validate_config accepts the entry.
        let body = r#"{
          "providers": {
            "opencode": {
              "authHeader": true,
              "headers": { "X-Foo": "bar" }
            }
          }
        }"#;
        let registry = registry_with_models_json(&dir, body);
        assert!(
            registry.error().is_none(),
            "load error: {:?}",
            registry.error()
        );
        let m = fake_model(model::types::Provider::Opencode, "any");
        // If a real env var resolves for opencode in this host, skip. The
        // key resolver returns `None` for opencode in the standard table,
        // so this branch is rarely exercised.
        if model::env_api_keys::get_env_api_key_by_str("opencode").is_some() {
            return;
        }
        match registry.api_key_and_headers(&m) {
            ResolvedRequestAuth::Err { reason } => {
                assert!(reason.contains("No API key"), "got: {reason}");
            }
            ResolvedRequestAuth::Ok { .. } => panic!("expected Err"),
        }
    }

    // ---------- register / unregister provider ----------

    #[test]
    fn register_provider_with_models_replaces_provider_models() {
        let dir = TempDir::new().unwrap();
        let mut registry = ModelRegistry::in_memory(auth(&dir));
        let initial_openai_count = registry
            .all()
            .iter()
            .filter(|m| m.provider.as_str() == "openai")
            .count();
        assert!(
            initial_openai_count > 0,
            "openai catalog should be non-empty"
        );

        let cfg = ProviderConfigInput {
            base_url: Some("http://override.local".to_string()),
            api_key: Some("k".to_string()),
            api: Some("openai-completions".to_string()),
            models: Some(vec![ProviderConfigInputModel {
                id: "ext-1".into(),
                name: "Ext 1".into(),
                api: None,
                base_url: None,
                reasoning: false,
                thinking_level_map: None,
                input: vec![InputType::Text],
                cost: Cost {
                    input: 0.0,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                },
                context_window: 128_000,
                max_tokens: 8192,
                headers: None,
                compat: None,
            }]),
            ..Default::default()
        };
        registry.register_provider("openai", cfg).unwrap();

        let openai: Vec<_> = registry
            .all()
            .iter()
            .filter(|m| m.provider.as_str() == "openai")
            .collect();
        assert_eq!(openai.len(), 1, "register with models replaces existing");
        assert_eq!(openai[0].id, "ext-1");
        assert_eq!(openai[0].base_url, "http://override.local");
    }

    #[test]
    fn register_provider_without_models_overrides_base_url_only() {
        let dir = TempDir::new().unwrap();
        let mut registry = ModelRegistry::in_memory(auth(&dir));
        let cfg = ProviderConfigInput {
            base_url: Some("http://override.local".to_string()),
            ..Default::default()
        };
        registry.register_provider("openai", cfg).unwrap();
        let m = registry
            .all()
            .iter()
            .find(|m| m.provider.as_str() == "openai")
            .unwrap();
        assert_eq!(m.base_url, "http://override.local");
    }

    #[test]
    fn register_provider_rejects_models_without_base_url() {
        let dir = TempDir::new().unwrap();
        let mut registry = ModelRegistry::in_memory(auth(&dir));
        let cfg = ProviderConfigInput {
            api_key: Some("k".to_string()),
            api: Some("openai-completions".to_string()),
            models: Some(vec![ProviderConfigInputModel {
                id: "ext-1".into(),
                name: "Ext 1".into(),
                api: None,
                base_url: None,
                reasoning: false,
                thinking_level_map: None,
                input: vec![InputType::Text],
                cost: Cost {
                    input: 0.0,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                },
                context_window: 128_000,
                max_tokens: 8192,
                headers: None,
                compat: None,
            }]),
            ..Default::default()
        };
        let err = registry.register_provider("openai", cfg).unwrap_err();
        assert!(err.to_string().contains("base_url"), "got: {err}");
    }

    #[test]
    fn unregister_provider_restores_built_in_models() {
        let dir = TempDir::new().unwrap();
        let mut registry = ModelRegistry::in_memory(auth(&dir));
        let initial_openai_ids: std::collections::HashSet<String> = registry
            .all()
            .iter()
            .filter(|m| m.provider.as_str() == "openai")
            .map(|m| m.id.clone())
            .collect();

        let cfg = ProviderConfigInput {
            base_url: Some("http://x".to_string()),
            api_key: Some("k".to_string()),
            api: Some("openai-completions".to_string()),
            models: Some(vec![ProviderConfigInputModel {
                id: "ext-1".into(),
                name: "Ext 1".into(),
                api: None,
                base_url: None,
                reasoning: false,
                thinking_level_map: None,
                input: vec![InputType::Text],
                cost: Cost {
                    input: 0.0,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                },
                context_window: 128_000,
                max_tokens: 8192,
                headers: None,
                compat: None,
            }]),
            ..Default::default()
        };
        registry.register_provider("openai", cfg).unwrap();
        registry.unregister_provider("openai");

        let restored_openai_ids: std::collections::HashSet<String> = registry
            .all()
            .iter()
            .filter(|m| m.provider.as_str() == "openai")
            .map(|m| m.id.clone())
            .collect();
        assert_eq!(initial_openai_ids, restored_openai_ids);
    }

    #[test]
    fn unregister_unknown_provider_is_noop() {
        let dir = TempDir::new().unwrap();
        let mut registry = ModelRegistry::in_memory(auth(&dir));
        let len_before = registry.len();
        registry.unregister_provider("never-registered-xyz");
        assert_eq!(registry.len(), len_before);
    }

    #[test]
    fn refresh_replays_registered_providers() {
        let dir = TempDir::new().unwrap();
        let mut registry = ModelRegistry::in_memory(auth(&dir));
        let cfg = ProviderConfigInput {
            base_url: Some("http://override.local".to_string()),
            ..Default::default()
        };
        registry.register_provider("openai", cfg).unwrap();
        registry.refresh();
        let m = registry
            .all()
            .iter()
            .find(|m| m.provider.as_str() == "openai")
            .unwrap();
        assert_eq!(
            m.base_url, "http://override.local",
            "refresh must replay registered providers",
        );
    }
}
