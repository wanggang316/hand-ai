//! Google Vertex AI provider.
//!
//! Implements streaming chat completions for the Vertex AI generative-content
//! endpoint. Unlike Google Generative AI (Gemini API), Vertex AI authenticates
//! either via an explicit API key (`?key=…`) or, more commonly, via Google
//! Cloud Application Default Credentials (ADC) — `gcloud auth
//! application-default login` — in which case the request carries a Bearer
//! access token and the URL is scoped to a project + location.
//!
//! The wire format (request body, SSE events, content blocks) is identical
//! to Google Generative AI; that logic lives in `google_shared` and is
//! reused here.

use crate::api_registry::AssistantMessageEventStream;
use crate::env_api_keys;
use crate::providers::google_shared::{
    self, GoogleThinkingLevel as SharedGoogleThinkingLevel, SharedGoogleOptions,
};
use crate::types::{
    Api, AssistantMessageEvent, Context, Model, SimpleStreamOptions, StreamOptions,
};
use futures::future::BoxFuture;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use std::sync::Arc;

/// Marker the TS reference uses to indicate "auth handled out-of-band". Any
/// option/api_key matching this string is ignored and ADC is used instead.
const GCP_VERTEX_CREDENTIALS_MARKER: &str = "gcp-vertex-credentials";

/// Async function that yields a Vertex access token. Used as a test seam to
/// avoid depending on real `gcloud` ADC during unit tests.
pub type VertexTokenProvider =
    Arc<dyn Fn() -> BoxFuture<'static, Result<String, String>> + Send + Sync + 'static>;

/// Vertex-specific stream options.
#[derive(Clone, Default)]
pub struct GoogleVertexOptions {
    pub base: StreamOptions,
    /// Optional GCP project override. If unset, falls back to the
    /// `GOOGLE_CLOUD_PROJECT` / `GCLOUD_PROJECT` environment variables.
    pub project: Option<String>,
    /// Optional location override (e.g. `"us-central1"`). If unset, falls
    /// back to the `GOOGLE_CLOUD_LOCATION` environment variable.
    pub location: Option<String>,
    /// Tool-choice mode (`"auto"`, `"none"`, `"any"`). Mirrors the Gemini
    /// `toolConfig.functionCallingConfig.mode` field.
    pub tool_choice: Option<String>,
    pub thinking_enabled: bool,
    pub thinking_budget_tokens: Option<i32>,
    pub thinking_level: Option<GoogleVertexThinkingLevel>,
}

impl std::fmt::Debug for GoogleVertexOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoogleVertexOptions")
            .field("base", &self.base)
            .field("project", &self.project)
            .field("location", &self.location)
            .field("tool_choice", &self.tool_choice)
            .field("thinking_enabled", &self.thinking_enabled)
            .field("thinking_budget_tokens", &self.thinking_budget_tokens)
            .field("thinking_level", &self.thinking_level)
            .finish()
    }
}

/// Public mirror of `google_shared::GoogleThinkingLevel` so this provider's
/// option type stays self-contained.
#[derive(Debug, Clone, Copy)]
pub enum GoogleVertexThinkingLevel {
    Minimal,
    Low,
    Medium,
    High,
}

impl GoogleVertexThinkingLevel {
    fn to_shared(self) -> SharedGoogleThinkingLevel {
        match self {
            GoogleVertexThinkingLevel::Minimal => SharedGoogleThinkingLevel::Minimal,
            GoogleVertexThinkingLevel::Low => SharedGoogleThinkingLevel::Low,
            GoogleVertexThinkingLevel::Medium => SharedGoogleThinkingLevel::Medium,
            GoogleVertexThinkingLevel::High => SharedGoogleThinkingLevel::High,
        }
    }
}

fn shared_to_public(level: SharedGoogleThinkingLevel) -> GoogleVertexThinkingLevel {
    match level {
        SharedGoogleThinkingLevel::Minimal => GoogleVertexThinkingLevel::Minimal,
        SharedGoogleThinkingLevel::Low => GoogleVertexThinkingLevel::Low,
        SharedGoogleThinkingLevel::Medium => GoogleVertexThinkingLevel::Medium,
        SharedGoogleThinkingLevel::High => GoogleVertexThinkingLevel::High,
    }
}

impl GoogleVertexOptions {
    fn into_shared(self) -> SharedGoogleOptions {
        SharedGoogleOptions {
            base: self.base,
            tool_choice: self.tool_choice,
            thinking_enabled: self.thinking_enabled,
            thinking_budget_tokens: self.thinking_budget_tokens,
            thinking_level: self
                .thinking_level
                .map(GoogleVertexThinkingLevel::to_shared),
        }
    }
}

// =============================================================================
// Provider
// =============================================================================

/// Provider implementation for the Vertex AI streamGenerateContent endpoint.
#[derive(Clone)]
pub struct GoogleVertexProvider {
    client: reqwest::Client,
    /// Test seam: fetch the ADC access token through this hook instead of
    /// reading `application_default_credentials.json`. When `None`, the
    /// provider falls back to `env_api_keys::vertex_access_token`.
    token_provider: Option<VertexTokenProvider>,
    /// Optional base-URL override for tests. When set, this string replaces
    /// the canonical `https://{location}-aiplatform.googleapis.com` host.
    base_url_override: Option<String>,
}

impl std::fmt::Debug for GoogleVertexProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoogleVertexProvider")
            .field("token_provider", &self.token_provider.is_some())
            .field("base_url_override", &self.base_url_override)
            .finish()
    }
}

impl Default for GoogleVertexProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl GoogleVertexProvider {
    /// Construct a new provider using a default `reqwest::Client`.
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            token_provider: None,
            base_url_override: None,
        }
    }

    /// Construct a new provider using the supplied HTTP client.
    pub fn with_client(client: reqwest::Client) -> Self {
        Self {
            client,
            token_provider: None,
            base_url_override: None,
        }
    }

    /// Test seam: install a custom token provider. When set, the provider
    /// uses this hook instead of the real ADC fetcher.
    pub fn with_token_provider(mut self, provider: VertexTokenProvider) -> Self {
        self.token_provider = Some(provider);
        self
    }

    /// Test seam: override the base URL used to build the request endpoint.
    /// When set, the URL becomes
    /// `<override>/v1/projects/{project}/locations/{location}/publishers/google/models/{model}:streamGenerateContent`.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url_override = Some(base_url.into());
        self
    }
}

impl crate::api_registry::ApiProvider for GoogleVertexProvider {
    fn stream(
        &self,
        model: Model,
        context: Context,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        let vertex_options = GoogleVertexOptions {
            base: options.unwrap_or_default(),
            ..Default::default()
        };
        Box::pin(stream_vertex(
            self.client.clone(),
            self.token_provider.clone(),
            self.base_url_override.clone(),
            model,
            context,
            vertex_options,
        ))
    }

    fn stream_simple(
        &self,
        model: Model,
        context: Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        let vertex_options = build_vertex_options(&model, options.as_ref());
        Box::pin(stream_vertex(
            self.client.clone(),
            self.token_provider.clone(),
            self.base_url_override.clone(),
            model,
            context,
            vertex_options,
        ))
    }
}

fn build_vertex_options(
    model: &Model,
    options: Option<&SimpleStreamOptions>,
) -> GoogleVertexOptions {
    let (base, reasoning) = match options {
        Some(opts) => {
            let api_key = env_api_keys::get_env_api_key(&model.provider);
            let base = opts.build_base_options(model, api_key);
            (base, opts.clamp_reasoning())
        }
        None => (StreamOptions::default(), None),
    };

    let mut vertex_opts = GoogleVertexOptions {
        base,
        ..Default::default()
    };

    match reasoning {
        None => {
            vertex_opts.thinking_enabled = false;
        }
        Some(effort) => {
            vertex_opts.thinking_enabled = true;

            if google_shared::is_gemini3_pro_model(&model.id)
                || google_shared::is_gemini3_flash_model(&model.id)
            {
                vertex_opts.thinking_level = Some(shared_to_public(
                    google_shared::get_gemini3_thinking_level(effort, &model.id),
                ));
            } else {
                vertex_opts.thinking_budget_tokens = Some(google_shared::get_google_budget(
                    &model.id,
                    effort,
                    options.and_then(|o| o.thinking_budgets.as_ref()),
                ));
            }
        }
    }

    vertex_opts
}

// =============================================================================
// Streaming
// =============================================================================

fn stream_vertex(
    client: reqwest::Client,
    token_provider: Option<VertexTokenProvider>,
    base_url_override: Option<String>,
    model: Model,
    context: Context,
    options: GoogleVertexOptions,
) -> impl futures::Stream<Item = AssistantMessageEvent> + Send + 'static {
    async_stream::stream! {
        // Emit `Start` unconditionally so consumers always see
        // `Start -> ... -> (Done | Error)` — including on early failure paths
        // (auth, network, ADC token fetch) where SSE never opens.
        // `parse_sse_stream` does NOT emit its own `Start`.
        let initial = crate::types::AssistantMessage {
            role: "assistant".to_string(),
            content: vec![],
            api: Api::GoogleVertex,
            provider: model.provider,
            model: model.id.clone(),
            usage: crate::types::Usage::default(),
            stop_reason: crate::types::StopReason::Stop,
            error_message: None,
            timestamp: google_shared::current_timestamp_ms(),
            response_model: None,
            response_id: None,
            diagnostics: None,
        };
        yield AssistantMessageEvent::Start { partial: initial.clone() };

        let result = stream_vertex_inner(
            client,
            token_provider,
            base_url_override,
            model,
            context,
            options,
        )
        .await;
        match result {
            Ok(events) => {
                for event in events {
                    yield event;
                }
            }
            Err(e) => {
                let mut error_msg = initial;
                error_msg.stop_reason = crate::types::StopReason::Error;
                error_msg.error_message = Some(e);
                yield AssistantMessageEvent::Error {
                    reason: crate::types::StopReason::Error,
                    error: error_msg,
                };
            }
        }
    }
}

async fn stream_vertex_inner(
    client: reqwest::Client,
    token_provider: Option<VertexTokenProvider>,
    base_url_override: Option<String>,
    model: Model,
    context: Context,
    options: GoogleVertexOptions,
) -> Result<Vec<AssistantMessageEvent>, String> {
    let explicit_api_key = resolve_explicit_api_key(&options);

    // When auth is by API key we still need a project + location to address
    // the Vertex endpoint. When ADC is used, same constraints apply.
    let project = resolve_project(&options)?;
    let location = resolve_location(&options)?;

    let host_base = resolve_vertex_host_base(base_url_override.as_deref(), &model.base_url, &location);

    let mut url = format!(
        "{host_base}/v1/projects/{project}/locations/{location}/publishers/google/models/{model_id}:streamGenerateContent?alt=sse",
        host_base = host_base,
        project = project,
        location = location,
        model_id = model.id,
    );

    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

    if let Some(api_key) = &explicit_api_key {
        url.push_str("&key=");
        url.push_str(&urlencoding::encode(api_key));
    } else {
        let token = match token_provider {
            Some(provider) => provider().await?,
            None => env_api_keys::vertex_access_token().await?,
        };
        let value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| format!("Invalid ADC access token: {e}"))?;
        headers.insert(AUTHORIZATION, value);
    }

    if let Some(model_headers) = &model.headers {
        for (key, value) in model_headers {
            if let (Ok(name), Ok(val)) = (
                reqwest::header::HeaderName::from_bytes(key.as_bytes()),
                HeaderValue::from_str(value),
            ) {
                headers.insert(name, val);
            }
        }
    }

    if let Some(custom_headers) = &options.base.headers {
        for (key, value) in custom_headers {
            if let (Ok(name), Ok(val)) = (
                reqwest::header::HeaderName::from_bytes(key.as_bytes()),
                HeaderValue::from_str(value),
            ) {
                headers.insert(name, val);
            }
        }
    }

    let shared_options = options.into_shared();
    let body = google_shared::build_request_body(&model, &context, &shared_options)?;

    let response = client
        .post(&url)
        .headers(headers)
        .body(body.to_string())
        .send()
        .await
        .map_err(|e| format!("Request failed: {e}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "Failed to read error body".to_string());
        return Err(format!("Vertex AI error ({status}): {body}"));
    }

    google_shared::parse_sse_stream(response, &model, Api::GoogleVertex).await
}

/// Pick the Vertex host base.
///
/// Precedence:
/// 1. Provider-level `base_url_override` (test seam or programmatic override).
/// 2. A custom `model.base_url` from the registry — anything that is not the
///    default template (`{location}-aiplatform.googleapis.com`) and not empty.
///    Lets `models.json` or extensions point Vertex traffic at a proxy.
/// 3. Default Vertex host built from the resolved location.
fn resolve_vertex_host_base(
    base_url_override: Option<&str>,
    model_base_url: &str,
    location: &str,
) -> String {
    let from_override = base_url_override
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let from_model = {
        let trimmed = model_base_url.trim();
        if trimmed.is_empty() || trimmed.contains("{location}") {
            None
        } else {
            Some(trimmed.to_string())
        }
    };
    let host = from_override
        .or(from_model)
        .unwrap_or_else(|| format!("https://{location}-aiplatform.googleapis.com"));
    host.trim_end_matches('/').to_string()
}

fn resolve_explicit_api_key(options: &GoogleVertexOptions) -> Option<String> {
    let candidate = options
        .base
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .or_else(|| {
            std::env::var("GOOGLE_CLOUD_API_KEY")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        });

    let key = candidate?;
    if key == GCP_VERTEX_CREDENTIALS_MARKER || is_placeholder_api_key(&key) {
        None
    } else {
        Some(key)
    }
}

fn is_placeholder_api_key(api_key: &str) -> bool {
    // Mirrors the TS reference regex `^<[^>]+>$`: must start with `<`, end
    // with `>`, contain at least one non-`>` character in between, and not
    // embed any other `>` until the trailing one.
    let bytes = api_key.as_bytes();
    if bytes.len() < 3 || bytes.first() != Some(&b'<') || bytes.last() != Some(&b'>') {
        return false;
    }
    !api_key[1..api_key.len() - 1].contains('>')
}

fn resolve_project(options: &GoogleVertexOptions) -> Result<String, String> {
    if let Some(project) = options.project.as_deref()
        && !project.trim().is_empty()
    {
        return Ok(project.trim().to_string());
    }
    if let Ok(project) = std::env::var("GOOGLE_CLOUD_PROJECT")
        && !project.trim().is_empty()
    {
        return Ok(project.trim().to_string());
    }
    if let Ok(project) = std::env::var("GCLOUD_PROJECT")
        && !project.trim().is_empty()
    {
        return Ok(project.trim().to_string());
    }
    Err(
        "Vertex AI requires a project ID. Set GOOGLE_CLOUD_PROJECT/GCLOUD_PROJECT or pass project in options."
            .to_string(),
    )
}

fn resolve_location(options: &GoogleVertexOptions) -> Result<String, String> {
    if let Some(location) = options.location.as_deref()
        && !location.trim().is_empty()
    {
        return Ok(location.trim().to_string());
    }
    if let Ok(location) = std::env::var("GOOGLE_CLOUD_LOCATION")
        && !location.trim().is_empty()
    {
        return Ok(location.trim().to_string());
    }
    Err(
        "Vertex AI requires a location. Set GOOGLE_CLOUD_LOCATION or pass location in options."
            .to_string(),
    )
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Generated Vertex models ship a `{location}` placeholder in
    /// `model.base_url` so the runtime can interpolate the resolved
    /// location at call time. The helper must NOT treat that as a
    /// caller-supplied override — doing so would emit a literal
    /// `{location}` to the wire.
    #[test]
    fn vertex_host_base_ignores_location_template_in_model_base_url() {
        let host = resolve_vertex_host_base(
            None,
            "https://{location}-aiplatform.googleapis.com",
            "us-central1",
        );
        assert_eq!(host, "https://us-central1-aiplatform.googleapis.com");
    }

    /// A non-templated `model.base_url` is a deliberate proxy / gateway
    /// pointer and must beat the default host.
    #[test]
    fn vertex_host_base_uses_model_base_url_when_custom() {
        let host =
            resolve_vertex_host_base(None, "https://proxy.example.com", "us-central1");
        assert_eq!(host, "https://proxy.example.com");
    }

    /// Provider-level override (test seam, programmatic config) wins over
    /// `model.base_url` so a runtime can force a specific endpoint.
    #[test]
    fn vertex_host_base_provider_override_beats_model_base_url() {
        let host = resolve_vertex_host_base(
            Some("https://override.example.com"),
            "https://proxy.example.com",
            "us-central1",
        );
        assert_eq!(host, "https://override.example.com");
    }

    #[test]
    fn vertex_host_base_trims_trailing_slash() {
        let host = resolve_vertex_host_base(
            Some("https://override.example.com/"),
            "https://{location}-aiplatform.googleapis.com",
            "us-central1",
        );
        assert_eq!(host, "https://override.example.com");
    }

    #[test]
    fn placeholder_api_key_detection() {
        assert!(is_placeholder_api_key("<authenticated>"));
        assert!(is_placeholder_api_key("<your-api-key>"));
        assert!(!is_placeholder_api_key("AIzaSyExample"));
        assert!(!is_placeholder_api_key(""));
        assert!(!is_placeholder_api_key("<>"));
        // Ensures we do not strip strings whose interior contains '>'.
        assert!(!is_placeholder_api_key("<abc>def>"));
    }

    #[test]
    fn explicit_api_key_strips_placeholder_marker() {
        let opts = GoogleVertexOptions {
            base: StreamOptions {
                api_key: Some(GCP_VERTEX_CREDENTIALS_MARKER.to_string()),
                ..Default::default()
            },
            ..Default::default()
        };
        // Force env to be empty for the lookup branch. We can't unset env in
        // a thread-safe way; this test only validates the option-side path.
        // Save and restore GOOGLE_CLOUD_API_KEY only when present.
        let saved = std::env::var("GOOGLE_CLOUD_API_KEY").ok();
        // SAFETY: tests are single-threaded by default for this module's vars.
        unsafe {
            std::env::remove_var("GOOGLE_CLOUD_API_KEY");
        }
        let key = resolve_explicit_api_key(&opts);
        if let Some(saved) = saved {
            unsafe {
                std::env::set_var("GOOGLE_CLOUD_API_KEY", saved);
            }
        }
        assert!(key.is_none());
    }

    #[test]
    fn explicit_api_key_passes_real_key_through() {
        let opts = GoogleVertexOptions {
            base: StreamOptions {
                api_key: Some("AIzaSyExample123".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            resolve_explicit_api_key(&opts).as_deref(),
            Some("AIzaSyExample123")
        );
    }
}
