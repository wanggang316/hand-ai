//! Core OAuth types shared across providers.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Identifier for a built-in OAuth provider.
///
/// Variants serialize as the same kebab-case slugs the TypeScript reference
/// uses, so credentials persisted by either implementation interoperate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OAuthProviderId {
    #[serde(rename = "anthropic")]
    Anthropic,
    #[serde(rename = "openai-codex")]
    OpenAICodex,
    #[serde(rename = "github-copilot")]
    GithubCopilot,
}

impl OAuthProviderId {
    /// Stable string slug used for serialization and CLI args.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAICodex => "openai-codex",
            Self::GithubCopilot => "github-copilot",
        }
    }
}

/// Persisted OAuth credentials.
///
/// Field semantics intentionally mirror the TS reference: `access_token` is
/// the bearer token used in API requests, `refresh_token` (when present) lets
/// us obtain a new access token without re-authenticating, and `expires_at`
/// is the Unix epoch in **milliseconds** at which the access token expires.
///
/// `Debug` is implemented manually so accidental `{:?}` formatting (logs,
/// panics, `dbg!`) does not leak token material — both tokens are replaced
/// with the literal `<redacted>`.
#[derive(Clone, Serialize, Deserialize)]
pub struct OAuthCredentials {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
}

impl std::fmt::Debug for OAuthCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthCredentials")
            .field("access_token", &"<redacted>")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("expires_at", &self.expires_at)
            .field("scope", &self.scope)
            .field("extra", &self.extra)
            .finish()
    }
}

/// Persisted authentication record (credentials + metadata).
///
/// `Debug` is implemented manually so the embedded credentials are printed
/// via the redacted impl above.
#[derive(Clone, Serialize, Deserialize)]
pub struct OAuthAuthInfo {
    pub provider_id: OAuthProviderId,
    pub credentials: OAuthCredentials,
    pub created_at_ms: u64,
}

impl std::fmt::Debug for OAuthAuthInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthAuthInfo")
            .field("provider_id", &self.provider_id)
            .field("credentials", &self.credentials)
            .field("created_at_ms", &self.created_at_ms)
            .finish()
    }
}

/// Errors raised by the OAuth subsystem.
#[derive(Debug, Error)]
pub enum OAuthError {
    #[error("user cancelled the OAuth flow")]
    UserCancelled,
    #[error("OAuth provider {0:?} returned an error: {1}")]
    ProviderError(OAuthProviderId, String),
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("token refresh failed: {0}")]
    RefreshFailed(String),
    #[error("invalid credentials format: {0}")]
    InvalidCredentials(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Callbacks invoked by provider login flows to interact with the user.
///
/// These are intentionally synchronous fn-pointers so callers can pass
/// closures that print to stdout (CLI) or update a UI surface. Long-running
/// flows (loopback servers, device-code polling) live inside the provider.
/// Callback invoked with a URL the user should open in a browser.
pub type OnOpenUrl = Box<dyn Fn(&str) + Send + Sync>;
/// Callback invoked with `(user_code, verification_url)` for device flows.
pub type OnDeviceCode = Box<dyn Fn(&str, &str) + Send + Sync>;

pub struct OAuthLoginCallbacks {
    /// Print or display a URL to the user (browser flow).
    pub on_open_url: OnOpenUrl,
    /// Print device-flow code to the user.
    pub on_device_code: OnDeviceCode,
}

impl OAuthLoginCallbacks {
    /// Build callbacks that print to stderr — convenient default for CLI use.
    pub fn stderr() -> Self {
        Self {
            on_open_url: Box::new(|url| eprintln!("Open this URL in your browser:\n  {url}")),
            on_device_code: Box::new(|user_code, verification_url| {
                eprintln!("Visit {verification_url} and enter the code: {user_code}");
            }),
        }
    }
}

/// Trait implemented by every OAuth provider.
#[async_trait]
pub trait OAuthProvider: Send + Sync {
    /// Stable provider identifier.
    fn id(&self) -> OAuthProviderId;

    /// Run the interactive login flow and return persisted credentials.
    async fn login(&self, callbacks: &OAuthLoginCallbacks) -> Result<OAuthCredentials, OAuthError>;

    /// Refresh an access token using the persisted refresh token.
    async fn refresh(&self, creds: &OAuthCredentials) -> Result<OAuthCredentials, OAuthError>;

    /// Revoke credentials at the provider. Default impl is a no-op because
    /// most providers do not expose a public revoke endpoint.
    async fn revoke(&self, _creds: &OAuthCredentials) -> Result<(), OAuthError> {
        Ok(())
    }

    /// Returns true when the access token is expired or within 60 seconds of
    /// expiry. Treating tokens as expired with a buffer avoids races where a
    /// long-running request fails mid-flight.
    fn is_expired(&self, creds: &OAuthCredentials) -> bool {
        if let Some(exp) = creds.expires_at {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            now + 60_000 >= exp
        } else {
            false
        }
    }
}
