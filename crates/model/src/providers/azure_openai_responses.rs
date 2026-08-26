//! Azure OpenAI Responses provider.
//!
//! A thin variant of [`OpenAIResponsesProvider`](super::openai_responses::OpenAIResponsesProvider)
//! that targets Azure-hosted OpenAI deployments.
//!
//! Differences from the OpenAI Responses provider:
//! - URL is built from `model.base_url` and points at
//!   `<base>/responses?api-version=<version>`.
//! - Authentication uses the `api-key` header instead of `Authorization: Bearer`.
//!
//! Body construction and SSE event parsing are delegated to the shared
//! `openai_responses_shared` helpers in this module's parent.

use crate::api_registry::{ApiProvider, AssistantMessageEventStream};
use crate::env_api_keys;
use crate::providers::openai_responses_shared::{
    build_request_body, current_timestamp_ms, drive_sse_stream,
};
use crate::types::{
    Api, AssistantMessage, AssistantMessageEvent, Context, Model, Provider, SimpleStreamOptions,
    StopReason, StreamOptions, Usage,
};

/// Default Azure OpenAI API version. Matches the TS reference's
/// `DEFAULT_AZURE_API_VERSION` ("v1").
const DEFAULT_AZURE_API_VERSION: &str = "v1";

// =============================================================================
// Options
// =============================================================================

/// Extended options for the Azure OpenAI Responses provider.
///
/// The current Rust port only carries the common base options; Azure-specific
/// knobs (deployment name overrides, custom api-version) can be threaded in
/// by callers via `model.base_url` until full parity with the TS surface is
/// required.
#[derive(Debug, Clone, Default)]
pub struct AzureOpenAIResponsesOptions {
    pub base: StreamOptions,
}

impl AzureOpenAIResponsesOptions {
    pub fn temperature(&self) -> Option<f32> {
        self.base.temperature
    }

    pub fn max_tokens(&self) -> Option<u32> {
        self.base.max_tokens
    }

    pub fn api_key(&self) -> Option<&str> {
        self.base.api_key.as_deref()
    }

    pub fn headers(&self) -> Option<&std::collections::HashMap<String, String>> {
        self.base.headers.as_ref()
    }
}

// =============================================================================
// Provider
// =============================================================================

/// Provider for Azure OpenAI Responses API.
#[derive(Debug, Clone)]
pub struct AzureOpenAIResponsesProvider {
    client: reqwest::Client,
    /// Optional base-URL override used by tests to point the provider at a
    /// mock HTTP server. When `None`, the request URL is built from
    /// `model.base_url`.
    base_url_override: Option<String>,
}

impl Default for AzureOpenAIResponsesProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl AzureOpenAIResponsesProvider {
    /// Create a new provider with a default `reqwest::Client`.
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url_override: None,
        }
    }

    /// Create a new provider using the supplied HTTP client. Useful when the
    /// caller wants to install custom timeouts, proxies, or other transport
    /// configuration.
    pub fn with_client(client: reqwest::Client) -> Self {
        Self {
            client,
            base_url_override: None,
        }
    }

    /// Test seam: override the base URL used to build the responses endpoint.
    /// Routes requests at `<base>/responses?api-version=<version>` regardless
    /// of the value carried by `Model.base_url`.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url_override = Some(base_url.into());
        self
    }
}

impl ApiProvider for AzureOpenAIResponsesProvider {
    fn stream(
        &self,
        model: Model,
        context: Context,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        stream_azure_openai_responses(
            self.client.clone(),
            self.base_url_override.clone(),
            model,
            context,
            options,
        )
    }

    fn stream_simple(
        &self,
        model: Model,
        context: Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        let api_key = options
            .as_ref()
            .and_then(|o| o.api_key().map(|s| s.to_string()))
            .or_else(|| env_api_keys::get_env_api_key(&model.provider));

        if api_key.is_none() {
            let error_msg = format!("No API key for provider: {:?}", model.provider);
            return make_error_stream(error_msg, model.id.clone(), model.provider, model.api);
        }

        let mut base = StreamOptions::default();
        if let Some(opts) = &options {
            base.temperature = opts.temperature();
            base.max_tokens = opts.max_tokens();
            base.api_key = api_key;
            base.headers = opts.headers().cloned();
            // Forward cancellation/timeout/retry surface from
            // SimpleStreamOptions.base — the wrapper in
            // `stream::stream_simple` installs a combined token into
            // `opts.base.signal`; dropping it here would silently
            // un-cancel the SSE loop despite the wrapper's contract.
            base.signal = opts.base.signal.clone();
            base.timeout_ms = opts.base.timeout_ms;
            base.max_retries = opts.base.max_retries;
            base.max_retry_delay_ms = opts.base.max_retry_delay_ms;
        }

        stream_azure_openai_responses(
            self.client.clone(),
            self.base_url_override.clone(),
            model,
            context,
            Some(base),
        )
    }
}

// =============================================================================
// Internal helpers
// =============================================================================

fn make_error_stream(
    error_msg: String,
    model_id: String,
    provider: Provider,
    api: Api,
) -> AssistantMessageEventStream<'static> {
    Box::pin(async_stream::stream! {
        yield AssistantMessageEvent::Error {
            reason: StopReason::Error,
            error: AssistantMessage {
                role: "assistant".to_string(),
                api,
                provider,
                model: model_id,
                usage: Usage::default(),
                stop_reason: StopReason::Error,
                raw_stop_reason: None,
                error_message: Some(error_msg),
                timestamp: current_timestamp_ms(),
                content: vec![],
                response_model: None,
                response_id: None,
                diagnostics: None,
            },
        };
    })
}

/// Resolve the Azure OpenAI base URL by walking the precedence chain:
/// provider-level override → `AZURE_OPENAI_BASE_URL` env var →
/// `AZURE_OPENAI_RESOURCE_NAME` env var (expanded to the canonical
/// `https://{resource}.openai.azure.com/openai/v1`) → `model.base_url`.
/// Empty / whitespace values at each stage are skipped. Returns `None`
/// when nothing resolves so the caller can emit a clear error.
fn resolve_azure_base_url(override_url: Option<&str>, model_base_url: &str) -> Option<String> {
    let from_override = override_url
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if from_override.is_some() {
        return from_override;
    }
    let from_env_base = std::env::var("AZURE_OPENAI_BASE_URL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if from_env_base.is_some() {
        return from_env_base;
    }
    let from_env_resource = std::env::var("AZURE_OPENAI_RESOURCE_NAME")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(|r| format!("https://{r}.openai.azure.com/openai/v1"));
    if from_env_resource.is_some() {
        return from_env_resource;
    }
    let from_model = model_base_url.trim();
    if from_model.is_empty() {
        None
    } else {
        Some(from_model.to_string())
    }
}

/// Build the Azure responses endpoint URL.
///
/// Treats `base` as the Azure base (e.g. `https://{resource}.openai.azure.com/openai/v1`)
/// and produces `<base>/responses?api-version=<version>`.
///
/// If `base` already encodes a path that looks like a fully-qualified
/// responses endpoint (ends in `/responses` with or without a query string),
/// it is returned as-is — this lets tests and advanced callers pin an exact
/// URL without the helper second-guessing them.
fn build_azure_url(base: &str, api_version: &str) -> String {
    let trimmed = base.trim().trim_end_matches('/');

    // Already a full responses URL? Don't double-append.
    if trimmed.contains("/responses") {
        return trimmed.to_string();
    }

    let normalized = normalize_azure_host_path(trimmed);
    format!("{}/responses?api-version={}", normalized, api_version)
}

/// Normalize Azure host base URLs so the AzureOpenAI SDK shape applies.
///
/// Azure hosts (`*.openai.azure.com`, `*.cognitiveservices.azure.com`)
/// require an `/openai/v1` base path so the trailing `/responses` and
/// `?api-version=...` slots line up. A bare host or one ending in just
/// `/openai` is rewritten to include `/openai/v1`. Non-Azure hosts pass
/// through unchanged.
fn normalize_azure_host_path(trimmed: &str) -> String {
    let Some((scheme_host, path)) = split_scheme_host_path(trimmed) else {
        return trimmed.to_string();
    };
    let host = scheme_host
        .strip_prefix("https://")
        .or_else(|| scheme_host.strip_prefix("http://"))
        .unwrap_or(&scheme_host)
        .to_lowercase();
    let is_azure_host =
        host.ends_with(".openai.azure.com") || host.ends_with(".cognitiveservices.azure.com");
    let normalized_path = path.trim_end_matches('/');
    if is_azure_host && (normalized_path.is_empty() || normalized_path == "/openai") {
        return format!("{scheme_host}/openai/v1");
    }
    if normalized_path == path {
        return trimmed.to_string();
    }
    format!("{scheme_host}{normalized_path}")
}

/// Split a `scheme://host[:port]/path` URL into the `scheme://host[:port]`
/// prefix and the `/path` suffix (path is `""` if absent). Returns
/// `None` when no scheme separator is found, which makes the caller
/// pass the input through untouched.
fn split_scheme_host_path(url: &str) -> Option<(String, String)> {
    let scheme_end = url.find("://")?;
    let after_scheme = &url[scheme_end + 3..];
    match after_scheme.find('/') {
        Some(path_start) => {
            let host_end = scheme_end + 3 + path_start;
            Some((url[..host_end].to_string(), url[host_end..].to_string()))
        }
        None => Some((url.to_string(), String::new())),
    }
}

fn stream_azure_openai_responses(
    client: reqwest::Client,
    base_url_override: Option<String>,
    model: Model,
    context: Context,
    options: Option<StreamOptions>,
) -> AssistantMessageEventStream<'static> {
    let options = options.unwrap_or_default();

    Box::pin(async_stream::stream! {
        let mut output = AssistantMessage {
            role: "assistant".to_string(),
            content: vec![],
            api: model.api,
            provider: model.provider,
            model: model.id.clone(),
            stop_reason: StopReason::Stop,
            raw_stop_reason: None,
            usage: Usage::default(),
            error_message: None,
            timestamp: current_timestamp_ms(),
            response_model: None,
            response_id: None,
            diagnostics: None,
        };

        yield AssistantMessageEvent::Start {
            partial: output.clone(),
        };

        let body = build_request_body(&model, &context, &options);

        // URL resolution precedence:
        //   1. Provider-level `base_url_override` (test seam).
        //   2. `AZURE_OPENAI_BASE_URL` env var.
        //   3. `AZURE_OPENAI_RESOURCE_NAME` env var, expanded to the
        //      canonical `https://{resource}.openai.azure.com/openai/v1`.
        //   4. `model.base_url` from the registry.
        // Empty / whitespace values at each stage are skipped so a
        // stale env var doesn't shadow a real config further down the
        // chain.
        let resolved_base = resolve_azure_base_url(base_url_override.as_deref(), &model.base_url);
        let Some(base) = resolved_base else {
            output.error_message = Some(
                "Azure OpenAI base URL is required. Set AZURE_OPENAI_BASE_URL or \
                 AZURE_OPENAI_RESOURCE_NAME, or set model.base_url.".to_string(),
            );
            output.stop_reason = StopReason::Error;
            yield AssistantMessageEvent::Error {
                reason: StopReason::Error,
                error: output,
            };
            return;
        };

        // API version: caller-supplied env override beats the baked
        // default; honour an empty env var as "use the default".
        let api_version = std::env::var("AZURE_OPENAI_API_VERSION")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_AZURE_API_VERSION.to_string());
        let url = build_azure_url(&base, &api_version);

        let api_key = options.api_key
            .or_else(|| env_api_keys::get_env_api_key(&model.provider))
            .unwrap_or_default();

        // Azure auth uses the `api-key` header — NOT `Authorization: Bearer`.
        let mut builder = client.post(&url)
            .header("Content-Type", "application/json")
            .header("api-key", api_key)
            .body(serde_json::to_string(&body).unwrap_or_default());

        if let Some(ref headers) = options.headers {
            for (k, v) in headers {
                builder = builder.header(k, v);
            }
        }
        if let Some(ref model_headers) = model.headers {
            for (k, v) in model_headers {
                builder = builder.header(k, v);
            }
        }

        let response = match builder.send().await {
            Ok(r) => r,
            Err(e) => {
                output.error_message = Some(format!("Request failed: {}", e));
                output.stop_reason = StopReason::Error;
                yield AssistantMessageEvent::Error {
                    reason: StopReason::Error,
                    error: output,
                };
                return;
            }
        };

        // `on_response` callback fires once the response headers are in,
        // regardless of HTTP status. Extensions use it to surface
        // rate-limit / request-id / retry-after headers per request.
        if let Some(on_response) = options.on_response.clone() {
            let status = response.status().as_u16();
            let mut headers_map: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            for (name, value) in response.headers().iter() {
                if let Ok(v) = value.to_str() {
                    headers_map.insert(name.as_str().to_string(), v.to_string());
                }
            }
            on_response(status, headers_map, &model);
        }

        if !response.status().is_success() {
            let status = response.status();
            let body_text = response.text().await.unwrap_or_default();
            output.error_message = Some(format!("HTTP {}: {}", status, body_text));
            output.stop_reason = StopReason::Error;
            yield AssistantMessageEvent::Error {
                reason: StopReason::Error,
                error: output,
            };
            return;
        }

        // Stream SSE events from the shared decoder. The decoder yields a
        // terminal `Error` event itself on transport failure and updates
        // `output.stop_reason` so we can skip the `Done` below.
        {
            use futures::StreamExt;
            let mut inner = Box::pin(drive_sse_stream(response, &mut output));
            while let Some(ev) = inner.next().await {
                yield ev;
            }
        }

        if matches!(output.stop_reason, StopReason::Error) {
            return;
        }

        yield AssistantMessageEvent::Done {
            reason: output.stop_reason,
            message: output,
        };
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn azure_url_appends_responses_and_api_version() {
        let url = build_azure_url("https://my-resource.openai.azure.com/openai/v1", "v1");
        assert_eq!(
            url,
            "https://my-resource.openai.azure.com/openai/v1/responses?api-version=v1",
        );
    }

    #[test]
    fn azure_url_strips_trailing_slash() {
        let url = build_azure_url("https://my-resource.openai.azure.com/openai/v1/", "v1");
        assert_eq!(
            url,
            "https://my-resource.openai.azure.com/openai/v1/responses?api-version=v1",
        );
    }

    #[test]
    fn azure_url_preserves_explicit_responses_path() {
        let url = build_azure_url(
            "https://my-resource.openai.azure.com/openai/v1/responses?api-version=2024-12-01",
            "v1",
        );
        assert_eq!(
            url,
            "https://my-resource.openai.azure.com/openai/v1/responses?api-version=2024-12-01",
        );
    }

    /// Azure OpenAI Service hosts (`*.openai.azure.com`) without an
    /// explicit `/openai/v1` path get the path auto-appended so the
    /// AzureOpenAI SDK shape (`<host>/openai/v1/responses`) lines up.
    #[test]
    fn azure_url_normalizes_bare_openai_azure_host() {
        let url = build_azure_url("https://my-resource.openai.azure.com", "v1");
        assert_eq!(
            url,
            "https://my-resource.openai.azure.com/openai/v1/responses?api-version=v1",
        );
    }

    /// A host that ends in `/openai` (no trailing version segment) is
    /// rewritten to the canonical `/openai/v1` base path.
    #[test]
    fn azure_url_normalizes_openai_only_path() {
        let url = build_azure_url("https://my-resource.openai.azure.com/openai", "v1");
        assert_eq!(
            url,
            "https://my-resource.openai.azure.com/openai/v1/responses?api-version=v1",
        );
    }

    /// Azure Cognitive Services hosts get the same `/openai/v1`
    /// rewrite so a bare cognitive-services URL doesn't end up with a
    /// broken `<host>/responses?api-version=...` shape.
    #[test]
    fn azure_url_normalizes_cognitiveservices_host() {
        let url = build_azure_url("https://my-resource.cognitiveservices.azure.com", "v1");
        assert_eq!(
            url,
            "https://my-resource.cognitiveservices.azure.com/openai/v1/responses?api-version=v1",
        );
    }

    /// Non-Azure hosts must pass through unchanged — the helper must
    /// not invent an `/openai/v1` path for arbitrary proxies.
    #[test]
    fn azure_url_leaves_non_azure_hosts_alone() {
        let url = build_azure_url("https://proxy.example.com/v1", "v1");
        assert_eq!(url, "https://proxy.example.com/v1/responses?api-version=v1");
    }

    /// `resolve_azure_base_url` walks the precedence chain — explicit
    /// override beats every other source, including the env vars and
    /// the model registry. Empty / whitespace overrides fall through.
    /// `AZURE_OPENAI_BASE_URL` then `AZURE_OPENAI_RESOURCE_NAME`
    /// expand next, and `model.base_url` is the final fallback;
    /// missing every stage returns `None` so the caller can surface a
    /// precise error. Cargo tests run in parallel and share process
    /// env, so this case folds every scenario into a single test
    /// guarded by a static mutex.
    #[test]
    fn resolve_azure_base_url_walks_precedence_chain() {
        static GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _lock = GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let prior_base = std::env::var("AZURE_OPENAI_BASE_URL").ok();
        let prior_resource = std::env::var("AZURE_OPENAI_RESOURCE_NAME").ok();

        // Case 1: explicit override beats env and model.
        unsafe {
            std::env::set_var("AZURE_OPENAI_BASE_URL", "https://from-env.example.com");
            std::env::set_var("AZURE_OPENAI_RESOURCE_NAME", "from-resource");
        }
        assert_eq!(
            resolve_azure_base_url(
                Some("https://from-override.example.com"),
                "https://from-model.example.com",
            ),
            Some("https://from-override.example.com".to_string()),
        );
        // Whitespace-only override falls through to env.
        assert_eq!(
            resolve_azure_base_url(Some("   "), "https://from-model.example.com"),
            Some("https://from-env.example.com".to_string()),
        );

        // Case 2: AZURE_OPENAI_RESOURCE_NAME expands when no override
        // and no `AZURE_OPENAI_BASE_URL`.
        unsafe {
            std::env::remove_var("AZURE_OPENAI_BASE_URL");
            std::env::set_var("AZURE_OPENAI_RESOURCE_NAME", "my-resource");
        }
        assert_eq!(
            resolve_azure_base_url(None, ""),
            Some("https://my-resource.openai.azure.com/openai/v1".to_string()),
        );

        // Case 3: `model.base_url` is the final fallback when nothing
        // else is set.
        unsafe {
            std::env::remove_var("AZURE_OPENAI_BASE_URL");
            std::env::remove_var("AZURE_OPENAI_RESOURCE_NAME");
        }
        assert_eq!(
            resolve_azure_base_url(None, "https://my-resource.openai.azure.com/openai/v1"),
            Some("https://my-resource.openai.azure.com/openai/v1".to_string()),
        );

        // Case 4: nothing set anywhere → None so the caller errors.
        assert_eq!(resolve_azure_base_url(None, ""), None);
        assert_eq!(resolve_azure_base_url(None, "   "), None);

        unsafe {
            match prior_base {
                Some(v) => std::env::set_var("AZURE_OPENAI_BASE_URL", v),
                None => std::env::remove_var("AZURE_OPENAI_BASE_URL"),
            }
            match prior_resource {
                Some(v) => std::env::set_var("AZURE_OPENAI_RESOURCE_NAME", v),
                None => std::env::remove_var("AZURE_OPENAI_RESOURCE_NAME"),
            }
        }
    }

    #[test]
    fn provider_creation() {
        let _ = AzureOpenAIResponsesProvider::new();
        let _ = AzureOpenAIResponsesProvider::default();
        let _ = AzureOpenAIResponsesProvider::with_client(reqwest::Client::new());
    }

    #[test]
    fn options_accessors() {
        let mut opts = AzureOpenAIResponsesOptions::default();
        opts.base.temperature = Some(0.5);
        opts.base.max_tokens = Some(128);
        opts.base.api_key = Some("k".to_string());
        assert_eq!(opts.temperature(), Some(0.5));
        assert_eq!(opts.max_tokens(), Some(128));
        assert_eq!(opts.api_key(), Some("k"));
    }
}
