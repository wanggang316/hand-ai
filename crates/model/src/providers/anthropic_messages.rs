//! Anthropic Messages API provider.
//!
//! Implements streaming chat completions for the Anthropic Messages API.
//! Supports tool use, thinking/reasoning blocks, and cache control.

use crate::api_registry::AssistantMessageEventStream;
use crate::types::{
    Api, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, CacheRetention, Compat,
    Context, InputType, Message, Model, Provider, SimpleStreamOptions, StopReason, StreamOptions,
    TextContent, ThinkingContent, ThinkingLevel, Tool, ToolCall, ToolResultContent,
    UserContentBlock,
};
use crate::{env_api_keys, supports_xhigh};
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use serde_json::Value;
use std::collections::HashMap;

// =============================================================================
// Provider
// =============================================================================

/// Provider implementation for Anthropic Messages API.
#[derive(Debug, Clone, Copy, Default)]
pub struct AnthropicMessagesProvider;

impl AnthropicMessagesProvider {
    pub fn new() -> Self {
        Self
    }
}

impl crate::api_registry::ApiProvider for AnthropicMessagesProvider {
    fn stream(
        &self,
        model: Model,
        context: Context,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        Box::pin(stream_anthropic_messages_with_reasoning(
            model, context, options, None,
        ))
    }

    fn stream_simple(
        &self,
        model: Model,
        context: Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        let (base_options, reasoning) = match options {
            Some(opts) => {
                let api_key = env_api_keys::get_env_api_key(&model.provider);
                let base = opts.build_base_options(&model, api_key);
                (Some(base), opts.reasoning)
            }
            None => (None, None),
        };

        let reasoning = reasoning.map(|r| match r {
            ThinkingLevel::Xhigh if !supports_xhigh(&model) => ThinkingLevel::High,
            other => other,
        });

        Box::pin(stream_anthropic_messages_with_reasoning(
            model,
            context,
            base_options,
            reasoning,
        ))
    }
}

// =============================================================================
// Streaming
// =============================================================================

/// Stream Anthropic Messages API with reasoning support.
fn stream_anthropic_messages_with_reasoning(
    model: Model,
    context: Context,
    options: Option<StreamOptions>,
    reasoning: Option<ThinkingLevel>,
) -> impl futures::Stream<Item = AssistantMessageEvent> + Send + 'static {
    async_stream::stream! {
        let result = stream_anthropic_inner(model, context, options, reasoning).await;
        match result {
            Ok(events) => {
                for event in events {
                    yield event;
                }
            }
            Err(e) => {
                let error_msg = AssistantMessage {
                    role: "assistant".to_string(),
                    content: vec![],
                    api: Api::AnthropicMessages,
                    provider: Provider::Anthropic,
                    model: String::new(),
                    usage: crate::types::Usage::default(),
                    stop_reason: StopReason::Error,
                    error_message: Some(e),
                    timestamp: current_timestamp_ms(),
                    response_model: None,
                    response_id: None,
                    diagnostics: None,
                };
                yield AssistantMessageEvent::Error {
                    reason: StopReason::Error,
                    error: error_msg,
                };
            }
        }
    }
}

/// Inner streaming implementation that collects events.
async fn stream_anthropic_inner(
    model: Model,
    context: Context,
    options: Option<StreamOptions>,
    reasoning: Option<ThinkingLevel>,
) -> Result<Vec<AssistantMessageEvent>, String> {
    let api_key = options
        .as_ref()
        .and_then(|o| o.api_key.clone())
        .or_else(|| env_api_keys::get_env_api_key(&model.provider))
        .ok_or_else(|| "No API key found for Anthropic".to_string())?;

    let base_url = if model.base_url.is_empty() {
        "https://api.anthropic.com".to_string()
    } else {
        model.base_url.trim_end_matches('/').to_string()
    };

    let url = format!("{}/v1/messages", base_url);
    let max_tokens = options
        .as_ref()
        .and_then(|o| o.max_tokens)
        .unwrap_or(model.max_tokens.min(32000) as u32);

    // Build request body
    let body = build_request_body(&model, &context, max_tokens, reasoning, &options)?;

    // Build headers
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        "x-api-key",
        HeaderValue::from_str(&api_key).map_err(|e| e.to_string())?,
    );
    headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));

    // Add beta features.
    //
    // The `fine-grained-tool-streaming-2025-05-14` beta is the legacy
    // opt-in for eager tool argument streaming. When the model's compat
    // block opts in to per-tool `eager_input_streaming` (default on
    // direct Anthropic), the beta header is redundant and is omitted —
    // we only set it for legacy proxies that flip
    // `supportsEagerToolInputStreaming: false` and still need eager
    // streaming via the header.
    let mut betas: Vec<&'static str> = Vec::new();
    let has_tools = context
        .tools
        .as_ref()
        .is_some_and(|tools| !tools.is_empty());
    if has_tools && !crate::transform::supports_eager_tool_input_streaming(&model) {
        betas.push("fine-grained-tool-streaming-2025-05-14");
    }
    // Adaptive-thinking models (Opus 4.6+, Sonnet 4.6) interleave
    // thinking natively. The `interleaved-thinking-2025-05-14` beta
    // is deprecated on Opus 4.6 and redundant on Sonnet 4.6, so only
    // attach it for legacy reasoning models (Sonnet 3.7, Opus 4.0,
    // ...) where it's still required.
    if reasoning.is_some() && !supports_adaptive_thinking(&model.id) {
        betas.push("interleaved-thinking-2025-05-14");
    }
    if !betas.is_empty() {
        headers.insert(
            "anthropic-beta",
            HeaderValue::from_str(&betas.join(",")).map_err(|e| e.to_string())?,
        );
    }

    // Add custom headers from options
    if let Some(opts) = &options
        && let Some(custom_headers) = &opts.headers
    {
        for (key, value) in custom_headers {
            if let (Ok(name), Ok(val)) = (
                reqwest::header::HeaderName::from_bytes(key.as_bytes()),
                HeaderValue::from_str(value),
            ) {
                headers.insert(name, val);
            }
        }
    }

    // Send request
    let client = reqwest::Client::new();
    let response = client
        .post(&url)
        .headers(headers)
        .body(body.to_string())
        .send()
        .await
        .map_err(|e| format!("Request failed: {}", e))?;

    // `on_response` callback fires once the response headers are in,
    // regardless of HTTP status. Extensions use it to surface rate-limit
    // headers, request ids, retry-after hints, etc. Bypass anything that
    // isn't ASCII or fails to round-trip cleanly to a String — neither
    // the callback contract nor downstream observers expect non-UTF-8
    // header values.
    if let Some(on_response) = options.as_ref().and_then(|o| o.on_response.clone()) {
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
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "Failed to read error body".to_string());
        return Err(format!("Anthropic API error ({}): {}", status, body));
    }

    // Parse SSE stream
    parse_sse_stream(response, &model).await
}

// =============================================================================
// Request Building
// =============================================================================

fn build_request_body(
    model: &Model,
    context: &Context,
    max_tokens: u32,
    reasoning: Option<ThinkingLevel>,
    options: &Option<StreamOptions>,
) -> Result<Value, String> {
    let mut body = serde_json::Map::new();

    body.insert("model".to_string(), Value::String(model.id.clone()));
    body.insert("max_tokens".to_string(), Value::Number(max_tokens.into()));
    body.insert("stream".to_string(), Value::Bool(true));

    // System prompt — emit as an array of content blocks so the system
    // prefix can carry its own `cache_control` breakpoint when prompt
    // caching is enabled. Without a breakpoint here, Anthropic only
    // caches up to the tool list; long system prompts (HAND.md, skill
    // catalog, ...) re-bill on every turn.
    if let Some(system_prompt) = &context.system_prompt
        && !system_prompt.is_empty()
    {
        let cache_control = resolve_anthropic_cache_control(model, options.as_ref());
        if let Some(cc) = cache_control {
            body.insert(
                "system".to_string(),
                Value::Array(vec![serde_json::json!({
                    "type": "text",
                    "text": system_prompt,
                    "cache_control": cc,
                })]),
            );
        } else {
            body.insert("system".to_string(), Value::String(system_prompt.clone()));
        }
    }

    // Temperature.
    //
    // Extended thinking (adaptive OR budget-based) is incompatible
    // with the `temperature` field: the upstream rejects requests
    // that set both. When reasoning is enabled on a thinking-capable
    // model, drop the caller's `temperature` value rather than
    // letting the upstream reject the request.
    let thinking_enabled = reasoning.is_some() && model.reasoning;
    if let Some(opts) = options
        && let Some(temp) = opts.temperature
        && !thinking_enabled
    {
        body.insert(
            "temperature".to_string(),
            Value::Number(
                serde_json::Number::from_f64(temp as f64)
                    .unwrap_or_else(|| serde_json::Number::from(0)),
            ),
        );
    }

    // Messages — when prompt caching is enabled, the last user message's
    // final cacheable content block (text/image/tool_result) carries an
    // additional `cache_control` breakpoint so the entire conversation
    // prefix up to that point can be reused on the next turn.
    let mut messages = convert_messages_to_anthropic(&context.messages, model);
    if let Some(cc) = resolve_anthropic_cache_control(model, options.as_ref()) {
        apply_last_user_message_cache_control(&mut messages, &cc);
    }
    body.insert("messages".to_string(), Value::Array(messages));

    // Tools — attach `cache_control` to the LAST tool when prompt caching
    // is enabled. Anthropic uses the last cache breakpoint as the boundary
    // for what gets cached; placing it on the tool list lets tool schemas
    // be cached independently from the (frequently-changing) transcript.
    //
    // Also opt each tool into `eager_input_streaming: true` when the
    // model supports it (default on api.anthropic.com). Eager streaming
    // delivers partial tool arguments mid-stream so the agent can begin
    // dispatch as soon as `tool_use` is fully decoded; the legacy
    // `fine-grained-tool-streaming` beta is the older opt-in for the
    // same capability and is now expressed per-tool instead of via beta
    // header.
    if let Some(tools) = &context.tools
        && !tools.is_empty()
    {
        let cache_control = resolve_anthropic_cache_control(model, options.as_ref());
        let eager_streaming = crate::transform::supports_eager_tool_input_streaming(model);
        let last_idx = tools.len() - 1;
        let tool_defs: Vec<Value> = tools
            .iter()
            .enumerate()
            .map(|(idx, t)| {
                let mut tool_obj = convert_tool_to_anthropic(t);
                if let Some(obj) = tool_obj.as_object_mut() {
                    if eager_streaming {
                        obj.insert("eager_input_streaming".to_string(), Value::Bool(true));
                    }
                    if idx == last_idx
                        && let Some(cc) = &cache_control
                    {
                        obj.insert("cache_control".to_string(), cc.clone());
                    }
                }
                tool_obj
            })
            .collect();
        body.insert("tools".to_string(), Value::Array(tool_defs));
    }

    // Thinking/reasoning configuration. On reasoning-capable models,
    // an absent `reasoning` level is an explicit opt-out: send
    // `thinking: { type: "disabled" }` so newer Claudes (Opus 4.7,
    // Mythos Preview) don't fall back to their server-side default
    // of running thinking anyway and bill the caller for unwanted
    // thought tokens. Non-reasoning models stay clean.
    if model.reasoning {
        if let Some(level) = reasoning {
            let thinking_config = build_thinking_config(level, model);
            body.insert("thinking".to_string(), thinking_config);
        } else {
            body.insert(
                "thinking".to_string(),
                serde_json::json!({ "type": "disabled" }),
            );
        }
    }

    // Anthropic's API accepts an optional `metadata.user_id` for abuse
    // tracking and rate limiting. Forward a caller-supplied
    // `metadata["user_id"]` string from the generic StreamOptions
    // metadata bag; ignore non-string values so we never emit a
    // malformed `metadata` block.
    if let Some(opts) = options
        && let Some(meta) = opts.metadata.as_ref()
        && let Some(user_id) = meta.get("user_id").and_then(|v| v.as_str())
        && !user_id.is_empty()
    {
        body.insert(
            "metadata".to_string(),
            serde_json::json!({ "user_id": user_id }),
        );
    }

    Ok(Value::Object(body))
}

/// Whether the model opts in to Anthropic long-cache retention.
/// Defaults to `true` for native `api.anthropic.com` and any
/// `AnthropicMessagesCompat.supports_long_cache_retention = true`
/// override; explicit `Some(false)` disables it.
fn supports_long_cache_retention(model: &Model) -> bool {
    if let Some(Compat::AnthropicMessages(c)) = &model.compat
        && let Some(v) = c.supports_long_cache_retention
    {
        return v;
    }
    model.base_url.contains("api.anthropic.com")
}

/// Compute the `cache_control` JSON object to attach to the last tool
/// definition (and, eventually, system / last conversation message)
/// for Anthropic prompt caching. Returns `None` when caching is
/// disabled, so callers can skip emitting the field entirely.
///
/// Caching policy:
/// - `CacheRetention::None` → never emit `cache_control`.
/// - any other retention → emit `{type: ephemeral}`; when the resolved
///   value is `Long` *and* the model supports long retention, add
///   `ttl: "1h"` so the Anthropic backend keeps the breakpoint for an
///   hour instead of the default five-minute window.
pub(crate) fn resolve_anthropic_cache_control(
    model: &Model,
    options: Option<&StreamOptions>,
) -> Option<Value> {
    let retention = CacheRetention::resolve(options.and_then(|o| o.cache_retention));
    if retention == CacheRetention::None {
        return None;
    }
    let mut cc = serde_json::Map::new();
    cc.insert("type".to_string(), Value::String("ephemeral".to_string()));
    if retention == CacheRetention::Long && supports_long_cache_retention(model) {
        cc.insert("ttl".to_string(), Value::String("1h".to_string()));
    }
    Some(Value::Object(cc))
}

/// Attach a `cache_control` breakpoint to the trailing cacheable block
/// of the last user message in `messages`. No-op when the conversation
/// has no user message, or when the last user message's content array
/// has no text / image / tool_result block to mark.
///
/// Anthropic's documentation places the breakpoint on the LAST user
/// turn so the cached prefix covers the entire conversation up to that
/// point. Marking an assistant turn would shorten the cached prefix
/// and waste the breakpoint budget.
pub(crate) fn apply_last_user_message_cache_control(messages: &mut [Value], cc: &Value) {
    let Some(last_user_idx) = messages
        .iter()
        .rposition(|m| m.get("role").and_then(Value::as_str) == Some("user"))
    else {
        return;
    };
    let last_user = &mut messages[last_user_idx];
    let Some(obj) = last_user.as_object_mut() else {
        return;
    };
    let Some(content) = obj.get_mut("content") else {
        return;
    };
    // Anthropic accepts content as a string OR an array. The current
    // converter always emits the array form; defensively handle the
    // string case anyway so a future refactor that returns Content::Text
    // doesn't silently lose the breakpoint.
    if content.is_string() {
        let text = content.as_str().unwrap_or("").to_string();
        *content = Value::Array(vec![serde_json::json!({
            "type": "text",
            "text": text,
            "cache_control": cc.clone(),
        })]);
        return;
    }
    let Some(arr) = content.as_array_mut() else {
        return;
    };
    // Walk backwards to find the last text / image / tool_result block.
    for block in arr.iter_mut().rev() {
        let Some(block_obj) = block.as_object_mut() else {
            continue;
        };
        let block_type = block_obj.get("type").and_then(Value::as_str);
        if matches!(block_type, Some("text" | "image" | "tool_result")) {
            block_obj.insert("cache_control".to_string(), cc.clone());
            return;
        }
    }
}

/// Models that support adaptive thinking — Opus 4.6+ and Sonnet 4.6.
/// Older Opus 4.0 / 4.1 use budget-based thinking and the broader
/// `opus-4` substring match would mis-route them. The check stays
/// id-substring based so OpenRouter's `anthropic/claude-opus-4.6`
/// and the direct `claude-opus-4-6-20251022` ids both match.
pub(crate) fn supports_adaptive_thinking(model_id: &str) -> bool {
    model_id.contains("opus-4-6")
        || model_id.contains("opus-4.6")
        || model_id.contains("opus-4-7")
        || model_id.contains("opus-4.7")
        || model_id.contains("sonnet-4-6")
        || model_id.contains("sonnet-4.6")
}

fn build_thinking_config(level: ThinkingLevel, model: &Model) -> Value {
    let supports_adaptive = supports_adaptive_thinking(&model.id);

    // `display: "summarized"` keeps thinking text in the streamed
    // response. Anthropic's silent API default flipped to "omitted"
    // for newer Claudes (Opus 4.7, Mythos Preview), which strips the
    // text while still returning the encrypted signature for
    // multi-turn continuity. Pin "summarized" so behaviour matches
    // older Claude 4 models — UIs that surface the thinking trace
    // depend on it; the encrypted signature is unaffected either way.
    if supports_adaptive {
        // xhigh maps to two different effort names across the
        // adaptive-thinking generations:
        // - Opus 4.6 uses the legacy `max` effort (highest tier).
        // - Opus 4.7+ exposes `xhigh` natively; sending `max` on 4.7
        //   is rejected with "invalid effort".
        // Other adaptive-thinking models (Sonnet 4.6, ...) don't
        // support either xhigh OR max; clamp to `high` so the request
        // still passes validation.
        let is_opus_4_6 = model.id.contains("opus-4-6") || model.id.contains("opus-4.6");
        let is_opus_4_7 = model.id.contains("opus-4-7") || model.id.contains("opus-4.7");
        let effort = match level {
            ThinkingLevel::Minimal | ThinkingLevel::Low => "low",
            ThinkingLevel::Medium => "medium",
            ThinkingLevel::High => "high",
            ThinkingLevel::Xhigh => {
                if is_opus_4_6 {
                    "max"
                } else if is_opus_4_7 {
                    "xhigh"
                } else {
                    "high"
                }
            }
        };

        serde_json::json!({
            "type": "adaptive",
            "effort": effort,
            "display": "summarized",
        })
    } else {
        // Budget-based thinking for older models
        let budget = match level {
            ThinkingLevel::Minimal => 1024u32,
            ThinkingLevel::Low => 2048,
            ThinkingLevel::Medium => 8192,
            ThinkingLevel::High | ThinkingLevel::Xhigh => 16384,
        };

        serde_json::json!({
            "type": "enabled",
            "budget_tokens": budget,
            "display": "summarized",
        })
    }
}

// =============================================================================
// Message Conversion
// =============================================================================

/// Convert internal messages to Anthropic API format.
fn convert_messages_to_anthropic(messages: &[Message], model: &Model) -> Vec<Value> {
    let supports_images = model.input.contains(&InputType::Image);
    let mut result = Vec::new();
    let mut pending_tool_results: Vec<Value> = Vec::new();

    for msg in messages {
        match msg {
            Message::User(user_msg) => {
                // Flush any pending tool results first
                if !pending_tool_results.is_empty() {
                    result.push(serde_json::json!({
                        "role": "user",
                        "content": pending_tool_results,
                    }));
                    pending_tool_results = Vec::new();
                }

                let content = convert_user_content(user_msg, supports_images);
                result.push(serde_json::json!({
                    "role": "user",
                    "content": content,
                }));
            }
            Message::Assistant(asst_msg) => {
                // Flush pending tool results
                if !pending_tool_results.is_empty() {
                    result.push(serde_json::json!({
                        "role": "user",
                        "content": pending_tool_results,
                    }));
                    pending_tool_results = Vec::new();
                }

                let content = convert_assistant_content(asst_msg, model);
                if !content.is_empty() {
                    result.push(serde_json::json!({
                        "role": "assistant",
                        "content": content,
                    }));
                }
            }
            Message::ToolResult(tool_result) => {
                let content_blocks = convert_tool_result_content(&tool_result.content);
                let mut block = serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": normalize_tool_call_id(&tool_result.tool_call_id),
                    "content": content_blocks,
                });
                if tool_result.is_error {
                    block
                        .as_object_mut()
                        .unwrap()
                        .insert("is_error".to_string(), Value::Bool(true));
                }
                pending_tool_results.push(block);
            }
        }
    }

    // Flush remaining tool results
    if !pending_tool_results.is_empty() {
        result.push(serde_json::json!({
            "role": "user",
            "content": pending_tool_results,
        }));
    }

    result
}

fn convert_user_content(user_msg: &crate::types::UserMessage, supports_images: bool) -> Value {
    match &user_msg.content {
        crate::types::UserContent::Text(text) => Value::String(sanitize_surrogates(text)),
        crate::types::UserContent::Blocks(blocks) => {
            let content_blocks: Vec<Value> = blocks
                .iter()
                .filter_map(|block| match block {
                    UserContentBlock::Text(tc) => Some(serde_json::json!({
                        "type": "text",
                        "text": sanitize_surrogates(&tc.text),
                    })),
                    UserContentBlock::Image(img) if supports_images => Some(serde_json::json!({
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": &img.mime_type,
                            "data": &img.data,
                        }
                    })),
                    _ => None,
                })
                .collect();
            Value::Array(content_blocks)
        }
    }
}

fn convert_assistant_content(asst_msg: &AssistantMessage, model: &Model) -> Vec<Value> {
    let is_same_model = asst_msg.api == Api::AnthropicMessages
        && asst_msg.provider == model.provider
        && asst_msg.model == model.id;

    let mut blocks = Vec::new();

    for block in &asst_msg.content {
        match block {
            AssistantContentBlock::Text(tc) => {
                if !tc.text.is_empty() {
                    blocks.push(serde_json::json!({
                        "type": "text",
                        "text": sanitize_surrogates(&tc.text),
                    }));
                }
            }
            AssistantContentBlock::Thinking(tc) => {
                if is_same_model {
                    if let Some(sig) = &tc.thinking_signature {
                        if !sig.is_empty() {
                            // Only the in-band redacted marker replays as
                            // redacted_thinking (its signature holds the
                            // opaque `data` payload). A signed block whose
                            // text ended up empty (e.g. minimal thinking
                            // budget) is a normal thinking block: replaying
                            // it as redacted would pass a thinking signature
                            // where the API expects redacted data, and
                            // dropping it would lose the signature.
                            if tc.thinking.contains("[Reasoning redacted]") {
                                blocks.push(serde_json::json!({
                                    "type": "redacted_thinking",
                                    "data": sig,
                                }));
                            } else {
                                blocks.push(serde_json::json!({
                                    "type": "thinking",
                                    "thinking": &tc.thinking,
                                    "signature": sig,
                                }));
                            }
                        } else if !tc.thinking.is_empty() {
                            // No signature — emit as plain text without
                            // `<thinking>` tags. Wrapping unsigned thinking
                            // in tags teaches Claude to mimic the tag
                            // structure in its own replies (the model
                            // copies the historical shape).
                            blocks.push(serde_json::json!({
                                "type": "text",
                                "text": sanitize_surrogates(&tc.thinking),
                            }));
                        }
                    } else if !tc.thinking.is_empty() {
                        blocks.push(serde_json::json!({
                            "type": "text",
                            "text": sanitize_surrogates(&tc.thinking),
                        }));
                    }
                } else if !tc.thinking.is_empty() {
                    // Cross-model: signature is invalid on the target
                    // model. Convert to plain text without `<thinking>`
                    // tags so the new model doesn't mimic the wrapper.
                    blocks.push(serde_json::json!({
                        "type": "text",
                        "text": sanitize_surrogates(&tc.thinking),
                    }));
                }
            }
            AssistantContentBlock::ToolCall(tc) => {
                // Anthropic rejects `input: null` on `tool_use` blocks; replay
                // history may carry a Null arguments value when a previous turn
                // emitted an argless tool call. Default to an empty object so
                // the wire payload stays well-formed.
                let input = if tc.arguments.is_null() {
                    serde_json::Value::Object(serde_json::Map::new())
                } else {
                    tc.arguments.clone()
                };
                blocks.push(serde_json::json!({
                    "type": "tool_use",
                    "id": normalize_tool_call_id(&tc.id),
                    "name": &tc.name,
                    "input": input,
                }));
            }
        }
    }

    blocks
}

fn convert_tool_result_content(content: &[ToolResultContent]) -> Vec<Value> {
    // Anthropic rejects text blocks whose `text` is empty (min length 1),
    // so drop them instead of forwarding.
    let blocks: Vec<Value> = content
        .iter()
        .filter_map(|block| match block {
            ToolResultContent::Text(tc) if tc.text.is_empty() => None,
            ToolResultContent::Text(tc) => Some(serde_json::json!({
                "type": "text",
                "text": sanitize_surrogates(&tc.text),
            })),
            ToolResultContent::Image(img) => Some(serde_json::json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": &img.mime_type,
                    "data": &img.data,
                }
            })),
        })
        .collect();

    // Neither text nor images: emit an explicit placeholder so the model
    // can tell the tool ran and returned nothing.
    if blocks.is_empty() {
        return vec![serde_json::json!({
            "type": "text",
            "text": "(no tool output)",
        })];
    }
    blocks
}

fn convert_tool_to_anthropic(tool: &Tool) -> Value {
    serde_json::json!({
        "name": &tool.name,
        "description": &tool.description,
        "input_schema": &tool.parameters,
    })
}

/// Normalize tool call ID to match Anthropic requirements.
/// Must be alphanumeric, dash, or underscore, max 64 chars.
fn normalize_tool_call_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect()
}

/// Remove unpaired Unicode surrogates to prevent JSON serialization errors.
fn sanitize_surrogates(text: &str) -> String {
    // Rust strings are always valid UTF-8, so unpaired surrogates can't exist.
    // This is a no-op in Rust but included for parity with the TypeScript version.
    text.to_string()
}

// =============================================================================
// SSE Response Parsing
// =============================================================================

/// Parse SSE stream from Anthropic API response.
async fn parse_sse_stream(
    response: reqwest::Response,
    model: &Model,
) -> Result<Vec<AssistantMessageEvent>, String> {
    let body = response.text().await.map_err(|e| e.to_string())?;
    parse_sse_body(&body, model)
}

/// Synchronous body parser split out so it is unit-testable without
/// stubbing a `reqwest::Response`.
fn parse_sse_body(body: &str, model: &Model) -> Result<Vec<AssistantMessageEvent>, String> {
    let mut events = Vec::new();
    let mut output = AssistantMessage {
        role: "assistant".to_string(),
        content: vec![],
        api: Api::AnthropicMessages,
        provider: model.provider,
        model: model.id.clone(),
        usage: crate::types::Usage::default(),
        stop_reason: StopReason::Stop,
        error_message: None,
        timestamp: current_timestamp_ms(),
        response_model: None,
        response_id: None,
        diagnostics: None,
    };

    let mut content_blocks: HashMap<usize, ContentBlockState> = HashMap::new();
    let mut current_stop_reason = StopReason::Stop;
    let mut saw_message_start = false;
    let mut saw_message_stop = false;

    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(data) = line.strip_prefix("data: ") {
            if data == "[DONE]" {
                break;
            }

            let event: Value = match serde_json::from_str(data) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");

            match event_type {
                "message_start" => {
                    saw_message_start = true;
                    if let Some(msg) = event.get("message") {
                        // Parse usage from message_start
                        if let Some(usage) = msg.get("usage") {
                            output.usage.input = usage
                                .get("input_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                            output.usage.cache_read = usage
                                .get("cache_read_input_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                            output.usage.cache_write = usage
                                .get("cache_creation_input_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                        }

                        // Parse model from response
                        if let Some(m) = msg.get("model").and_then(|v| v.as_str()) {
                            output.model = m.to_string();
                        }

                        // Capture the provider-assigned response id
                        // (`msg_...`) so callers can correlate this
                        // assistant turn with Anthropic's own logging
                        // / observability — and so downstream tooling
                        // can replay the exact response when needed.
                        if let Some(id) = msg.get("id").and_then(|v| v.as_str())
                            && !id.is_empty()
                            && output.response_id.is_none()
                        {
                            output.response_id = Some(id.to_string());
                        }
                    }

                    events.push(AssistantMessageEvent::Start {
                        partial: output.clone(),
                    });
                }

                "content_block_start" => {
                    let index = event.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    let content_block = event.get("content_block").unwrap_or(&Value::Null);
                    let block_type = content_block
                        .get("type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    match block_type {
                        "text" => {
                            let text = content_block
                                .get("text")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let text_content = TextContent::new(&text);
                            content_blocks
                                .insert(index, ContentBlockState::Text(text_content.clone()));
                            output
                                .content
                                .push(AssistantContentBlock::Text(text_content));
                            events.push(AssistantMessageEvent::TextStart {
                                content_index: index as u32,
                                partial: output.clone(),
                            });
                        }
                        "thinking" => {
                            let thinking = content_block
                                .get("thinking")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let thinking_content = ThinkingContent::new(&thinking);
                            content_blocks.insert(
                                index,
                                ContentBlockState::Thinking(thinking_content.clone()),
                            );
                            output
                                .content
                                .push(AssistantContentBlock::Thinking(thinking_content));
                            events.push(AssistantMessageEvent::ThinkingStart {
                                content_index: index as u32,
                                partial: output.clone(),
                            });
                        }
                        "redacted_thinking" => {
                            let data = content_block
                                .get("data")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let mut thinking_content = ThinkingContent::new("[Reasoning redacted]");
                            thinking_content.thinking_signature = Some(data);
                            content_blocks.insert(
                                index,
                                ContentBlockState::Thinking(thinking_content.clone()),
                            );
                            output
                                .content
                                .push(AssistantContentBlock::Thinking(thinking_content));
                            events.push(AssistantMessageEvent::ThinkingStart {
                                content_index: index as u32,
                                partial: output.clone(),
                            });
                        }
                        "tool_use" => {
                            let id = content_block
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let name = content_block
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let tool_call = ToolCall::new(&id, &name, serde_json::json!({}));
                            content_blocks.insert(
                                index,
                                ContentBlockState::ToolCall(tool_call.clone(), String::new()),
                            );
                            output
                                .content
                                .push(AssistantContentBlock::ToolCall(tool_call));
                            events.push(AssistantMessageEvent::ToolCallStart {
                                content_index: index as u32,
                                partial: output.clone(),
                            });
                        }
                        _ => {}
                    }
                }

                "content_block_delta" => {
                    let index = event.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    let delta = event.get("delta").unwrap_or(&Value::Null);
                    let delta_type = delta.get("type").and_then(|v| v.as_str()).unwrap_or("");

                    match delta_type {
                        "text_delta" => {
                            let text = delta.get("text").and_then(|v| v.as_str()).unwrap_or("");
                            if let Some(ContentBlockState::Text(tc)) =
                                content_blocks.get_mut(&index)
                            {
                                tc.text.push_str(text);
                            }
                            // Update the content block in output
                            if let Some(AssistantContentBlock::Text(tc)) =
                                output.content.get_mut(index)
                            {
                                tc.text.push_str(text);
                            }
                            events.push(AssistantMessageEvent::TextDelta {
                                content_index: index as u32,
                                delta: text.to_string(),
                                partial: output.clone(),
                            });
                        }
                        "thinking_delta" => {
                            let thinking =
                                delta.get("thinking").and_then(|v| v.as_str()).unwrap_or("");
                            if let Some(ContentBlockState::Thinking(tc)) =
                                content_blocks.get_mut(&index)
                            {
                                tc.thinking.push_str(thinking);
                            }
                            if let Some(AssistantContentBlock::Thinking(tc)) =
                                output.content.get_mut(index)
                            {
                                tc.thinking.push_str(thinking);
                            }
                            events.push(AssistantMessageEvent::ThinkingDelta {
                                content_index: index as u32,
                                delta: thinking.to_string(),
                                partial: output.clone(),
                            });
                        }
                        "signature_delta" => {
                            let signature = delta
                                .get("signature")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            if let Some(ContentBlockState::Thinking(tc)) =
                                content_blocks.get_mut(&index)
                            {
                                let existing =
                                    tc.thinking_signature.get_or_insert_with(String::new);
                                existing.push_str(signature);
                            }
                            if let Some(AssistantContentBlock::Thinking(tc)) =
                                output.content.get_mut(index)
                            {
                                let existing =
                                    tc.thinking_signature.get_or_insert_with(String::new);
                                existing.push_str(signature);
                            }
                        }
                        "input_json_delta" => {
                            let partial_json = delta
                                .get("partial_json")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            if let Some(ContentBlockState::ToolCall(_, args_buf)) =
                                content_blocks.get_mut(&index)
                            {
                                args_buf.push_str(partial_json);
                            }
                            events.push(AssistantMessageEvent::ToolCallDelta {
                                content_index: index as u32,
                                delta: partial_json.to_string(),
                                partial: output.clone(),
                            });
                        }
                        _ => {}
                    }
                }

                "content_block_stop" => {
                    let index = event.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

                    if let Some(block_state) = content_blocks.remove(&index) {
                        match block_state {
                            ContentBlockState::Text(tc) => {
                                // Update final state
                                if let Some(AssistantContentBlock::Text(out_tc)) =
                                    output.content.get_mut(index)
                                {
                                    *out_tc = tc.clone();
                                }
                                events.push(AssistantMessageEvent::TextEnd {
                                    content_index: index as u32,
                                    content: tc.text,
                                    partial: output.clone(),
                                });
                            }
                            ContentBlockState::Thinking(tc) => {
                                if let Some(AssistantContentBlock::Thinking(out_tc)) =
                                    output.content.get_mut(index)
                                {
                                    *out_tc = tc.clone();
                                }
                                events.push(AssistantMessageEvent::ThinkingEnd {
                                    content_index: index as u32,
                                    content: tc.thinking,
                                    partial: output.clone(),
                                });
                            }
                            ContentBlockState::ToolCall(mut tc, args_buf) => {
                                // Final parse of accumulated JSON. Models
                                // sometimes emit raw control bytes or invalid
                                // backslash escapes inside `input_json_delta`
                                // payloads; fall back to the repair pass so
                                // we don't silently drop the entire tool call
                                // to `{}`.
                                if !args_buf.is_empty() {
                                    tc.arguments =
                                        crate::transform::parse_json_with_repair(&args_buf)
                                            .unwrap_or(serde_json::json!({}));
                                }
                                if let Some(AssistantContentBlock::ToolCall(out_tc)) =
                                    output.content.get_mut(index)
                                {
                                    out_tc.arguments = tc.arguments.clone();
                                    out_tc.name = tc.name.clone();
                                }
                                events.push(AssistantMessageEvent::ToolCallEnd {
                                    content_index: index as u32,
                                    tool_call: tc,
                                    partial: output.clone(),
                                });
                            }
                        }
                    }
                }

                "message_delta" => {
                    if let Some(delta) = event.get("delta") {
                        let stop_reason_str = delta.get("stop_reason").and_then(|v| v.as_str());
                        current_stop_reason = match stop_reason_str {
                            Some("end_turn") => StopReason::Stop,
                            Some("max_tokens") => StopReason::Length,
                            Some("tool_use") => StopReason::ToolUse,
                            Some("stop_sequence") => StopReason::Stop,
                            Some("refusal") | Some("sensitive") => StopReason::Error,
                            _ => StopReason::Stop,
                        };
                    }

                    // Update usage from message_delta
                    if let Some(usage) = event.get("usage") {
                        if let Some(output_tokens) =
                            usage.get("output_tokens").and_then(|v| v.as_u64())
                        {
                            output.usage.output = output_tokens;
                        }
                        if let Some(cache_read) = usage
                            .get("cache_read_input_tokens")
                            .and_then(|v| v.as_u64())
                        {
                            output.usage.cache_read = cache_read;
                        }
                        if let Some(cache_write) = usage
                            .get("cache_creation_input_tokens")
                            .and_then(|v| v.as_u64())
                        {
                            output.usage.cache_write = cache_write;
                        }
                        output.usage.total_tokens = output.usage.input
                            + output.usage.output
                            + output.usage.cache_read
                            + output.usage.cache_write;
                    }
                }

                "message_stop" => {
                    saw_message_stop = true;
                    // Final message
                    output.stop_reason = current_stop_reason;

                    // Calculate cost
                    crate::models::calculate_cost(model, &mut output.usage);

                    events.push(AssistantMessageEvent::Done {
                        reason: current_stop_reason,
                        message: output.clone(),
                    });
                }

                "error" => {
                    let error_msg = event
                        .get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown error")
                        .to_string();
                    output.stop_reason = StopReason::Error;
                    output.error_message = Some(error_msg);
                    events.push(AssistantMessageEvent::Error {
                        reason: StopReason::Error,
                        error: output.clone(),
                    });
                }

                _ => {}
            }
        }
    }

    // A stream that opened with `message_start` but closed before
    // `message_stop` is an incomplete response — the upstream's
    // connection dropped mid-message. Surfacing it as a successful
    // partial silently truncates the assistant turn. Promote to an
    // Error event so callers can retry or fail loudly.
    if saw_message_start && !saw_message_stop {
        output.stop_reason = StopReason::Error;
        output.error_message = Some("Anthropic stream ended before message_stop".to_string());
        crate::models::calculate_cost(model, &mut output.usage);
        events.push(AssistantMessageEvent::Error {
            reason: StopReason::Error,
            error: output,
        });
        return Ok(events);
    }

    // If no Done event was emitted, emit one
    if !events.iter().any(|e| {
        matches!(
            e,
            AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. }
        )
    }) {
        output.stop_reason = current_stop_reason;
        crate::models::calculate_cost(model, &mut output.usage);
        events.push(AssistantMessageEvent::Done {
            reason: current_stop_reason,
            message: output,
        });
    }

    Ok(events)
}

/// State for tracking content blocks during streaming.
enum ContentBlockState {
    Text(TextContent),
    Thinking(ThinkingContent),
    ToolCall(ToolCall, String), // ToolCall + accumulated partial JSON args
}

fn current_timestamp_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        AnthropicMessagesCompat, AssistantContentBlock, Compat, Cost, InputType, ToolResultMessage,
        UserMessage,
    };

    fn test_model() -> Model {
        Model {
            id: "claude-sonnet-4-20250514".to_string(),
            name: "Claude Sonnet 4".to_string(),
            api: Api::AnthropicMessages,
            provider: Provider::Anthropic,
            base_url: "https://api.anthropic.com".to_string(),
            reasoning: true,
            input: vec![InputType::Text, InputType::Image],
            cost: Cost {
                input: 3.0,
                output: 15.0,
                cache_read: 0.3,
                cache_write: 3.75,
            },
            context_window: 200000,
            max_tokens: 64000,
            headers: None,
            compat: None,
            thinking_level_map: None,
        }
    }

    /// A stream that opens with `message_start` and then ends mid-flight
    /// (no `message_stop`) is an incomplete response. The previous behavior
    /// synthesized a clean Done event from whatever partial state had
    /// accumulated, silently truncating the assistant turn. Pin the new
    /// loud-Error behavior so a retry layer or human can react.
    #[test]
    fn parse_sse_body_treats_premature_eof_as_error() {
        let body = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10},\"model\":\"claude-test\"}}\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n",
        );
        let events = parse_sse_body(body, &test_model()).expect("parse should succeed");
        let last = events.last().expect("at least one event");
        match last {
            AssistantMessageEvent::Error { reason, error } => {
                assert_eq!(*reason, StopReason::Error);
                assert!(
                    error
                        .error_message
                        .as_deref()
                        .unwrap_or("")
                        .contains("message_stop"),
                    "error message must mention message_stop: {:?}",
                    error.error_message
                );
            }
            other => panic!("expected Error event, got {other:?}"),
        }
    }

    /// A clean stream (`message_start` … `message_stop`) must still finish
    /// with a normal Done event — the new error path is scoped strictly
    /// to the missing-stop case.
    #[test]
    fn parse_sse_body_emits_done_when_stream_completes_cleanly() {
        let body = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10},\"model\":\"claude-test\"}}\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n",
            "data: {\"type\":\"message_stop\"}\n",
        );
        let events = parse_sse_body(body, &test_model()).expect("parse should succeed");
        let last = events.last().expect("at least one event");
        assert!(
            matches!(last, AssistantMessageEvent::Done { .. }),
            "expected Done, got {last:?}"
        );
    }

    /// The `message_start` event carries the provider-assigned
    /// response id (`msg_...`). Surfacing it on the assistant message
    /// lets callers correlate the turn with Anthropic's own logging
    /// and lets downstream tooling replay the exact response.
    #[test]
    fn parse_sse_body_captures_response_id_from_message_start() {
        let body = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_abc123\",\"usage\":{\"input_tokens\":10},\"model\":\"claude-test\"}}\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n",
            "data: {\"type\":\"message_stop\"}\n",
        );
        let events = parse_sse_body(body, &test_model()).expect("parse should succeed");
        let done = events
            .last()
            .and_then(|e| match e {
                AssistantMessageEvent::Done { message, .. } => Some(message),
                _ => None,
            })
            .expect("Done event with message");
        assert_eq!(done.response_id.as_deref(), Some("msg_abc123"));
    }

    /// Streams that omit the response id (older Anthropic backends,
    /// proxies that strip the field) must still parse cleanly with
    /// `response_id == None` — no panic, no error.
    #[test]
    fn parse_sse_body_leaves_response_id_none_when_missing() {
        let body = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10},\"model\":\"claude-test\"}}\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n",
            "data: {\"type\":\"message_stop\"}\n",
        );
        let events = parse_sse_body(body, &test_model()).expect("parse should succeed");
        let done = events
            .last()
            .and_then(|e| match e {
                AssistantMessageEvent::Done { message, .. } => Some(message),
                _ => None,
            })
            .expect("Done event with message");
        assert!(done.response_id.is_none());
    }

    #[test]
    fn test_normalize_tool_call_id() {
        assert_eq!(normalize_tool_call_id("call_123"), "call_123");
        assert_eq!(normalize_tool_call_id("call.123"), "call_123");
        assert_eq!(
            normalize_tool_call_id("a".repeat(100).as_str()),
            "a".repeat(64)
        );
    }

    #[test]
    fn test_convert_user_text_message() {
        let msgs = vec![Message::User(UserMessage::new_text("Hello"))];
        let model = test_model();
        let result = convert_messages_to_anthropic(&msgs, &model);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["role"], "user");
        assert_eq!(result[0]["content"], "Hello");
    }

    #[test]
    fn test_convert_tool_result_message() {
        let msgs = vec![Message::ToolResult(ToolResultMessage::new(
            "call_1",
            "read",
            vec![ToolResultContent::Text(TextContent::new("file content"))],
        ))];
        let model = test_model();
        let result = convert_messages_to_anthropic(&msgs, &model);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["role"], "user");
        let content = result[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[0]["tool_use_id"], "call_1");
    }

    /// Empty tool results (no text, no images) must emit an explicit
    /// "(no tool output)" text block. Anthropic rejects text blocks
    /// whose `text` is empty, and the model otherwise can't tell the
    /// tool ran and returned nothing.
    #[test]
    fn empty_tool_result_gets_no_output_placeholder() {
        let model = test_model();
        let msgs = vec![
            // An empty text block and no content at all must both hit
            // the placeholder.
            Message::ToolResult(ToolResultMessage::new(
                "call_1",
                "bash",
                vec![ToolResultContent::Text(TextContent::new(""))],
            )),
            Message::ToolResult(ToolResultMessage::new("call_2", "bash", vec![])),
        ];
        let result = convert_messages_to_anthropic(&msgs, &model);
        assert_eq!(result.len(), 1);
        let content = result[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        for block in content {
            assert_eq!(block["type"], "tool_result");
            let inner = block["content"].as_array().unwrap();
            assert_eq!(inner.len(), 1, "exactly one placeholder block: {inner:?}");
            assert_eq!(inner[0]["type"], "text");
            assert_eq!(inner[0]["text"], "(no tool output)");
        }
    }

    /// A tool result carrying an image next to an empty text block keeps
    /// the image and drops the empty text — no placeholder is added when
    /// real content is present.
    #[test]
    fn image_only_tool_result_keeps_image_without_placeholder() {
        use crate::types::ImageContent;
        let model = test_model();
        let msgs = vec![Message::ToolResult(ToolResultMessage::new(
            "call_img",
            "screenshot",
            vec![
                ToolResultContent::Text(TextContent::new("")),
                ToolResultContent::Image(ImageContent::new("ZmFrZQ==", "image/png")),
            ],
        ))];
        let result = convert_messages_to_anthropic(&msgs, &model);
        let content = result[0]["content"].as_array().unwrap();
        let inner = content[0]["content"].as_array().unwrap();
        assert_eq!(
            inner.len(),
            1,
            "empty text block must be dropped: {inner:?}"
        );
        assert_eq!(inner[0]["type"], "image");
    }

    #[test]
    fn test_convert_assistant_with_tool_call() {
        let model = test_model();
        let asst = AssistantMessage {
            role: "assistant".to_string(),
            content: vec![
                AssistantContentBlock::Text(TextContent::new("Let me read that file.")),
                AssistantContentBlock::ToolCall(ToolCall::new(
                    "call_1",
                    "read",
                    serde_json::json!({"path": "test.rs"}),
                )),
            ],
            api: Api::AnthropicMessages,
            provider: Provider::Anthropic,
            model: "claude-sonnet-4-20250514".to_string(),
            usage: crate::types::Usage::default(),
            stop_reason: StopReason::ToolUse,
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        };

        let msgs = vec![Message::Assistant(asst)];
        let result = convert_messages_to_anthropic(&msgs, &model);
        assert_eq!(result.len(), 1);
        let content = result[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "tool_use");
        assert_eq!(content[1]["name"], "read");
    }

    /// Anthropic rejects `tool_use` blocks whose `input` is `null` (the API
    /// requires an object). Replay history can carry a Null arguments value
    /// when a previous turn issued an argless tool call. The converter must
    /// emit `{}` in that case so the wire payload stays well-formed.
    #[test]
    fn test_convert_assistant_tool_call_defaults_null_input_to_empty_object() {
        let model = test_model();
        let asst = AssistantMessage {
            role: "assistant".to_string(),
            content: vec![AssistantContentBlock::ToolCall(ToolCall::new(
                "call_x",
                "now",
                serde_json::Value::Null,
            ))],
            api: Api::AnthropicMessages,
            provider: Provider::Anthropic,
            model: "claude-sonnet-4-20250514".to_string(),
            usage: crate::types::Usage::default(),
            stop_reason: StopReason::ToolUse,
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        };
        let msgs = vec![Message::Assistant(asst)];
        let result = convert_messages_to_anthropic(&msgs, &model);
        let content = result[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "tool_use");
        assert!(
            content[0]["input"].is_object(),
            "input must be an object, got: {}",
            content[0]["input"]
        );
        assert_eq!(content[0]["input"].as_object().unwrap().len(), 0);
    }

    #[test]
    fn test_convert_tool_to_anthropic() {
        let tool = Tool::new(
            "read",
            "Read a file",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"]
            }),
        );
        let result = convert_tool_to_anthropic(&tool);
        assert_eq!(result["name"], "read");
        assert_eq!(result["description"], "Read a file");
        assert!(result["input_schema"].is_object());
    }

    #[test]
    fn test_build_thinking_config_adaptive() {
        // Opus 4.6+ and Sonnet 4.6 use adaptive thinking; older Opus
        // 4.0 / 4.1 still use the budget-based path.
        let model = Model {
            id: "claude-opus-4-6-20251022".to_string(),
            ..test_model()
        };
        let config = build_thinking_config(ThinkingLevel::High, &model);
        assert_eq!(config["type"], "adaptive");
        assert_eq!(config["effort"], "high");
    }

    /// Older Opus 4.0 / 4.1 don't support adaptive thinking — they
    /// route through the legacy budget-based path. The substring
    /// check previously matched `opus-4` and mis-classified them.
    #[test]
    fn test_build_thinking_config_opus_4_0_uses_budget_not_adaptive() {
        let model = Model {
            id: "claude-opus-4-20250514".to_string(),
            ..test_model()
        };
        let config = build_thinking_config(ThinkingLevel::High, &model);
        assert_eq!(config["type"], "enabled");
        assert!(config["budget_tokens"].as_u64().is_some());
    }

    #[test]
    fn test_build_thinking_config_budget() {
        let model = Model {
            id: "claude-3-5-sonnet-20241022".to_string(),
            ..test_model()
        };
        let config = build_thinking_config(ThinkingLevel::Medium, &model);
        assert_eq!(config["type"], "enabled");
        assert_eq!(config["budget_tokens"], 8192);
    }

    /// Anthropic silently changed the API default for thinking
    /// `display` from "summarized" to "omitted" on newer Claudes
    /// (Opus 4.7, Mythos Preview). "omitted" strips the thinking text
    /// while still returning the encrypted signature, so multi-turn
    /// continuity works but UIs that surface the trace see empty
    /// blocks. Pin "summarized" on every emitted thinking config so
    /// the visible behaviour matches older Claude 4 models — UIs that
    /// depend on the trace keep working.
    #[test]
    fn thinking_config_pins_display_summarized_on_adaptive_branch() {
        let model = Model {
            id: "claude-opus-4-7-20260101".to_string(),
            ..test_model()
        };
        let config = build_thinking_config(ThinkingLevel::High, &model);
        assert_eq!(config["type"], "adaptive");
        assert_eq!(
            config["display"], "summarized",
            "adaptive thinking must pin display: summarized: {config}"
        );
    }

    /// `xhigh` maps to different adaptive effort names across the
    /// Opus generations:
    /// - Opus 4.6 uses the legacy `"max"` effort (highest tier).
    /// - Opus 4.7 exposes `"xhigh"` natively; sending `"max"` on 4.7
    ///   would be rejected as an invalid effort name.
    #[test]
    fn thinking_config_xhigh_maps_to_max_on_opus_4_6() {
        let model = Model {
            id: "claude-opus-4-6-20251022".to_string(),
            ..test_model()
        };
        let config = build_thinking_config(ThinkingLevel::Xhigh, &model);
        assert_eq!(config["type"], "adaptive");
        assert_eq!(config["effort"], "max");
    }

    #[test]
    fn thinking_config_xhigh_maps_to_xhigh_on_opus_4_7() {
        let model = Model {
            id: "claude-opus-4-7-20260101".to_string(),
            ..test_model()
        };
        let config = build_thinking_config(ThinkingLevel::Xhigh, &model);
        assert_eq!(config["type"], "adaptive");
        assert_eq!(config["effort"], "xhigh");
    }

    /// Other adaptive-thinking models (Sonnet 4.6, ...) don't accept
    /// xhigh OR max. Clamp to `high` so the request stays valid.
    #[test]
    fn thinking_config_xhigh_clamps_to_high_on_sonnet_4_6() {
        let model = Model {
            id: "claude-sonnet-4-6-20251022".to_string(),
            ..test_model()
        };
        let config = build_thinking_config(ThinkingLevel::Xhigh, &model);
        assert_eq!(config["type"], "adaptive");
        assert_eq!(config["effort"], "high");
    }

    /// Extended thinking is incompatible with `temperature`: the
    /// upstream rejects requests that set both. When reasoning is
    /// enabled on a thinking-capable model, the builder must drop
    /// the caller's `temperature` value rather than letting the
    /// request fail.
    #[test]
    fn build_request_body_drops_temperature_when_thinking_enabled() {
        let model = test_model();
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new_text("hi"))],
            tools: None,
        };
        let opts = StreamOptions {
            temperature: Some(0.7),
            ..Default::default()
        };
        let body = build_request_body(
            &model,
            &context,
            4096,
            Some(ThinkingLevel::High),
            &Some(opts),
        )
        .unwrap();
        assert!(
            body.get("temperature").is_none(),
            "temperature must be dropped when thinking is enabled: {body}"
        );
        assert!(body.get("thinking").is_some(), "thinking must be set");
    }

    /// Without reasoning, temperature must still flow through.
    #[test]
    fn build_request_body_keeps_temperature_when_thinking_off() {
        let model = test_model();
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new_text("hi"))],
            tools: None,
        };
        let opts = StreamOptions {
            temperature: Some(0.7),
            ..Default::default()
        };
        let body = build_request_body(&model, &context, 4096, None, &Some(opts)).unwrap();
        assert_eq!(
            body["temperature"]
                .as_f64()
                .map(|v| (v * 10.0).round() / 10.0),
            Some(0.7)
        );
    }

    /// `supports_adaptive_thinking` recognises Opus 4.6+, Opus 4.7,
    /// and Sonnet 4.6 (both dashed and dotted ids). Older Opus 4.0,
    /// Sonnet 3.7, Haiku 3.5 don't qualify.
    #[test]
    fn supports_adaptive_thinking_recognises_4_6_plus() {
        for id in [
            "claude-opus-4-6",
            "claude-opus-4.6",
            "claude-opus-4-7",
            "claude-opus-4.7",
            "claude-sonnet-4-6",
            "claude-sonnet-4.6",
            "anthropic/claude-opus-4.7",
        ] {
            assert!(supports_adaptive_thinking(id), "{id} should be adaptive");
        }
        for id in [
            "claude-opus-4",
            "claude-opus-4-0",
            "claude-opus-4-1",
            "claude-sonnet-4",
            "claude-sonnet-3-7",
            "claude-3-5-haiku",
        ] {
            assert!(!supports_adaptive_thinking(id), "{id} must NOT be adaptive");
        }
    }

    /// Anthropic's newer Claudes (Opus 4.7, Mythos Preview) run
    /// thinking by default when no `thinking` field is sent. That
    /// charges the caller for unwanted thought tokens on every
    /// request that omitted the reasoning level. Emit
    /// `thinking: { type: "disabled" }` explicitly on reasoning-
    /// capable models when no reasoning level is set, so the model
    /// stays silent unless the caller asked for thinking.
    #[test]
    fn build_request_body_explicitly_disables_thinking_when_off() {
        let model = test_model();
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new_text("hi"))],
            tools: None,
        };
        // reasoning = None → opt-out
        let body = build_request_body(&model, &context, 4096, None, &None).unwrap();
        assert_eq!(
            body["thinking"],
            serde_json::json!({ "type": "disabled" }),
            "reasoning-capable model with no level must send thinking: disabled: {body}"
        );
    }

    /// Non-reasoning models must not carry a `thinking` field at all
    /// — the upstream rejects it on models that don't support
    /// thinking.
    #[test]
    fn build_request_body_omits_thinking_on_non_reasoning_model() {
        let mut model = test_model();
        model.reasoning = false;
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new_text("hi"))],
            tools: None,
        };
        let body = build_request_body(&model, &context, 4096, None, &None).unwrap();
        assert!(
            body.get("thinking").is_none(),
            "non-reasoning model must NOT emit thinking: {body}"
        );
    }

    /// Anthropic accepts `metadata.user_id` for abuse tracking and rate
    /// limiting. When the caller threads `metadata["user_id"]` through
    /// `StreamOptions`, the request body must surface it as a top-level
    /// `metadata: { user_id: ... }` object.
    #[test]
    fn build_request_body_forwards_metadata_user_id() {
        let model = test_model();
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new_text("hi"))],
            tools: None,
        };
        let mut metadata = std::collections::HashMap::new();
        metadata.insert(
            "user_id".to_string(),
            serde_json::Value::String("u_42".to_string()),
        );
        let opts = StreamOptions {
            metadata: Some(metadata),
            ..Default::default()
        };
        let body = build_request_body(&model, &context, 4096, None, &Some(opts)).unwrap();
        assert_eq!(
            body["metadata"]["user_id"], "u_42",
            "metadata.user_id must flow into the request body: {body}"
        );
    }

    /// Non-string `metadata["user_id"]` values (numbers, objects,
    /// nulls) are ignored — we never emit a malformed `metadata` block.
    #[test]
    fn build_request_body_ignores_non_string_user_id() {
        let model = test_model();
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new_text("hi"))],
            tools: None,
        };
        let mut metadata = std::collections::HashMap::new();
        metadata.insert(
            "user_id".to_string(),
            serde_json::Value::Number(serde_json::Number::from(42)),
        );
        let opts = StreamOptions {
            metadata: Some(metadata),
            ..Default::default()
        };
        let body = build_request_body(&model, &context, 4096, None, &Some(opts)).unwrap();
        assert!(
            body.get("metadata").is_none(),
            "non-string user_id must not emit metadata: {body}"
        );
    }

    /// Empty-string `user_id` values are dropped too — Anthropic would
    /// reject `metadata: { user_id: "" }`.
    #[test]
    fn build_request_body_drops_empty_user_id() {
        let model = test_model();
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new_text("hi"))],
            tools: None,
        };
        let mut metadata = std::collections::HashMap::new();
        metadata.insert(
            "user_id".to_string(),
            serde_json::Value::String(String::new()),
        );
        let opts = StreamOptions {
            metadata: Some(metadata),
            ..Default::default()
        };
        let body = build_request_body(&model, &context, 4096, None, &Some(opts)).unwrap();
        assert!(
            body.get("metadata").is_none(),
            "empty user_id must not emit metadata: {body}"
        );
    }

    #[test]
    fn thinking_config_pins_display_summarized_on_budget_branch() {
        let model = Model {
            id: "claude-3-5-haiku-20251022".to_string(),
            ..test_model()
        };
        let config = build_thinking_config(ThinkingLevel::Medium, &model);
        assert_eq!(config["type"], "enabled");
        assert_eq!(
            config["display"], "summarized",
            "budget-based thinking must pin display: summarized: {config}"
        );
    }

    #[test]
    fn test_build_request_body() {
        let model = test_model();
        let context = Context {
            system_prompt: Some("You are helpful.".to_string()),
            messages: vec![Message::User(UserMessage::new_text("Hello"))],
            tools: None,
        };
        let body = build_request_body(&model, &context, 4096, None, &None).unwrap();
        assert_eq!(body["model"], "claude-sonnet-4-20250514");
        assert_eq!(body["max_tokens"], 4096);
        assert_eq!(body["stream"], true);
        // Default caching is `Short`, so the system prompt now ships as
        // a single content block with a `cache_control` breakpoint.
        let sys = body["system"].as_array().unwrap();
        assert_eq!(sys.len(), 1);
        assert_eq!(sys[0]["type"], "text");
        assert_eq!(sys[0]["text"], "You are helpful.");
        assert_eq!(sys[0]["cache_control"]["type"], "ephemeral");
    }

    /// Caching disabled (`CacheRetention::None`) keeps the historical
    /// plain-string `system` shape so callers that explicitly opt out
    /// don't see the array form on the wire.
    #[test]
    fn system_prompt_remains_plain_string_when_caching_disabled() {
        let model = test_model();
        let context = Context {
            system_prompt: Some("You are helpful.".to_string()),
            messages: vec![],
            tools: None,
        };
        let opts = StreamOptions {
            cache_retention: Some(CacheRetention::None),
            ..Default::default()
        };
        let body = build_request_body(&model, &context, 4096, None, &Some(opts)).unwrap();
        assert_eq!(body["system"], "You are helpful.");
    }

    fn tool_def(name: &str) -> Tool {
        Tool::new(name, format!("desc {name}"), serde_json::json!({}))
    }

    /// Anthropic prompt caching keys off the last `cache_control`
    /// breakpoint. Placing it on the final tool definition lets the
    /// tool list (which is stable across turns) be cached independently
    /// from the constantly-changing transcript. Default `cache_retention`
    /// is `Short` so the breakpoint should appear even when the caller
    /// doesn't set the option.
    #[test]
    fn last_tool_carries_cache_control_when_caching_enabled() {
        let model = test_model();
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new_text("hi"))],
            tools: Some(vec![tool_def("a"), tool_def("b"), tool_def("c")]),
        };
        let body = build_request_body(&model, &context, 4096, None, &None).unwrap();
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 3);
        assert!(
            tools[0].get("cache_control").is_none(),
            "first tool must not carry cache_control: {tools:?}"
        );
        assert!(
            tools[1].get("cache_control").is_none(),
            "middle tool must not carry cache_control: {tools:?}"
        );
        let cc = tools[2]
            .get("cache_control")
            .unwrap_or_else(|| panic!("last tool missing cache_control: {tools:?}"));
        assert_eq!(cc["type"], "ephemeral");
        assert!(cc.get("ttl").is_none(), "short retention must not set ttl");
    }

    /// `Long` retention against an Anthropic-supported endpoint promotes
    /// the breakpoint to the 1-hour TTL window.
    #[test]
    fn last_tool_cache_control_long_retention_sets_1h_ttl() {
        let model = test_model();
        let context = Context {
            system_prompt: None,
            messages: vec![],
            tools: Some(vec![tool_def("a")]),
        };
        let opts = StreamOptions {
            cache_retention: Some(CacheRetention::Long),
            ..Default::default()
        };
        let body = build_request_body(&model, &context, 4096, None, &Some(opts)).unwrap();
        let cc = &body["tools"][0]["cache_control"];
        assert_eq!(cc["type"], "ephemeral");
        assert_eq!(cc["ttl"], "1h");
    }

    /// Anthropic places the conversation breakpoint on the LAST user
    /// message so the cached prefix covers everything up to that turn.
    /// Marking an assistant turn would shorten the prefix and waste a
    /// breakpoint budget slot — assistants must stay untouched.
    #[test]
    fn last_user_message_carries_cache_control_when_caching_enabled() {
        let model = test_model();
        let context = Context {
            system_prompt: None,
            messages: vec![
                Message::User(UserMessage::new_text("first")),
                Message::Assistant(AssistantMessage {
                    role: "assistant".to_string(),
                    content: vec![AssistantContentBlock::Text(TextContent::new("ack"))],
                    api: Api::AnthropicMessages,
                    provider: Provider::Anthropic,
                    model: "test".to_string(),
                    usage: Default::default(),
                    stop_reason: StopReason::Stop,
                    error_message: None,
                    timestamp: 0,
                    response_model: None,
                    response_id: None,
                    diagnostics: None,
                }),
                Message::User(UserMessage::new_text("follow-up")),
            ],
            tools: None,
        };
        let body = build_request_body(&model, &context, 4096, None, &None).unwrap();
        let msgs = body["messages"].as_array().unwrap();
        // The non-last user message stays in its historical
        // `Value::String` content shape and obviously carries no
        // breakpoint. Just assert the type to pin the contract.
        assert!(msgs[0]["content"].is_string());
        // Assistant must NOT carry a breakpoint anywhere in its
        // content array.
        let assistant_content = msgs[1]["content"].as_array().unwrap();
        for b in assistant_content {
            assert!(
                b.get("cache_control").is_none(),
                "assistant must not carry cache_control: {b:?}"
            );
        }
        // Last user message's content gets promoted to array form (if
        // it wasn't already) and its trailing block carries the
        // breakpoint.
        let last_user_content = msgs[2]["content"].as_array().unwrap();
        let last_block = last_user_content.last().unwrap();
        assert_eq!(last_block["cache_control"]["type"], "ephemeral");
    }

    /// Disabling caching must skip every conversation-level breakpoint
    /// just as it does for system / tools.
    #[test]
    fn last_user_message_cache_control_omitted_when_caching_disabled() {
        let model = test_model();
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new_text("hi"))],
            tools: None,
        };
        let opts = StreamOptions {
            cache_retention: Some(CacheRetention::None),
            ..Default::default()
        };
        let body = build_request_body(&model, &context, 4096, None, &Some(opts)).unwrap();
        // No breakpoint of any kind should appear in or around the
        // user content. Plain-string shape is fine; assert just that.
        let content = &body["messages"][0]["content"];
        match content {
            Value::String(_) => {}
            Value::Array(blocks) => {
                for b in blocks {
                    assert!(b.get("cache_control").is_none(), "{b:?}");
                }
            }
            other => panic!("unexpected content shape: {other:?}"),
        }
    }

    /// `None` retention is the explicit opt-out; the request must NOT
    /// carry any `cache_control` markers so a caller that knows the
    /// transcript will never repeat avoids paying the cache-write cost.
    #[test]
    fn last_tool_cache_control_omitted_when_retention_none() {
        let model = test_model();
        let context = Context {
            system_prompt: None,
            messages: vec![],
            tools: Some(vec![tool_def("a")]),
        };
        let opts = StreamOptions {
            cache_retention: Some(CacheRetention::None),
            ..Default::default()
        };
        let body = build_request_body(&model, &context, 4096, None, &Some(opts)).unwrap();
        assert!(body["tools"][0].get("cache_control").is_none());
    }

    /// Direct Anthropic supports per-tool `eager_input_streaming`, which
    /// delivers partial tool arguments mid-stream. The default model
    /// (no compat overrides, AnthropicMessages api) opts in to it, so
    /// every tool definition in the request body must carry the flag.
    #[test]
    fn every_tool_carries_eager_input_streaming_by_default() {
        let model = test_model();
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new_text("hi"))],
            tools: Some(vec![tool_def("a"), tool_def("b")]),
        };
        let body = build_request_body(&model, &context, 4096, None, &None).unwrap();
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        for (i, t) in tools.iter().enumerate() {
            assert_eq!(
                t["eager_input_streaming"],
                serde_json::Value::Bool(true),
                "tool {i} missing eager_input_streaming: {t:?}"
            );
        }
    }

    /// Models that opt out of eager streaming via
    /// `AnthropicMessagesCompat.supportsEagerToolInputStreaming = false`
    /// (e.g. legacy proxies) must NOT carry the per-tool flag on the
    /// request body. The TS reference flips to a `fine-grained-tool-
    /// streaming` beta header in that case; verifying the body-level
    /// flag is dropped is the stable contract on the Rust side.
    #[test]
    fn no_eager_input_streaming_when_compat_opts_out() {
        let mut model = test_model();
        model.compat = Some(Compat::AnthropicMessages(AnthropicMessagesCompat {
            supports_eager_tool_input_streaming: Some(false),
            ..Default::default()
        }));
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new_text("hi"))],
            tools: Some(vec![tool_def("a")]),
        };
        let body = build_request_body(&model, &context, 4096, None, &None).unwrap();
        let tools = body["tools"].as_array().unwrap();
        assert!(
            tools[0].get("eager_input_streaming").is_none(),
            "compat opt-out must drop eager_input_streaming: {tools:?}"
        );
    }

    #[test]
    fn test_consecutive_tool_results_batched() {
        let model = test_model();
        let msgs = vec![
            Message::ToolResult(ToolResultMessage::new(
                "call_1",
                "read",
                vec![ToolResultContent::Text(TextContent::new("content1"))],
            )),
            Message::ToolResult(ToolResultMessage::new(
                "call_2",
                "write",
                vec![ToolResultContent::Text(TextContent::new("content2"))],
            )),
        ];
        let result = convert_messages_to_anthropic(&msgs, &model);
        // Both tool results should be in a single user message
        assert_eq!(result.len(), 1);
        let content = result[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
    }

    /// Cross-model thinking blocks must be converted to plain text
    /// without `<thinking>` wrapper tags. Previous behavior wrapped
    /// them, which taught Claude to mimic the tag structure in its own
    /// replies (the model copies historical shapes). Convert to plain
    /// text so the model sees only the reasoning content.
    #[test]
    fn test_thinking_cross_model_converted_to_plain_text_without_tags() {
        let model = test_model();
        let asst = AssistantMessage {
            role: "assistant".to_string(),
            content: vec![AssistantContentBlock::Thinking(ThinkingContent::new(
                "Let me think...",
            ))],
            api: Api::OpenAICompletions, // Different API
            provider: Provider::OpenAI,  // Different provider
            model: "gpt-4".to_string(),
            usage: crate::types::Usage::default(),
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        };
        let result = convert_assistant_content(&asst, &model);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["type"], "text");
        let text = result[0]["text"].as_str().unwrap();
        assert!(
            !text.contains("<thinking>"),
            "cross-model thinking must not be wrapped in <thinking> tags: {text}"
        );
        assert!(
            !text.contains("</thinking>"),
            "cross-model thinking must not be wrapped in </thinking> tags: {text}"
        );
        assert_eq!(text, "Let me think...");
    }

    /// Same-model thinking blocks without a signature (aborted stream,
    /// missing scratch buffer) must also be emitted as plain text
    /// without `<thinking>` wrapper tags — the unsigned shape is
    /// rejected by the API and the wrapper trains the model to mimic
    /// the tags.
    #[test]
    fn test_thinking_same_model_unsigned_converted_to_plain_text() {
        let model = test_model();
        let mut thinking = ThinkingContent::new("partial reasoning");
        thinking.thinking_signature = None;
        let asst = AssistantMessage {
            role: "assistant".to_string(),
            content: vec![AssistantContentBlock::Thinking(thinking)],
            api: Api::AnthropicMessages,
            provider: Provider::Anthropic,
            model: model.id.clone(),
            usage: crate::types::Usage::default(),
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        };
        let result = convert_assistant_content(&asst, &model);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["type"], "text");
        let text = result[0]["text"].as_str().unwrap();
        assert!(
            !text.contains("<thinking>"),
            "unsigned thinking must not be wrapped in <thinking> tags: {text}"
        );
        assert_eq!(text, "partial reasoning");
    }

    fn same_model_assistant(model: &Model, thinking: ThinkingContent) -> AssistantMessage {
        AssistantMessage {
            role: "assistant".to_string(),
            content: vec![AssistantContentBlock::Thinking(thinking)],
            api: Api::AnthropicMessages,
            provider: Provider::Anthropic,
            model: model.id.clone(),
            usage: crate::types::Usage::default(),
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        }
    }

    /// A signed thinking block whose text is empty (e.g. minimal thinking
    /// budget yields a signature without any thinking deltas) must replay
    /// as a `thinking` block that preserves the signature — not as
    /// `redacted_thinking`, whose `data` field expects a different opaque
    /// payload, and not dropped, which would lose the signature.
    #[test]
    fn test_thinking_same_model_signed_empty_replays_as_thinking() {
        let model = test_model();
        let mut thinking = ThinkingContent::new("");
        thinking.thinking_signature = Some("sig-1".to_string());
        let asst = same_model_assistant(&model, thinking);
        let result = convert_assistant_content(&asst, &model);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["type"], "thinking");
        assert_eq!(result[0]["thinking"], "");
        assert_eq!(result[0]["signature"], "sig-1");
    }

    /// Real redacted thinking (marked in-band at parse time) must keep
    /// round-tripping as `redacted_thinking` with the opaque payload in
    /// `data`.
    #[test]
    fn test_thinking_redacted_marker_replays_as_redacted() {
        let model = test_model();
        let mut thinking = ThinkingContent::new("[Reasoning redacted]");
        thinking.thinking_signature = Some("opaque-data".to_string());
        let asst = same_model_assistant(&model, thinking);
        let result = convert_assistant_content(&asst, &model);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["type"], "redacted_thinking");
        assert_eq!(result[0]["data"], "opaque-data");
    }
}
