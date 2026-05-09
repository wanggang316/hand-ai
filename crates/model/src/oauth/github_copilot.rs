//! GitHub Copilot OAuth — Device Flow.
//!
//! Mirrors `pi-mono/.../oauth/github-copilot.ts`. Differences from the
//! browser-redirect flows:
//!
//! - No loopback server; we exchange a `device_code` for an access token by
//!   polling.
//! - The GitHub access token is a **refresh token** for our purposes; the
//!   actual short-lived Copilot access token is fetched from
//!   `api.github.com/copilot_internal/v2/token` and stored in `access_token`.
//! - Optional enterprise domain support is preserved via `extra`.

use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use serde_json::json;

use crate::models::get_models;

use super::types::{
    OAuthCredentials, OAuthError, OAuthLoginCallbacks, OAuthProvider, OAuthProviderId,
};

// CLIENT_ID is base64-obfuscated upstream; preserve that.
const CLIENT_ID_B64: &str = "SXYxLmI1MDdhMDhjODdlY2ZlOTg=";

const COPILOT_USER_AGENT: &str = "GitHubCopilotChat/0.35.0";
const COPILOT_EDITOR_VERSION: &str = "vscode/1.107.0";
const COPILOT_EDITOR_PLUGIN_VERSION: &str = "copilot-chat/0.35.0";
const COPILOT_INTEGRATION_ID: &str = "vscode-chat";

const INITIAL_POLL_INTERVAL_MULTIPLIER: f64 = 1.2;
const SLOW_DOWN_POLL_INTERVAL_MULTIPLIER: f64 = 1.4;

fn client_id() -> String {
    let bytes = STANDARD
        .decode(CLIENT_ID_B64)
        .expect("hard-coded base64 client id is well-formed");
    String::from_utf8(bytes).expect("hard-coded client id is utf-8")
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    interval: u64,
    expires_in: u64,
}

#[derive(Debug, Deserialize)]
struct DeviceTokenResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
    #[serde(default)]
    interval: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct CopilotTokenResponse {
    token: String,
    expires_at: u64,
}

/// Bundle of GitHub endpoints used by the device-flow login. The default is
/// `github.com`; enterprise users (and tests) can override it.
#[derive(Debug, Clone)]
pub struct GithubEndpoints {
    pub device_code_url: String,
    pub access_token_url: String,
    pub copilot_token_url: String,
}

impl GithubEndpoints {
    pub fn for_domain(domain: &str) -> Self {
        Self {
            device_code_url: format!("https://{domain}/login/device/code"),
            access_token_url: format!("https://{domain}/login/oauth/access_token"),
            copilot_token_url: format!("https://api.{domain}/copilot_internal/v2/token"),
        }
    }
}

impl Default for GithubEndpoints {
    fn default() -> Self {
        Self::for_domain("github.com")
    }
}

pub struct GithubCopilotOAuthProvider {
    http: reqwest::Client,
    endpoints: GithubEndpoints,
}

impl GithubCopilotOAuthProvider {
    pub fn new() -> Self {
        Self::with_endpoints(GithubEndpoints::default())
    }

    /// Construct a provider with custom endpoints — the device-code, access-
    /// token, and Copilot-token URLs. Production code uses [`Self::new`];
    /// this lets tests point the provider at a local mock server.
    pub fn with_endpoints(endpoints: GithubEndpoints) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("default reqwest client"),
            endpoints,
        }
    }
}

impl Default for GithubCopilotOAuthProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl OAuthProvider for GithubCopilotOAuthProvider {
    fn id(&self) -> OAuthProviderId {
        OAuthProviderId::GithubCopilot
    }

    async fn login(&self, callbacks: &OAuthLoginCallbacks) -> Result<OAuthCredentials, OAuthError> {
        let device = start_device_flow(&self.http, &self.endpoints.device_code_url).await?;

        (callbacks.on_device_code)(&device.user_code, &device.verification_uri);

        let github_access_token = poll_for_access_token(
            &self.http,
            &self.endpoints.access_token_url,
            &device.device_code,
            device.interval,
            device.expires_in,
        )
        .await?;

        // Exchange the GitHub access token for a Copilot session token.
        let creds = exchange_for_copilot_token(
            &self.http,
            &self.endpoints.copilot_token_url,
            &github_access_token,
            None,
        )
        .await?;
        // Best-effort: enable all known GitHub Copilot models on the user's
        // account. Mirrors the TS reference, which fires this off after every
        // successful login. We spawn it as a detached task so a slow policy
        // endpoint does not stall the login return; failures are silently
        // swallowed.
        let http = self.http.clone();
        let token = creds.access_token.clone();
        tokio::spawn(async move {
            enable_all_github_copilot_models(&http, &token, None).await;
        });
        Ok(creds)
    }

    async fn refresh(&self, creds: &OAuthCredentials) -> Result<OAuthCredentials, OAuthError> {
        let refresh = creds.refresh_token.as_deref().ok_or_else(|| {
            OAuthError::RefreshFailed("github-copilot credentials missing refresh_token".into())
        })?;
        let enterprise = creds
            .extra
            .as_ref()
            .and_then(|v| v.get("enterprise_domain"))
            .and_then(|v| v.as_str());
        let copilot_url = match enterprise {
            Some(d) => GithubEndpoints::for_domain(d).copilot_token_url,
            None => self.endpoints.copilot_token_url.clone(),
        };
        exchange_for_copilot_token(&self.http, &copilot_url, refresh, enterprise).await
    }
}

async fn start_device_flow(
    http: &reqwest::Client,
    device_code_url: &str,
) -> Result<DeviceCodeResponse, OAuthError> {
    let cid = client_id();
    let form = [("client_id", cid.as_str()), ("scope", "read:user")];
    let resp = http
        .post(device_code_url)
        .header("Accept", "application/json")
        .header("User-Agent", COPILOT_USER_AGENT)
        .form(&form)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        // Status-only — error bodies can leak tokens.
        return Err(OAuthError::ProviderError(
            OAuthProviderId::GithubCopilot,
            format!("device_code request failed: HTTP {status}"),
        ));
    }
    let parsed: DeviceCodeResponse = serde_json::from_str(&text)?;
    Ok(parsed)
}

async fn poll_for_access_token(
    http: &reqwest::Client,
    access_token_url: &str,
    device_code: &str,
    interval_seconds: u64,
    expires_in: u64,
) -> Result<String, OAuthError> {
    let cid = client_id();

    let deadline = std::time::Instant::now() + Duration::from_secs(expires_in);
    let mut interval_ms = std::cmp::max(1000, interval_seconds.saturating_mul(1000));
    let mut multiplier = INITIAL_POLL_INTERVAL_MULTIPLIER;
    let mut slow_down_count = 0u32;

    while std::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let wait = std::cmp::min(
            Duration::from_millis(((interval_ms as f64) * multiplier).ceil() as u64),
            remaining,
        );
        tokio::time::sleep(wait).await;

        let form = [
            ("client_id", cid.as_str()),
            ("device_code", device_code),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ];
        let resp = http
            .post(access_token_url)
            .header("Accept", "application/json")
            .header("User-Agent", COPILOT_USER_AGENT)
            .form(&form)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            // Body may include the freshly minted access token in some
            // error responses — keep the error status-only.
            return Err(OAuthError::ProviderError(
                OAuthProviderId::GithubCopilot,
                format!("device token request failed: HTTP {status}"),
            ));
        }
        let parsed: DeviceTokenResponse = serde_json::from_str(&text)?;

        if let Some(token) = parsed.access_token {
            return Ok(token);
        }

        match parsed.error.as_deref() {
            Some("authorization_pending") => continue,
            Some("slow_down") => {
                slow_down_count += 1;
                interval_ms = match parsed.interval {
                    Some(i) if i > 0 => i.saturating_mul(1000),
                    _ => interval_ms.saturating_add(5_000),
                };
                multiplier = SLOW_DOWN_POLL_INTERVAL_MULTIPLIER;
            }
            Some(other) => {
                let suffix = parsed
                    .error_description
                    .map(|d| format!(": {d}"))
                    .unwrap_or_default();
                return Err(OAuthError::ProviderError(
                    OAuthProviderId::GithubCopilot,
                    format!("device flow failed: {other}{suffix}"),
                ));
            }
            None => {
                // Body could echo a token if the schema ever drifts; surface
                // the failure without the body content.
                return Err(OAuthError::ProviderError(
                    OAuthProviderId::GithubCopilot,
                    "unexpected device token response shape".into(),
                ));
            }
        }
    }

    if slow_down_count > 0 {
        Err(OAuthError::ProviderError(
            OAuthProviderId::GithubCopilot,
            "device flow timed out after slow_down responses; check system clock drift".into(),
        ))
    } else {
        Err(OAuthError::ProviderError(
            OAuthProviderId::GithubCopilot,
            "device flow timed out".into(),
        ))
    }
}

async fn exchange_for_copilot_token(
    http: &reqwest::Client,
    copilot_token_url: &str,
    github_access_token: &str,
    enterprise_domain: Option<&str>,
) -> Result<OAuthCredentials, OAuthError> {
    let resp = http
        .get(copilot_token_url)
        .header("Accept", "application/json")
        .header("Authorization", format!("Bearer {github_access_token}"))
        .header("User-Agent", COPILOT_USER_AGENT)
        .header("Editor-Version", COPILOT_EDITOR_VERSION)
        .header("Editor-Plugin-Version", COPILOT_EDITOR_PLUGIN_VERSION)
        .header("Copilot-Integration-Id", COPILOT_INTEGRATION_ID)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        // Status-only — error bodies can leak tokens.
        return Err(OAuthError::ProviderError(
            OAuthProviderId::GithubCopilot,
            format!("copilot token request failed: HTTP {status}"),
        ));
    }
    let parsed: CopilotTokenResponse = serde_json::from_str(&text)?;

    // expires_at is seconds; subtract a 5-minute safety window to mirror TS.
    let expires_at_ms = parsed
        .expires_at
        .saturating_mul(1000)
        .saturating_sub(5 * 60 * 1000);

    let extra = enterprise_domain.map(|d| json!({ "enterprise_domain": d }));

    Ok(OAuthCredentials {
        access_token: parsed.token,
        refresh_token: Some(github_access_token.to_string()),
        expires_at: Some(expires_at_ms),
        scope: None,
        extra,
    })
}

/// Normalize a free-form GitHub Enterprise URL or domain into a hostname.
///
/// Returns `None` for empty input or values we cannot parse as a URL.
/// Mirrors `normalizeDomain` in the TS reference.
pub fn normalize_domain(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    // The TS reference uses `new URL(...)`. We mimic that by stripping a
    // leading scheme (anything before `://`), then taking the host portion
    // up to the first `/`, `?`, or `#`.
    let after_scheme = match trimmed.find("://") {
        Some(idx) => &trimmed[idx + 3..],
        None => trimmed,
    };
    if after_scheme.is_empty() {
        return None;
    }
    // Strip optional userinfo (`user:pass@host`).
    let after_userinfo = match after_scheme.rfind('@') {
        Some(idx) => &after_scheme[idx + 1..],
        None => after_scheme,
    };
    // Take up to the first path/query/fragment delimiter.
    let host_end = after_userinfo
        .find(['/', '?', '#'])
        .unwrap_or(after_userinfo.len());
    let host_with_port = &after_userinfo[..host_end];
    if host_with_port.is_empty() {
        return None;
    }
    // Drop an explicit port, if any.
    let host = match host_with_port.rfind(':') {
        Some(idx) => &host_with_port[..idx],
        None => host_with_port,
    };
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}

/// Build the GitHub Copilot API base URL.
///
/// Tokens issued by the Copilot token endpoint contain a `proxy-ep=...`
/// segment whose value points at the proxy host (e.g.
/// `proxy.individual.githubcopilot.com`). The API host is the same domain
/// with `proxy.` swapped for `api.`.
///
/// If the token does not carry a `proxy-ep` and an enterprise domain is
/// supplied, fall back to `https://copilot-api.<enterprise>`. Otherwise
/// return the documented public default `https://api.individual.githubcopilot.com`.
///
/// Mirrors `getGitHubCopilotBaseUrl` in the TS reference.
pub fn github_copilot_base_url(token: Option<&str>, enterprise_domain: Option<&str>) -> String {
    if let Some(t) = token
        && let Some(url) = base_url_from_token(t)
    {
        return url;
    }
    if let Some(d) = enterprise_domain {
        return format!("https://copilot-api.{d}");
    }
    "https://api.individual.githubcopilot.com".to_string()
}

/// Pull the `proxy-ep=...` segment out of a Copilot token and translate it
/// into an `https://api.<host>` URL, or `None` if the segment is absent.
fn base_url_from_token(token: &str) -> Option<String> {
    let needle = "proxy-ep=";
    let start = token.find(needle)? + needle.len();
    let rest = &token[start..];
    let end = rest.find(';').unwrap_or(rest.len());
    let proxy_host = &rest[..end];
    if proxy_host.is_empty() {
        return None;
    }
    // `proxy.foo.bar` -> `api.foo.bar`. If there's no `proxy.` prefix, leave
    // the host as-is (matches the TS regex `^proxy\.`).
    let api_host = proxy_host
        .strip_prefix("proxy.")
        .map(|tail| format!("api.{tail}"))
        .unwrap_or_else(|| proxy_host.to_string());
    Some(format!("https://{api_host}"))
}

/// Best-effort: ask the Copilot policy endpoint to enable every known
/// `github-copilot` model on the user's account. Some models (Claude, Grok)
/// require this before they can be invoked.
///
/// Errors are intentionally swallowed: this is fired after a successful
/// login and we do not want a transient policy failure to surface as a
/// login error.
async fn enable_all_github_copilot_models(
    http: &reqwest::Client,
    token: &str,
    enterprise_domain: Option<&str>,
) {
    let base_url = github_copilot_base_url(Some(token), enterprise_domain);
    let models = get_models("github-copilot");
    let mut futs = Vec::with_capacity(models.len());
    for model in models {
        let model_id = model.id;
        let url = format!("{base_url}/models/{model_id}/policy");
        let req = http
            .post(&url)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {token}"))
            .header("User-Agent", COPILOT_USER_AGENT)
            .header("Editor-Version", COPILOT_EDITOR_VERSION)
            .header("Editor-Plugin-Version", COPILOT_EDITOR_PLUGIN_VERSION)
            .header("Copilot-Integration-Id", COPILOT_INTEGRATION_ID)
            .header("openai-intent", "chat-policy")
            .header("x-interaction-type", "chat-policy")
            .json(&json!({ "state": "enabled" }))
            .send();
        futs.push(req);
    }
    let _ = futures::future::join_all(futs).await;
}
