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
//! Body construction and SSE event parsing are delegated to
//! [`super::openai_responses_shared`].

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
    let trimmed = base.trim_end_matches('/');

    // Already a full responses URL? Don't double-append.
    if trimmed.contains("/responses") {
        return trimmed.to_string();
    }

    format!("{}/responses?api-version={}", trimmed, api_version)
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

        // URL: prefer explicit override (test seam), then `model.base_url`.
        let base = base_url_override
            .as_deref()
            .unwrap_or(model.base_url.as_str())
            .trim();

        if base.is_empty() {
            output.error_message = Some(
                "Azure OpenAI base URL is required (set model.base_url or pass an override).".to_string(),
            );
            output.stop_reason = StopReason::Error;
            yield AssistantMessageEvent::Error {
                reason: StopReason::Error,
                error: output,
            };
            return;
        }

        let url = build_azure_url(base, DEFAULT_AZURE_API_VERSION);

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
