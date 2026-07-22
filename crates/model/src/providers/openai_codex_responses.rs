//! OpenAI Codex Responses API provider.
//!
//! The Codex Responses endpoint is a ChatGPT-account-scoped variant of the
//! standard OpenAI Responses API. Three transports are envisaged:
//!
//! - **SSE** (default): POSTs to `https://chatgpt.com/backend-api/codex/responses`
//!   with `Authorization: Bearer <oauth-token>` plus a handful of
//!   ChatGPT-specific headers (`chatgpt-account-id`, `OpenAI-Beta`,
//!   `session_id`, `x-client-request-id`). Wire format matches OpenAI
//!   Responses, so the SSE parser is shared.
//! - **WebSocket** (`Transport::Websocket`): A `wss://` channel with the
//!   same payload shape. The current Rust port stubs this transport — the
//!   request is routed to the WebSocket URL but actual frame handling is
//!   left for a follow-up.
//! - **WebSocket-cached** (`Transport::WebsocketCached`): Reuses an idle
//!   socket from [`SessionResources`]. Also stubbed.
//!
//! Credentials come from M4's `OAuthRegistry` when the caller does not
//! supply an explicit `api_key`. Expired tokens are refreshed in place.

use std::sync::Arc;

use crate::api_registry::{ApiProvider, AssistantMessageEventStream};
use crate::env_api_keys;
use crate::oauth::{OAuthCredentials, OAuthProviderId, OAuthRegistry};
use crate::providers::openai_responses_shared::{
    build_request_body, current_timestamp_ms, drive_sse_stream,
};
use crate::session_resources::SessionResources;
use crate::types::{
    AssistantMessage, AssistantMessageEvent, Context, Model, Provider, SimpleStreamOptions,
    StopReason, StreamOptions, Transport, Usage,
};
use serde_json::Value;

// =============================================================================
// Constants
// =============================================================================

/// Default base URL for the ChatGPT Codex backend. Mirrors the TS
/// reference's `DEFAULT_CODEX_BASE_URL`.
const DEFAULT_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api";

/// JWT claim path containing the ChatGPT account id. Token payloads embed
/// `{ "https://api.openai.com/auth": { "chatgpt_account_id": "..." } }`.
const JWT_CLAIM_PATH: &str = "https://api.openai.com/auth";

/// Beta header value for SSE responses.
const OPENAI_BETA_RESPONSES: &str = "responses=experimental";

/// Beta header value for WebSocket responses.
///
/// Reserved for the WebSocket transport follow-up; used the moment the
/// `tokio-tungstenite` driver lands.
#[allow(dead_code)]
const OPENAI_BETA_RESPONSES_WEBSOCKETS: &str = "responses_websockets=2026-02-06";

/// User-agent originator value.
const ORIGINATOR: &str = "pi";

// =============================================================================
// Options
// =============================================================================

/// Extended options for the OpenAI Codex Responses provider.
///
/// Mirrors `OpenAICodexResponsesOptions` in the TS reference. The current
/// Rust port surfaces only the common base — Codex-specific knobs
/// (`reasoningEffort`, `serviceTier`, `textVerbosity`) can be threaded
/// through `base.metadata` until full parity with the TS surface is
/// required.
#[derive(Debug, Clone, Default)]
pub struct OpenAICodexResponsesOptions {
    pub base: StreamOptions,
}

impl OpenAICodexResponsesOptions {
    pub fn temperature(&self) -> Option<f32> {
        self.base.temperature
    }

    pub fn max_tokens(&self) -> Option<u32> {
        self.base.max_tokens
    }

    pub fn api_key(&self) -> Option<&str> {
        self.base.api_key.as_deref()
    }

    pub fn session_id(&self) -> Option<&str> {
        self.base.session_id.as_deref()
    }

    pub fn transport(&self) -> Option<Transport> {
        self.base.transport
    }
}

// =============================================================================
// Debug stats
// =============================================================================

/// Debug counters surfaced by the Codex WebSocket transport.
///
/// The current SSE-only port keeps this struct as a stable surface so
/// downstream code (CLI, observability hooks) can rely on it. Counters
/// are wired up by the WebSocket follow-up.
#[derive(Debug, Clone, Default)]
pub struct OpenAICodexWebSocketDebugStats {
    pub connections_opened: u64,
    pub connections_reused: u64,
    pub cache_probe_latency_ms: Option<u64>,
}

/// Snapshot of the global WebSocket debug counters.
///
/// Returns zeros until the WebSocket transport lands. Callers should
/// treat the result as a point-in-time copy.
pub fn websocket_debug_stats() -> OpenAICodexWebSocketDebugStats {
    OpenAICodexWebSocketDebugStats::default()
}

// =============================================================================
// Provider
// =============================================================================

/// Provider for the OpenAI Codex Responses API.
#[derive(Clone)]
pub struct OpenAICodexResponsesProvider {
    client: reqwest::Client,
    oauth_registry: Option<Arc<OAuthRegistry>>,
    session_pool: Arc<SessionResources>,
    /// Optional base-URL override. When set, replaces `model.base_url`
    /// and the hard-coded `DEFAULT_CODEX_BASE_URL`. Used by tests to
    /// point the provider at a `tiny_http` mock.
    base_url_override: Option<String>,
}

impl std::fmt::Debug for OpenAICodexResponsesProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAICodexResponsesProvider")
            .field("oauth_registry", &self.oauth_registry.is_some())
            .field("base_url_override", &self.base_url_override)
            .finish()
    }
}

impl Default for OpenAICodexResponsesProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenAICodexResponsesProvider {
    /// Create a new provider with a default `reqwest::Client` and no
    /// OAuth registry attached. Callers that rely on OAuth credentials
    /// should chain `.with_oauth_registry(...)`.
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            oauth_registry: None,
            session_pool: SessionResources::shared(),
            base_url_override: None,
        }
    }

    /// Create a new provider using the supplied HTTP client.
    pub fn with_client(client: reqwest::Client) -> Self {
        Self {
            client,
            oauth_registry: None,
            session_pool: SessionResources::shared(),
            base_url_override: None,
        }
    }

    /// Attach an `OAuthRegistry` for credential lookup. When the caller
    /// does not supply an explicit `api_key` we read credentials for
    /// [`OAuthProviderId::OpenAICodex`] out of the registry, refreshing
    /// expired tokens in place.
    pub fn with_oauth_registry(mut self, registry: Arc<OAuthRegistry>) -> Self {
        self.oauth_registry = Some(registry);
        self
    }

    /// Test seam: override the base URL used to build the responses
    /// endpoint. Routes requests at `<base>/codex/responses` (or the
    /// supplied URL as-is if it already encodes that path).
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url_override = Some(base_url.into());
        self
    }
}

impl ApiProvider for OpenAICodexResponsesProvider {
    fn stream(
        &self,
        model: Model,
        context: Context,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        stream_openai_codex_responses(
            self.client.clone(),
            self.oauth_registry.clone(),
            self.session_pool.clone(),
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
        // Use `build_base_options` so every field the caller set on the
        // simple options surface — notably `transport`, `cache_retention`,
        // `metadata`, `signal`, `on_payload`, `on_response`, and the
        // retry / timeout knobs — propagates to the underlying stream.
        // The previous manual mapping silently dropped `transport`, so a
        // caller asking for `Transport::Websocket` got SSE instead.
        let base = options.map(|opts| {
            let api_key = opts.api_key().map(|s| s.to_string());
            opts.build_base_options(&model, api_key)
        });
        stream_openai_codex_responses(
            self.client.clone(),
            self.oauth_registry.clone(),
            self.session_pool.clone(),
            self.base_url_override.clone(),
            model,
            context,
            base,
        )
    }
}

// =============================================================================
// URL & header helpers
// =============================================================================

/// Build the request body for the Codex `/codex/responses` endpoint.
///
/// Codex requires two fields that the shared Responses builder does not
/// emit:
///
/// * `store: false` — the ChatGPT Codex backend rejects `store: true`
///   ("Store must be set to false"). The shared builder omits the field
///   entirely; codex must always pin it to `false`.
/// * `instructions` — codex rejects requests with an empty or missing
///   `instructions` string. The shared builder skips the field when
///   `system_prompt` is empty; codex needs an explicit fallback of
///   `"You are a helpful assistant."`.
fn build_codex_request_body(model: &Model, context: &Context, options: &StreamOptions) -> Value {
    let mut body = build_request_body(model, context, options);

    // store: false is mandatory for Codex Responses.
    body["store"] = Value::Bool(false);

    // Codex Responses (chat.com /backend-api/codex/responses) doesn't
    // accept `prompt_cache_retention`. The shared builder may have
    // injected it for `cache_retention: long`; strip it back out.
    // `prompt_cache_key` is fine — Codex accepts and uses it.
    if let Some(obj) = body.as_object_mut() {
        obj.remove("prompt_cache_retention");
    }

    // Codex Responses returns reasoning content as encrypted blobs that
    // the backend re-attaches on subsequent turns. Without this opt-in
    // include the response stream drops reasoning entirely, breaking
    // multi-turn chains that depend on prior reasoning context.
    body["include"] = Value::Array(vec![Value::String(
        "reasoning.encrypted_content".to_string(),
    )]);

    // Always emit `instructions` for codex — backend rejects empty/missing.
    let has_instructions = body
        .get("instructions")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty());
    if !has_instructions {
        body["instructions"] = Value::String("You are a helpful assistant.".to_string());
    }

    // Default to `text.verbosity: "low"` so codex returns terse output by
    // default; agents that wrap responses with their own narration get
    // more signal per token. The upstream lowered this from `medium` for
    // the same reason. Callers that already set `text.verbosity` win.
    let text_has_verbosity = body.get("text").and_then(|t| t.get("verbosity")).is_some();
    if !text_has_verbosity {
        let entry = body
            .as_object_mut()
            .expect("request body is an object")
            .entry("text")
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        if let Some(obj) = entry.as_object_mut() {
            obj.insert("verbosity".to_string(), Value::String("low".to_string()));
        }
    }

    // Forward `service_tier` from caller metadata. The codex backend
    // accepts `auto` / `default` / `flex` / `scale` / `priority`; the
    // dedicated Rust options struct only exposes the common base today,
    // so callers thread the value through `StreamOptions.metadata`
    // under the `service_tier` key.
    if let Some(metadata) = options.metadata.as_ref()
        && let Some(value) = metadata.get("service_tier")
        && !value.is_null()
    {
        body["service_tier"] = value.clone();
    }

    body
}

fn resolve_codex_url(base: &str) -> String {
    let raw = if base.trim().is_empty() {
        DEFAULT_CODEX_BASE_URL
    } else {
        base
    };
    let trimmed = raw.trim_end_matches('/');

    if trimmed.ends_with("/codex/responses") {
        trimmed.to_string()
    } else if trimmed.ends_with("/codex") {
        format!("{trimmed}/responses")
    } else {
        format!("{trimmed}/codex/responses")
    }
}

/// Resolve the WebSocket endpoint URL by swapping the scheme of the SSE URL.
fn resolve_codex_websocket_url(base: &str) -> String {
    let url = resolve_codex_url(base);
    if let Some(rest) = url.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = url.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        url
    }
}

/// Pull the `chatgpt_account_id` claim out of a JWT bearer token.
///
/// Returns `None` if the token is not a JWT, the payload is not valid
/// base64/JSON, or the claim is missing. Callers degrade to omitting the
/// `chatgpt-account-id` header rather than failing the request — a
/// non-JWT token (e.g. a plain API key in tests) should still be usable
/// against mock servers.
fn extract_account_id(token: &str) -> Option<String> {
    use base64::Engine;

    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    // JWT payloads use base64url without padding; tolerate both.
    let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .ok()
        .or_else(|| {
            base64::engine::general_purpose::STANDARD_NO_PAD
                .decode(parts[1])
                .ok()
        })?;
    let payload: serde_json::Value = serde_json::from_slice(&payload_bytes).ok()?;
    payload
        .get(JWT_CLAIM_PATH)?
        .get("chatgpt_account_id")?
        .as_str()
        .map(|s| s.to_string())
}

/// Whether to emit the `session_id` cache-affinity header. Strict
/// OpenAI-compatible proxies reject the underscore-bearing header name;
/// they opt out via `OpenAIResponsesCompat.sendSessionIdHeader = false`.
/// `x-client-request-id` is unaffected and always sent — with the session
/// id when present, otherwise with a generated fallback.
pub(crate) fn should_send_session_id_header(compat: Option<&crate::types::Compat>) -> bool {
    match compat {
        Some(crate::types::Compat::OpenAIResponses(c)) => c.send_session_id_header.unwrap_or(true),
        _ => true,
    }
}

/// Build the SSE headers for a Codex request.
///
/// `x-client-request-id` is always emitted: the backend keys prompt caching
/// and request correlation on it, and some endpoints require a time-ordered
/// identifier. Without a session id we fall back to a fresh UUIDv7 instead
/// of sending nothing.
fn build_sse_headers(
    builder: reqwest::RequestBuilder,
    model_headers: Option<&std::collections::HashMap<String, String>>,
    extra_headers: Option<&std::collections::HashMap<String, String>>,
    token: &str,
    session_id: Option<&str>,
    send_session_id_header: bool,
) -> reqwest::RequestBuilder {
    let mut b = builder
        .header("Content-Type", "application/json")
        .header("Accept", "text/event-stream")
        .header("Authorization", format!("Bearer {token}"))
        .header("OpenAI-Beta", OPENAI_BETA_RESPONSES)
        .header("originator", ORIGINATOR);

    if let Some(account_id) = extract_account_id(token) {
        b = b.header("chatgpt-account-id", account_id);
    }

    match session_id {
        Some(sid) => {
            if send_session_id_header {
                b = b.header("session_id", sid);
            }
            b = b.header("x-client-request-id", sid);
        }
        None => {
            b = b.header("x-client-request-id", crate::utils::uuid_v7());
        }
    }

    if let Some(headers) = model_headers {
        for (k, v) in headers {
            b = b.header(k, v);
        }
    }
    if let Some(headers) = extra_headers {
        for (k, v) in headers {
            b = b.header(k, v);
        }
    }
    b
}

// =============================================================================
// OAuth integration
// =============================================================================

/// Resolve the bearer token used for the request.
///
/// Order of precedence:
/// 1. Explicit `options.api_key`.
/// 2. Provider environment variables (kept for parity with TS — current
///    Rust env table does not export an `OPENAI_CODEX_*` key, so this is
///    a no-op today).
/// 3. OAuth credentials persisted by `OAuthRegistry`. Expired tokens are
///    refreshed in place; the refreshed credentials are written back to
///    disk so subsequent requests reuse them.
async fn resolve_codex_token(
    options_api_key: Option<&str>,
    provider: &Provider,
    oauth_registry: Option<&Arc<OAuthRegistry>>,
) -> Result<String, String> {
    if let Some(key) = options_api_key.filter(|s| !s.is_empty()) {
        return Ok(key.to_string());
    }
    if let Some(env_key) = env_api_keys::get_env_api_key(provider).filter(|s| !s.is_empty()) {
        return Ok(env_key);
    }
    let registry = oauth_registry.ok_or_else(|| {
        "OAuth credentials required for OpenAI Codex (no api_key supplied and no \
         OAuth registry attached to the provider)"
            .to_string()
    })?;
    let map = registry
        .load()
        .await
        .map_err(|e| format!("failed to load OAuth credentials: {e}"))?;
    let info = map
        .get(&OAuthProviderId::OpenAICodex)
        .ok_or_else(|| {
            "OAuth credentials missing for OpenAI Codex (run `oauth login`)".to_string()
        })?
        .clone();

    let provider_impl = registry
        .get(OAuthProviderId::OpenAICodex)
        .ok_or_else(|| "OAuth provider OpenAICodex not registered".to_string())?;

    let creds = if provider_impl.is_expired(&info.credentials) {
        match provider_impl.refresh(&info.credentials).await {
            Ok(refreshed) => {
                // Persist the rotated token so future requests don't hit
                // the refresh endpoint on every call.
                let new_info = crate::oauth::OAuthAuthInfo {
                    provider_id: OAuthProviderId::OpenAICodex,
                    credentials: refreshed.clone(),
                    created_at_ms: current_timestamp_ms(),
                };
                let _ = registry.save(&new_info).await;
                refreshed
            }
            Err(e) => return Err(format!("OAuth refresh failed: {e}")),
        }
    } else {
        info.credentials
    };

    Ok(token_string(&creds))
}

fn token_string(creds: &OAuthCredentials) -> String {
    creds.access_token.clone()
}

// =============================================================================
// Stream driver
// =============================================================================

#[allow(clippy::too_many_arguments)]
fn stream_openai_codex_responses(
    client: reqwest::Client,
    oauth_registry: Option<Arc<OAuthRegistry>>,
    _session_pool: Arc<SessionResources>,
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

        // Always emit Start first — this matters for the parity guarantee
        // that an Error never lands without a preceding Start.
        yield AssistantMessageEvent::Start {
            partial: output.clone(),
        };

        // Resolve auth token (api_key → env → OAuth).
        let token = match resolve_codex_token(
            options.api_key.as_deref(),
            &model.provider,
            oauth_registry.as_ref(),
        )
        .await
        {
            Ok(t) => t,
            Err(msg) => {
                output.error_message = Some(msg);
                output.stop_reason = StopReason::Error;
                yield AssistantMessageEvent::Error {
                    reason: StopReason::Error,
                    error: output,
                };
                return;
            }
        };

        // Resolve URL (override → model.base_url → default).
        let base = base_url_override
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                let trimmed = model.base_url.trim();
                if trimmed.is_empty() { None } else { Some(trimmed) }
            })
            .unwrap_or(DEFAULT_CODEX_BASE_URL);

        let transport = options.transport.unwrap_or(Transport::Sse);
        match transport {
            Transport::Sse | Transport::Auto => {}
            Transport::Websocket | Transport::WebsocketCached => {
                // WebSocket transport not yet implemented in the Rust
                // port. Surface a clear error so callers know to fall
                // back to SSE rather than hanging on a dead socket.
                // TODO(M9-followup): wire `tokio-tungstenite` here.
                let url = resolve_codex_websocket_url(base);
                output.error_message = Some(format!(
                    "WebSocket transport for OpenAI Codex Responses is not yet \
                     implemented in the Rust port (would have connected to {url}). \
                     Set transport: \"sse\" or omit the option."
                ));
                output.stop_reason = StopReason::Error;
                yield AssistantMessageEvent::Error {
                    reason: StopReason::Error,
                    error: output,
                };
                return;
            }
        }

        let url = resolve_codex_url(base);
        let body = build_codex_request_body(&model, &context, &options);

        let builder = client
            .post(&url)
            .body(serde_json::to_string(&body).unwrap_or_default());
        let send_session_id = should_send_session_id_header(model.compat.as_ref());
        let builder = build_sse_headers(
            builder,
            model.headers.as_ref(),
            options.headers.as_ref(),
            &token,
            options.session_id.as_deref(),
            send_session_id,
        );

        let response = match builder.send().await {
            Ok(r) => r,
            Err(e) => {
                output.error_message = Some(format!("Request failed: {e}"));
                output.stop_reason = StopReason::Error;
                yield AssistantMessageEvent::Error {
                    reason: StopReason::Error,
                    error: output,
                };
                return;
            }
        };

        // `on_response` callback fires once the response headers are in,
        // regardless of HTTP status. Extensions use it to capture
        // rate-limit / request-id / retry hint headers per request.
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
            output.error_message = Some(format!("HTTP {status}: {body_text}"));
            output.stop_reason = StopReason::Error;
            yield AssistantMessageEvent::Error {
                reason: StopReason::Error,
                error: output,
            };
            return;
        }

        // Drive the SSE decoder. It owns its own Error emission on
        // mid-stream transport failure and stamps `output.stop_reason`
        // so we know to skip the trailing Done.
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

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_appends_codex_responses_to_root() {
        assert_eq!(
            resolve_codex_url("https://chatgpt.com/backend-api"),
            "https://chatgpt.com/backend-api/codex/responses",
        );
    }

    #[test]
    fn url_appends_responses_to_codex_path() {
        assert_eq!(
            resolve_codex_url("https://example.com/backend-api/codex"),
            "https://example.com/backend-api/codex/responses",
        );
    }

    #[test]
    fn url_preserves_full_codex_responses_path() {
        assert_eq!(
            resolve_codex_url("https://example.com/foo/codex/responses"),
            "https://example.com/foo/codex/responses",
        );
    }

    #[test]
    fn url_strips_trailing_slash() {
        assert_eq!(
            resolve_codex_url("https://example.com/backend-api/"),
            "https://example.com/backend-api/codex/responses",
        );
    }

    #[test]
    fn url_falls_back_to_default_for_empty_base() {
        assert_eq!(
            resolve_codex_url(""),
            "https://chatgpt.com/backend-api/codex/responses",
        );
    }

    #[test]
    fn websocket_url_swaps_https_for_wss() {
        assert_eq!(
            resolve_codex_websocket_url("https://chatgpt.com/backend-api"),
            "wss://chatgpt.com/backend-api/codex/responses",
        );
    }

    #[test]
    fn websocket_url_swaps_http_for_ws() {
        assert_eq!(
            resolve_codex_websocket_url("http://127.0.0.1:8080"),
            "ws://127.0.0.1:8080/codex/responses",
        );
    }

    #[test]
    fn account_id_extraction_returns_none_for_non_jwt() {
        assert!(extract_account_id("not-a-jwt").is_none());
        assert!(extract_account_id("aa.bb").is_none());
        assert!(extract_account_id("").is_none());
    }

    #[test]
    fn account_id_extraction_decodes_jwt_payload() {
        use base64::Engine;
        // Build a fake JWT-ish token: header.payload.signature, only
        // the payload matters for the extractor.
        let payload = serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acc_123"
            },
            "exp": 1_700_000_000_u64,
        });
        let payload_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&payload).unwrap());
        let token = format!("header.{payload_b64}.signature");

        assert_eq!(extract_account_id(&token), Some("acc_123".to_string()));
    }

    #[test]
    fn debug_stats_default_is_zero() {
        let stats = websocket_debug_stats();
        assert_eq!(stats.connections_opened, 0);
        assert_eq!(stats.connections_reused, 0);
        assert_eq!(stats.cache_probe_latency_ms, None);
    }

    #[test]
    fn provider_creation() {
        let _ = OpenAICodexResponsesProvider::new();
        let _ = OpenAICodexResponsesProvider::default();
        let _ = OpenAICodexResponsesProvider::with_client(reqwest::Client::new());
    }

    fn codex_test_model() -> Model {
        use crate::types::{Api, Cost, InputType};
        Model {
            id: "gpt-5-codex".to_string(),
            name: "gpt-5-codex".to_string(),
            api: Api::OpenAICodexResponses,
            provider: Provider::OpenAICodex,
            base_url: String::new(),
            reasoning: true,
            input: vec![InputType::Text],
            cost: Cost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_window: 200_000,
            max_tokens: 100_000,
            headers: None,
            compat: None,
            thinking_level_map: None,
        }
    }

    fn codex_user_context(system_prompt: Option<&str>) -> Context {
        use crate::types::{Message, UserMessage};
        Context {
            system_prompt: system_prompt.map(|s| s.to_string()),
            messages: vec![Message::User(UserMessage::new_text("hi"))],
            tools: None,
        }
    }

    /// Codex Responses rejects requests when `instructions` is empty or
    /// missing. The body builder must inject the default
    /// "You are a helpful assistant." whenever the caller's `system_prompt`
    /// is `None` or empty.
    #[test]
    fn codex_body_uses_default_instructions_when_system_prompt_missing() {
        let body = build_codex_request_body(
            &codex_test_model(),
            &codex_user_context(None),
            &StreamOptions::default(),
        );
        assert_eq!(
            body["instructions"].as_str(),
            Some("You are a helpful assistant.")
        );
    }

    #[test]
    fn codex_body_uses_default_instructions_when_system_prompt_empty() {
        let body = build_codex_request_body(
            &codex_test_model(),
            &codex_user_context(Some("")),
            &StreamOptions::default(),
        );
        assert_eq!(
            body["instructions"].as_str(),
            Some("You are a helpful assistant.")
        );
    }

    /// When the caller already supplied a non-empty system prompt, the
    /// builder must keep it verbatim — the default is a fallback, not a
    /// clobber.
    #[test]
    fn codex_body_preserves_explicit_system_prompt() {
        let body = build_codex_request_body(
            &codex_test_model(),
            &codex_user_context(Some("You are an assistant.")),
            &StreamOptions::default(),
        );
        assert_eq!(body["instructions"].as_str(), Some("You are an assistant."));
    }

    /// ChatGPT Codex Responses rejects `store: true` ("Store must be set
    /// to false"). The codex body must always pin `store: false`, even
    /// though the shared Responses builder omits the field entirely.
    #[test]
    fn codex_body_pins_store_false() {
        let body = build_codex_request_body(
            &codex_test_model(),
            &codex_user_context(Some("You are an assistant.")),
            &StreamOptions::default(),
        );
        assert_eq!(body["store"], serde_json::Value::Bool(false));
    }

    /// Strict OpenAI-compatible proxies reject the `session_id` header
    /// (underscore-bearing name fails their HTTP/2 validators). Models
    /// served by such proxies must be able to opt out via
    /// `OpenAIResponsesCompat.sendSessionIdHeader = false`.
    #[test]
    fn should_send_session_id_header_honors_responses_compat_flag() {
        use crate::types::{Compat, OpenAIResponsesCompat};
        assert!(should_send_session_id_header(None), "default is on");
        assert!(should_send_session_id_header(Some(
            &Compat::OpenAIResponses(OpenAIResponsesCompat {
                send_session_id_header: None,
                ..Default::default()
            })
        )));
        assert!(should_send_session_id_header(Some(
            &Compat::OpenAIResponses(OpenAIResponsesCompat {
                send_session_id_header: Some(true),
                ..Default::default()
            })
        )));
        assert!(!should_send_session_id_header(Some(
            &Compat::OpenAIResponses(OpenAIResponsesCompat {
                send_session_id_header: Some(false),
                ..Default::default()
            })
        )));
    }

    /// A non-Responses compat block (e.g. OpenAICompletions on a model
    /// served as Codex) must not interfere — the helper defaults to
    /// sending the header.
    #[test]
    fn should_send_session_id_header_ignores_unrelated_compat() {
        use crate::types::Compat;
        let other = Compat::OpenAICompletions(Box::default());
        assert!(should_send_session_id_header(Some(&other)));
    }

    /// Build a request through `build_sse_headers` and return the header map.
    fn sse_request_headers(
        session_id: Option<&str>,
        send_session_id_header: bool,
    ) -> reqwest::header::HeaderMap {
        let builder = reqwest::Client::new().post("http://localhost/responses");
        build_sse_headers(
            builder,
            None,
            None,
            "sk-test",
            session_id,
            send_session_id_header,
        )
        .build()
        .expect("request must build")
        .headers()
        .clone()
    }

    /// With a session id, `x-client-request-id` carries the session id and
    /// `session_id` rides along (subject to the compat gate).
    #[test]
    fn sse_headers_use_session_id_as_request_id() {
        let headers = sse_request_headers(Some("sess-codex"), true);
        assert_eq!(
            headers.get("x-client-request-id").unwrap(),
            "sess-codex",
            "request id must echo the session id"
        );
        assert_eq!(headers.get("session_id").unwrap(), "sess-codex");

        let gated = sse_request_headers(Some("sess-codex"), false);
        assert_eq!(gated.get("x-client-request-id").unwrap(), "sess-codex");
        assert!(
            gated.get("session_id").is_none(),
            "compat gate must suppress the session_id header"
        );
    }

    /// Without a session id the request must still carry a request id —
    /// the backend keys prompt caching and correlation on it — and the
    /// fallback must be a time-ordered UUIDv7. The `session_id` header
    /// must never be fabricated.
    #[test]
    fn sse_headers_fall_back_to_uuidv7_request_id() {
        let headers = sse_request_headers(None, true);
        assert!(
            headers.get("session_id").is_none(),
            "no session id means no session_id header"
        );
        let request_id = headers
            .get("x-client-request-id")
            .expect("fallback request id must be present")
            .to_str()
            .unwrap();
        assert_eq!(request_id.len(), 36, "hyphenated UUID: {request_id}");
        assert_eq!(
            request_id.as_bytes()[14],
            b'7',
            "version nibble must be 7: {request_id}"
        );
    }

    /// Codex defaults to `text.verbosity: "low"` so the backend returns
    /// terse output unless the caller overrides it. Without the default,
    /// the model leans toward verbose narration that wraps inside the
    /// agent's own narration and burns tokens.
    #[test]
    fn codex_body_defaults_text_verbosity_to_low() {
        let body = build_codex_request_body(
            &codex_test_model(),
            &codex_user_context(Some("You are an assistant.")),
            &StreamOptions::default(),
        );
        assert_eq!(
            body["text"]["verbosity"].as_str(),
            Some("low"),
            "body must default text.verbosity to low: {body}"
        );
    }

    /// Codex Responses returns reasoning as encrypted blobs that the
    /// backend only re-attaches on subsequent turns when the caller
    /// opts in via `include: ["reasoning.encrypted_content"]`. Without
    /// the include the reasoning chain is dropped after one turn, so
    /// the field must be set on every codex request — regardless of
    /// `cache_retention`, `session_id`, or any other option.
    #[test]
    fn codex_body_always_requests_encrypted_reasoning_content() {
        let body = build_codex_request_body(
            &codex_test_model(),
            &codex_user_context(Some("You are an assistant.")),
            &StreamOptions::default(),
        );
        let include = body
            .get("include")
            .and_then(|v| v.as_array())
            .expect("codex body must carry an `include` array");
        let entries: Vec<&str> = include.iter().filter_map(|v| v.as_str()).collect();
        assert!(
            entries.contains(&"reasoning.encrypted_content"),
            "include must request encrypted reasoning content: {body}"
        );
    }

    /// The dedicated codex options struct only surfaces the common
    /// `base: StreamOptions` today, so callers pass codex-specific
    /// knobs (service tier, reasoning effort, verbosity) through
    /// `metadata`. The body builder must forward
    /// `metadata.service_tier` to the request body so priority /
    /// flex pricing is actually applied; missing or `null` metadata
    /// must leave the field unset.
    #[test]
    fn codex_body_forwards_service_tier_from_metadata() {
        use std::collections::HashMap;
        let mut metadata: HashMap<String, Value> = HashMap::new();
        metadata.insert(
            "service_tier".to_string(),
            Value::String("priority".to_string()),
        );
        let options = StreamOptions {
            metadata: Some(metadata),
            ..StreamOptions::default()
        };
        let body = build_codex_request_body(
            &codex_test_model(),
            &codex_user_context(Some("You are an assistant.")),
            &options,
        );
        assert_eq!(
            body["service_tier"].as_str(),
            Some("priority"),
            "service_tier must be forwarded from metadata: {body}"
        );
    }

    #[test]
    fn codex_body_omits_service_tier_when_metadata_missing_or_null() {
        use std::collections::HashMap;
        let no_meta = build_codex_request_body(
            &codex_test_model(),
            &codex_user_context(Some("You are an assistant.")),
            &StreamOptions::default(),
        );
        assert!(
            no_meta.get("service_tier").is_none(),
            "service_tier must be absent when metadata is unset: {no_meta}"
        );

        let mut metadata: HashMap<String, Value> = HashMap::new();
        metadata.insert("service_tier".to_string(), Value::Null);
        let options = StreamOptions {
            metadata: Some(metadata),
            ..StreamOptions::default()
        };
        let null_meta = build_codex_request_body(
            &codex_test_model(),
            &codex_user_context(Some("You are an assistant.")),
            &options,
        );
        assert!(
            null_meta.get("service_tier").is_none(),
            "explicit null metadata must not produce a service_tier field: {null_meta}"
        );
    }

    /// Codex Responses accepts `prompt_cache_key` but not
    /// `prompt_cache_retention`. The shared Responses body builder
    /// emits both for `cache_retention: long`; the codex wrapper has
    /// to strip the retention field while preserving the key.
    #[test]
    fn codex_body_strips_prompt_cache_retention_but_keeps_key() {
        use crate::types::CacheRetention;
        let options = StreamOptions {
            session_id: Some("sess-codex".to_string()),
            cache_retention: Some(CacheRetention::Long),
            ..StreamOptions::default()
        };
        let body = build_codex_request_body(
            &codex_test_model(),
            &codex_user_context(Some("You are an assistant.")),
            &options,
        );
        assert_eq!(
            body["prompt_cache_key"].as_str(),
            Some("sess-codex"),
            "codex must still emit prompt_cache_key: {body}"
        );
        assert!(
            body.get("prompt_cache_retention").is_none(),
            "codex must strip prompt_cache_retention: {body}"
        );
    }

    /// `stream_simple` is the entry point for the agent loop. It previously
    /// only forwarded a handful of fields from `SimpleStreamOptions`,
    /// silently dropping `transport`, `cache_retention`, `metadata`,
    /// `signal`, `on_payload`, `on_response`, and the retry / timeout
    /// knobs. A caller asking for `Transport::Websocket` therefore got
    /// SSE behaviour, hiding the "not yet implemented" error and giving
    /// the wrong transport semantics. Pin the propagation via the public
    /// helper so a future refactor cannot regress it.
    #[test]
    fn simple_options_build_base_options_forwards_transport() {
        let model = codex_test_model();
        let mut simple = SimpleStreamOptions::default();
        simple.base.transport = Some(Transport::Websocket);
        let base = simple.build_base_options(&model, None);
        assert_eq!(base.transport, Some(Transport::Websocket));

        simple.base.transport = Some(Transport::WebsocketCached);
        let base = simple.build_base_options(&model, None);
        assert_eq!(base.transport, Some(Transport::WebsocketCached));

        simple.base.transport = Some(Transport::Auto);
        let base = simple.build_base_options(&model, None);
        assert_eq!(base.transport, Some(Transport::Auto));
    }

    /// `stream_simple` must surface the websocket-not-implemented error
    /// when the caller asks for a WebSocket transport — anything else
    /// means the option was silently dropped and the call fell back to
    /// SSE behaviour against the real network.
    #[tokio::test]
    async fn stream_simple_with_websocket_transport_yields_not_implemented_error() {
        use crate::api_registry::ApiProvider;
        use futures::StreamExt;

        let provider = OpenAICodexResponsesProvider::new();
        let mut model = codex_test_model();
        model.base_url = "https://chatgpt.com/backend-api".to_string();

        let mut simple = SimpleStreamOptions::default();
        simple.base.api_key = Some("sk-fake".to_string());
        simple.base.transport = Some(Transport::Websocket);

        let mut stream = provider.stream_simple(
            model,
            codex_user_context(Some("You are an assistant.")),
            Some(simple),
        );

        let mut saw_websocket_error = false;
        while let Some(event) = stream.next().await {
            if let AssistantMessageEvent::Error { error, .. } = event {
                let msg = error.error_message.clone().unwrap_or_default();
                assert!(
                    msg.contains("WebSocket transport"),
                    "expected websocket-not-implemented error, got: {msg}"
                );
                saw_websocket_error = true;
                break;
            }
        }
        assert!(
            saw_websocket_error,
            "stream_simple must yield an Error event when transport is WebSocket"
        );
    }
}
