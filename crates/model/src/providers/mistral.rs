//! Mistral conversations API provider.
//!
//! Implements streaming chat completions for Mistral.
//!
//! Key Mistral specifics:
//!
//! - Uses the Mistral chat completions endpoint at
//!   `<base>/v1/chat/completions` with `stream: true`. The endpoint and
//!   request shape match the OpenAI completions wire format with two extra
//!   fields: `prompt_mode` and `reasoning_effort`.
//! - Tool call IDs must be exactly nine alphanumeric characters. Reuses
//!   `normalize_mistral_tool_id` from `providers::openai_completions`.
//! - Reasoning effort is exposed two ways: `reasoning_effort` for the small/
//!   medium reasoning models, `prompt_mode: "reasoning"` for everyone else
//!   (Magistral-style models).

use crate::api_registry::AssistantMessageEventStream;
use crate::types::{
    Api, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Context, InputType,
    Message, Model, SimpleStreamOptions, StopReason, StreamOptions, TextContent, ThinkingContent,
    ThinkingLevel, Tool, ToolCall, ToolResultContent, Usage, UserContentBlock,
};
use crate::{calculate_cost, env_api_keys};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde_json::Value;
use std::collections::HashMap;

use super::openai_completions::normalize_mistral_tool_id;

// =============================================================================
// Options
// =============================================================================

/// Provider-specific options for the Mistral conversations API.
#[derive(Debug, Clone, Default)]
pub struct MistralOptions {
    /// Base streaming options shared with all providers.
    pub base: StreamOptions,
    /// Optional tool-choice override (`auto` / `none` / `any` / `required` /
    /// a specific function name).
    pub tool_choice: Option<MistralToolChoice>,
    /// Magistral-style models: enable reasoning by setting this to
    /// `Some(MistralPromptMode::Reasoning)`.
    pub prompt_mode: Option<MistralPromptMode>,
    /// Mistral Small / Medium reasoning effort flag.
    pub reasoning_effort: Option<MistralReasoningEffort>,
}

impl MistralOptions {
    pub fn temperature(&self) -> Option<f32> {
        self.base.temperature
    }

    pub fn max_tokens(&self) -> Option<u32> {
        self.base.max_tokens
    }

    pub fn api_key(&self) -> Option<&str> {
        self.base.api_key.as_deref()
    }

    pub fn headers(&self) -> Option<&HashMap<String, String>> {
        self.base.headers.as_ref()
    }
}

/// Tool-choice values accepted by Mistral.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MistralToolChoice {
    Auto,
    None,
    Any,
    Required,
    Function(String),
}

impl MistralToolChoice {
    fn to_json(&self) -> Value {
        match self {
            MistralToolChoice::Auto => Value::String("auto".into()),
            MistralToolChoice::None => Value::String("none".into()),
            MistralToolChoice::Any => Value::String("any".into()),
            MistralToolChoice::Required => Value::String("required".into()),
            MistralToolChoice::Function(name) => serde_json::json!({
                "type": "function",
                "function": { "name": name },
            }),
        }
    }
}

/// Magistral-style prompt mode flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MistralPromptMode {
    Reasoning,
}

impl MistralPromptMode {
    fn as_wire(self) -> &'static str {
        match self {
            MistralPromptMode::Reasoning => "reasoning",
        }
    }
}

/// Reasoning effort levels accepted by Mistral Small / Medium reasoning
/// models.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MistralReasoningEffort {
    None,
    High,
}

impl MistralReasoningEffort {
    fn as_wire(self) -> &'static str {
        match self {
            MistralReasoningEffort::None => "none",
            MistralReasoningEffort::High => "high",
        }
    }
}

// =============================================================================
// Provider
// =============================================================================

/// Provider implementation for the Mistral conversations API.
#[derive(Debug, Clone)]
pub struct MistralProvider {
    client: reqwest::Client,
    /// Optional base-URL override used by tests to point at a mock HTTP
    /// server. When `None`, the request URL is built from `model.base_url`
    /// (or the canonical Mistral endpoint when that field is empty).
    base_url_override: Option<String>,
}

impl Default for MistralProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl MistralProvider {
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

    /// Test seam: override the base URL used to build the chat-completions
    /// endpoint. Routes requests at `<base>/v1/chat/completions` regardless of
    /// the value carried by `Model.base_url`.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url_override = Some(base_url.into());
        self
    }
}

impl crate::api_registry::ApiProvider for MistralProvider {
    fn stream(
        &self,
        model: Model,
        context: Context,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        let mistral_options = MistralOptions {
            base: options.unwrap_or_default(),
            tool_choice: None,
            prompt_mode: None,
            reasoning_effort: None,
        };
        Box::pin(stream_mistral(
            self.client.clone(),
            self.base_url_override.clone(),
            model,
            context,
            mistral_options,
        ))
    }

    fn stream_simple(
        &self,
        model: Model,
        context: Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        let mistral_options = build_mistral_options(&model, options.as_ref());
        Box::pin(stream_mistral(
            self.client.clone(),
            self.base_url_override.clone(),
            model,
            context,
            mistral_options,
        ))
    }
}

fn build_mistral_options(model: &Model, options: Option<&SimpleStreamOptions>) -> MistralOptions {
    let (base, reasoning) = match options {
        Some(opts) => {
            let api_key = env_api_keys::get_env_api_key(&model.provider);
            let base = opts.build_base_options(model, api_key);
            (base, opts.clamp_reasoning())
        }
        None => (StreamOptions::default(), None),
    };

    let should_use_reasoning = model.reasoning && reasoning.is_some();

    let mut prompt_mode = None;
    let mut reasoning_effort = None;
    if should_use_reasoning {
        if uses_reasoning_effort(model) {
            reasoning_effort = Some(map_reasoning_effort(model, reasoning.unwrap()));
        } else if uses_prompt_mode_reasoning(model) {
            prompt_mode = Some(MistralPromptMode::Reasoning);
        }
    }

    MistralOptions {
        base,
        tool_choice: None,
        prompt_mode,
        reasoning_effort,
    }
}

fn uses_reasoning_effort(model: &Model) -> bool {
    matches!(
        model.id.as_str(),
        "mistral-small-2603" | "mistral-small-latest" | "mistral-medium-3.5"
    )
}

fn uses_prompt_mode_reasoning(model: &Model) -> bool {
    model.reasoning && !uses_reasoning_effort(model)
}

fn map_reasoning_effort(model: &Model, level: ThinkingLevel) -> MistralReasoningEffort {
    let key = match level {
        ThinkingLevel::Minimal => "minimal",
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High | ThinkingLevel::Xhigh | ThinkingLevel::Max => "high",
    };

    if let Some(map) = model.thinking_level_map.as_ref()
        && let Some(Some(value)) = map.get(key)
    {
        return match value.as_str() {
            "none" => MistralReasoningEffort::None,
            _ => MistralReasoningEffort::High,
        };
    }
    MistralReasoningEffort::High
}

// =============================================================================
// Streaming
// =============================================================================

fn stream_mistral(
    client: reqwest::Client,
    base_url_override: Option<String>,
    model: Model,
    context: Context,
    options: MistralOptions,
) -> impl futures::Stream<Item = AssistantMessageEvent> + Send + 'static {
    async_stream::stream! {
        // Emit `Start` unconditionally so consumers always see the same
        // `Start -> ... -> (Done | Error)` shape, including on early failure
        // paths (auth, network) where SSE never opens. `parse_sse_stream`
        // intentionally does NOT emit its own `Start` to avoid duplicates.
        let initial = AssistantMessage {
            role: "assistant".to_string(),
            content: vec![],
            api: Api::MistralConversations,
            provider: model.provider,
            model: model.id.clone(),
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: current_timestamp_ms(),
            response_model: None,
            response_id: None,
            diagnostics: None,
        };
        yield AssistantMessageEvent::Start { partial: initial.clone() };

        let result =
            stream_mistral_inner(client, base_url_override, model.clone(), context, options).await;
        match result {
            Ok(events) => {
                for event in events {
                    yield event;
                }
            }
            Err(e) => {
                let mut error_msg = initial;
                error_msg.stop_reason = StopReason::Error;
                error_msg.error_message = Some(e);
                yield AssistantMessageEvent::Error {
                    reason: StopReason::Error,
                    error: error_msg,
                };
            }
        }
    }
}

async fn stream_mistral_inner(
    client: reqwest::Client,
    base_url_override: Option<String>,
    model: Model,
    context: Context,
    options: MistralOptions,
) -> Result<Vec<AssistantMessageEvent>, String> {
    let api_key = options
        .api_key()
        .map(|s| s.to_string())
        .or_else(|| env_api_keys::get_env_api_key(&model.provider))
        .ok_or_else(|| format!("No API key for provider: {}", model.provider.as_str()))?;

    let base_url = if let Some(b) = base_url_override.as_deref() {
        b.trim_end_matches('/').to_string()
    } else if model.base_url.is_empty() {
        "https://api.mistral.ai".to_string()
    } else {
        model.base_url.trim_end_matches('/').to_string()
    };
    let url = format!("{}/v1/chat/completions", base_url);

    // Normalize cross-provider tool-call IDs to Mistral's 9-char alphanumeric
    // form before serializing the request. Without this step, OpenAI-style
    // assistant context replayed against a Mistral target would carry IDs
    // Mistral's API rejects. The normalizer closure is dropped before any
    // await so the surrounding future stays `Send`.
    let normalized_context = {
        let normalizer: crate::transform::NormalizeToolCallIdFn =
            Box::new(|id, _model, _src| normalize_mistral_tool_id(id));
        let normalized_messages =
            crate::transform::transform_messages(&context.messages, &model, Some(&normalizer));
        Context {
            system_prompt: context.system_prompt.clone(),
            messages: normalized_messages,
            tools: context.tools.clone(),
        }
    };

    // Build request body
    let body = build_request_body(&model, &normalized_context, &options)?;

    // Headers
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", api_key)).map_err(|e| e.to_string())?,
    );

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
    if let Some(custom) = options.headers() {
        for (key, value) in custom {
            if let (Ok(name), Ok(val)) = (
                reqwest::header::HeaderName::from_bytes(key.as_bytes()),
                HeaderValue::from_str(value),
            ) {
                headers.insert(name, val);
            }
        }
    }

    // Mistral infrastructure honors `x-affinity` for KV-cache reuse. Caller-
    // supplied headers always win.
    if let Some(session_id) = options.base.session_id.as_deref()
        && !headers.contains_key("x-affinity")
        && let Ok(val) = HeaderValue::from_str(session_id)
    {
        headers.insert("x-affinity", val);
    }

    let response = client
        .post(&url)
        .headers(headers)
        .body(body.to_string())
        .send()
        .await
        .map_err(|e| format!("Request failed: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response
            .text()
            .await
            .unwrap_or_else(|_| "Failed to read error body".to_string());
        return Err(format!("Mistral API error ({}): {}", status, text));
    }

    parse_sse_stream(response, &model).await
}

// =============================================================================
// Request Building
// =============================================================================

fn build_request_body(
    model: &Model,
    context: &Context,
    options: &MistralOptions,
) -> Result<Value, String> {
    let mut body = serde_json::Map::new();

    body.insert("model".to_string(), Value::String(model.id.clone()));
    body.insert("stream".to_string(), Value::Bool(true));

    let supports_images = model.input.contains(&InputType::Image);
    let messages = convert_messages(&context.messages, model, supports_images);
    let mut messages_with_system = Vec::with_capacity(messages.len() + 1);
    if let Some(system_prompt) = &context.system_prompt
        && !system_prompt.is_empty()
    {
        messages_with_system.push(serde_json::json!({
            "role": "system",
            "content": sanitize_surrogates(system_prompt),
        }));
    }
    messages_with_system.extend(messages);
    body.insert("messages".to_string(), Value::Array(messages_with_system));

    if let Some(tools) = &context.tools
        && !tools.is_empty()
    {
        let tool_defs: Vec<Value> = tools.iter().map(convert_tool).collect();
        body.insert("tools".to_string(), Value::Array(tool_defs));
    }

    if let Some(temp) = options.temperature()
        && let Some(n) = serde_json::Number::from_f64(temp as f64)
    {
        body.insert("temperature".to_string(), Value::Number(n));
    }

    if let Some(max_tokens) = options.max_tokens() {
        body.insert("max_tokens".to_string(), Value::Number(max_tokens.into()));
    }

    if let Some(choice) = options.tool_choice.as_ref() {
        body.insert("tool_choice".to_string(), choice.to_json());
    }

    if let Some(mode) = options.prompt_mode {
        body.insert(
            "prompt_mode".to_string(),
            Value::String(mode.as_wire().to_string()),
        );
    }
    if let Some(effort) = options.reasoning_effort {
        body.insert(
            "reasoning_effort".to_string(),
            Value::String(effort.as_wire().to_string()),
        );
    }

    Ok(Value::Object(body))
}

fn convert_tool(tool: &Tool) -> Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.parameters,
            "strict": false,
        },
    })
}

// =============================================================================
// Message Conversion
// =============================================================================

fn convert_messages(messages: &[Message], model: &Model, supports_images: bool) -> Vec<Value> {
    let mut result: Vec<Value> = Vec::new();

    for msg in messages {
        match msg {
            Message::User(user_msg) => {
                if let Some(value) = convert_user_message(user_msg, supports_images) {
                    result.push(value);
                }
            }
            Message::Assistant(assistant) => {
                if let Some(value) = convert_assistant_message(assistant, model) {
                    result.push(value);
                }
            }
            Message::ToolResult(tool_result) => {
                result.push(convert_tool_result(tool_result, supports_images));
            }
        }
    }

    result
}

fn convert_user_message(
    user_msg: &crate::types::UserMessage,
    supports_images: bool,
) -> Option<Value> {
    match &user_msg.content {
        crate::types::UserContent::Text(text) => Some(serde_json::json!({
            "role": "user",
            "content": sanitize_surrogates(text),
        })),
        crate::types::UserContent::Blocks(blocks) => {
            let had_images = blocks
                .iter()
                .any(|b| matches!(b, UserContentBlock::Image(_)));
            let parts: Vec<Value> = blocks
                .iter()
                .filter_map(|block| match block {
                    UserContentBlock::Text(t) => Some(serde_json::json!({
                        "type": "text",
                        "text": sanitize_surrogates(&t.text),
                    })),
                    UserContentBlock::Image(img) if supports_images => Some(serde_json::json!({
                        "type": "image_url",
                        "image_url": format!("data:{};base64,{}", img.mime_type, img.data),
                    })),
                    UserContentBlock::Image(_) => None,
                })
                .collect();

            if !parts.is_empty() {
                return Some(serde_json::json!({
                    "role": "user",
                    "content": parts,
                }));
            }
            if had_images && !supports_images {
                return Some(serde_json::json!({
                    "role": "user",
                    "content": "(image omitted: model does not support images)",
                }));
            }
            None
        }
    }
}

fn convert_assistant_message(assistant: &AssistantMessage, _model: &Model) -> Option<Value> {
    let mut parts: Vec<Value> = Vec::new();
    let mut tool_calls: Vec<Value> = Vec::new();

    for block in &assistant.content {
        match block {
            AssistantContentBlock::Text(t) => {
                if !t.text.trim().is_empty() {
                    parts.push(serde_json::json!({
                        "type": "text",
                        "text": sanitize_surrogates(&t.text),
                    }));
                }
            }
            AssistantContentBlock::Thinking(t) => {
                if !t.thinking.trim().is_empty() {
                    parts.push(serde_json::json!({
                        "type": "thinking",
                        "thinking": [{ "type": "text", "text": sanitize_surrogates(&t.thinking) }],
                    }));
                }
            }
            AssistantContentBlock::ToolCall(tc) => {
                tool_calls.push(serde_json::json!({
                    "id": tc.id,
                    "type": "function",
                    "function": {
                        "name": tc.name,
                        "arguments": tc.arguments.to_string(),
                    },
                }));
            }
        }
    }

    if parts.is_empty() && tool_calls.is_empty() {
        return None;
    }

    let mut msg = serde_json::Map::new();
    msg.insert("role".to_string(), Value::String("assistant".to_string()));
    if !parts.is_empty() {
        msg.insert("content".to_string(), Value::Array(parts));
    }
    if !tool_calls.is_empty() {
        msg.insert("tool_calls".to_string(), Value::Array(tool_calls));
    }
    Some(Value::Object(msg))
}

fn convert_tool_result(
    tool_result: &crate::types::ToolResultMessage,
    supports_images: bool,
) -> Value {
    let text_result = tool_result
        .content
        .iter()
        .filter_map(|c| match c {
            ToolResultContent::Text(t) => Some(sanitize_surrogates(&t.text)),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let has_images = tool_result
        .content
        .iter()
        .any(|c| matches!(c, ToolResultContent::Image(_)));
    let tool_text = build_tool_result_text(
        &text_result,
        has_images,
        supports_images,
        tool_result.is_error,
    );

    let mut content_parts: Vec<Value> = vec![serde_json::json!({
        "type": "text",
        "text": tool_text,
    })];
    if supports_images {
        for part in &tool_result.content {
            if let ToolResultContent::Image(img) = part {
                content_parts.push(serde_json::json!({
                    "type": "image_url",
                    "image_url": format!("data:{};base64,{}", img.mime_type, img.data),
                }));
            }
        }
    }

    serde_json::json!({
        "role": "tool",
        "tool_call_id": tool_result.tool_call_id,
        "name": tool_result.tool_name,
        "content": content_parts,
    })
}

fn build_tool_result_text(
    text: &str,
    has_images: bool,
    supports_images: bool,
    is_error: bool,
) -> String {
    let trimmed = text.trim();
    let error_prefix = if is_error { "[tool error] " } else { "" };

    if !trimmed.is_empty() {
        let suffix = if has_images && !supports_images {
            "\n[tool image omitted: model does not support images]"
        } else {
            ""
        };
        return format!("{error_prefix}{trimmed}{suffix}");
    }

    if has_images {
        if supports_images {
            return if is_error {
                "[tool error] (see attached image)".to_string()
            } else {
                "(see attached image)".to_string()
            };
        }
        return if is_error {
            "[tool error] (image omitted: model does not support images)".to_string()
        } else {
            "(image omitted: model does not support images)".to_string()
        };
    }

    if is_error {
        "[tool error] (no tool output)".to_string()
    } else {
        "(no tool output)".to_string()
    }
}

// =============================================================================
// SSE parsing
// =============================================================================

async fn parse_sse_stream(
    response: reqwest::Response,
    model: &Model,
) -> Result<Vec<AssistantMessageEvent>, String> {
    let mut events = Vec::new();

    let mut output = AssistantMessage {
        role: "assistant".to_string(),
        content: vec![],
        api: Api::MistralConversations,
        provider: model.provider,
        model: model.id.clone(),
        usage: Usage::default(),
        stop_reason: StopReason::Stop,
        error_message: None,
        timestamp: current_timestamp_ms(),
        response_model: None,
        response_id: None,
        diagnostics: None,
    };

    // Note: `Start` is emitted by the outer `stream_mistral` wrapper so
    // every code path (including auth/network early failure) yields a
    // `Start` before any terminal event. Do not emit it here.

    let body = response
        .text()
        .await
        .map_err(|e| format!("Failed to read response: {}", e))?;

    // Tracks the current open block so we can emit text/thinking start/end
    // events; tool blocks are tracked separately because they may interleave
    // by index.
    let mut current_block: Option<&'static str> = None;
    // Map of (id_or_index_key) -> output content index for tool blocks.
    let mut tool_blocks: HashMap<String, usize> = HashMap::new();
    // Parallel partial-args buffers keyed the same way.
    let mut tool_args_buffers: HashMap<String, String> = HashMap::new();

    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        if data == "[DONE]" {
            break;
        }

        let chunk: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if output.response_id.is_none()
            && let Some(id) = chunk.get("id").and_then(|v| v.as_str())
            && !id.is_empty()
        {
            output.response_id = Some(id.to_string());
        }

        if let Some(usage) = chunk.get("usage") {
            output.usage.input = usage
                .get("prompt_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            output.usage.output = usage
                .get("completion_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            output.usage.cache_read = 0;
            output.usage.cache_write = 0;
            output.usage.total_tokens = usage
                .get("total_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(output.usage.input + output.usage.output);
            calculate_cost(model, &mut output.usage);
        }

        let Some(choice) = chunk
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|c| c.first())
        else {
            continue;
        };

        if let Some(reason) = choice.get("finish_reason").and_then(|v| v.as_str()) {
            output.stop_reason = map_stop_reason(reason);
        }

        let delta = choice.get("delta").cloned().unwrap_or(Value::Null);

        // Process content delta.
        if let Some(content) = delta.get("content")
            && !content.is_null()
        {
            let items: Vec<Value> = if content.is_string() {
                vec![content.clone()]
            } else if let Some(arr) = content.as_array() {
                arr.clone()
            } else {
                vec![]
            };

            for item in items {
                if let Some(text) = item.as_str() {
                    push_text_delta(text, &mut current_block, &mut output, &mut events);
                    continue;
                }
                let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match item_type {
                    "text" => {
                        let text = item.get("text").and_then(|v| v.as_str()).unwrap_or("");
                        push_text_delta(text, &mut current_block, &mut output, &mut events);
                    }
                    "thinking" => {
                        let thinking_arr = item.get("thinking").and_then(|v| v.as_array()).cloned();
                        let mut delta_text = String::new();
                        if let Some(parts) = thinking_arr {
                            for part in parts {
                                if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                                    delta_text.push_str(t);
                                }
                            }
                        }
                        if !delta_text.is_empty() {
                            push_thinking_delta(
                                &delta_text,
                                &mut current_block,
                                &mut output,
                                &mut events,
                            );
                        }
                    }
                    _ => {}
                }
            }
        }

        // Process tool-call deltas.
        if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
            for (i, tc) in tool_calls.iter().enumerate() {
                close_current_block(&mut current_block, &mut output, &mut events);

                let provided_id = tc
                    .get("id")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty() && *s != "null")
                    .map(String::from);
                let index = tc
                    .get("index")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize)
                    .unwrap_or(i);

                let call_id = provided_id
                    .clone()
                    .unwrap_or_else(|| normalize_mistral_tool_id(&format!("toolcall:{index}")));
                let key = format!("{call_id}:{index}");

                let function = tc.get("function").cloned().unwrap_or(Value::Null);
                let name_delta = function
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let args_delta = match function.get("arguments") {
                    Some(Value::String(s)) => s.clone(),
                    Some(other) if !other.is_null() => other.to_string(),
                    _ => String::new(),
                };

                let block_idx_opt = tool_blocks.get(&key).copied();
                let block_idx = match block_idx_opt {
                    Some(idx) => idx,
                    None => {
                        let tool_call = ToolCall::new(&call_id, &name_delta, serde_json::json!({}));
                        output
                            .content
                            .push(AssistantContentBlock::ToolCall(tool_call));
                        let idx = output.content.len() - 1;
                        tool_blocks.insert(key.clone(), idx);
                        tool_args_buffers.insert(key.clone(), String::new());
                        events.push(AssistantMessageEvent::ToolCallStart {
                            content_index: idx as u32,
                            partial: output.clone(),
                        });
                        idx
                    }
                };

                if !name_delta.is_empty()
                    && let Some(AssistantContentBlock::ToolCall(tc_ref)) =
                        output.content.get_mut(block_idx)
                    && tc_ref.name.is_empty()
                {
                    tc_ref.name = name_delta;
                }

                if !args_delta.is_empty() {
                    let buf = tool_args_buffers.entry(key.clone()).or_default();
                    buf.push_str(&args_delta);
                    let parsed = parse_streaming_json(buf);
                    if let Some(AssistantContentBlock::ToolCall(tc_ref)) =
                        output.content.get_mut(block_idx)
                    {
                        tc_ref.arguments = parsed;
                    }
                    events.push(AssistantMessageEvent::ToolCallDelta {
                        content_index: block_idx as u32,
                        delta: args_delta,
                        partial: output.clone(),
                    });
                }
            }
        }
    }

    close_current_block(&mut current_block, &mut output, &mut events);

    // Finalize tool blocks.
    let mut keys: Vec<(String, usize)> = tool_blocks.iter().map(|(k, v)| (k.clone(), *v)).collect();
    keys.sort_by_key(|(_, idx)| *idx);
    for (key, idx) in keys {
        let buf = tool_args_buffers.remove(&key).unwrap_or_default();
        let parsed = parse_streaming_json(&buf);
        if let Some(AssistantContentBlock::ToolCall(tc_ref)) = output.content.get_mut(idx) {
            tc_ref.arguments = parsed;
            let tool_call_clone = tc_ref.clone();
            events.push(AssistantMessageEvent::ToolCallEnd {
                content_index: idx as u32,
                tool_call: tool_call_clone,
                partial: output.clone(),
            });
        }
    }

    events.push(AssistantMessageEvent::Done {
        reason: output.stop_reason,
        message: output,
    });

    Ok(events)
}

fn push_text_delta(
    text: &str,
    current_block: &mut Option<&'static str>,
    output: &mut AssistantMessage,
    events: &mut Vec<AssistantMessageEvent>,
) {
    if text.is_empty() {
        return;
    }
    let text_delta = sanitize_surrogates(text);
    if *current_block != Some("text") {
        close_current_block(current_block, output, events);
        output
            .content
            .push(AssistantContentBlock::Text(TextContent::new("")));
        let idx = (output.content.len() - 1) as u32;
        events.push(AssistantMessageEvent::TextStart {
            content_index: idx,
            partial: output.clone(),
        });
        *current_block = Some("text");
    }
    if let Some(AssistantContentBlock::Text(t)) = output.content.last_mut() {
        t.text.push_str(&text_delta);
    }
    let idx = (output.content.len() - 1) as u32;
    events.push(AssistantMessageEvent::TextDelta {
        content_index: idx,
        delta: text_delta,
        partial: output.clone(),
    });
}

fn push_thinking_delta(
    text: &str,
    current_block: &mut Option<&'static str>,
    output: &mut AssistantMessage,
    events: &mut Vec<AssistantMessageEvent>,
) {
    if text.is_empty() {
        return;
    }
    let thinking_delta = sanitize_surrogates(text);
    if *current_block != Some("thinking") {
        close_current_block(current_block, output, events);
        output
            .content
            .push(AssistantContentBlock::Thinking(ThinkingContent::new("")));
        let idx = (output.content.len() - 1) as u32;
        events.push(AssistantMessageEvent::ThinkingStart {
            content_index: idx,
            partial: output.clone(),
        });
        *current_block = Some("thinking");
    }
    if let Some(AssistantContentBlock::Thinking(t)) = output.content.last_mut() {
        t.thinking.push_str(&thinking_delta);
    }
    let idx = (output.content.len() - 1) as u32;
    events.push(AssistantMessageEvent::ThinkingDelta {
        content_index: idx,
        delta: thinking_delta,
        partial: output.clone(),
    });
}

fn close_current_block(
    current_block: &mut Option<&'static str>,
    output: &mut AssistantMessage,
    events: &mut Vec<AssistantMessageEvent>,
) {
    let kind = match current_block.take() {
        Some(k) => k,
        None => return,
    };
    let idx = (output.content.len().saturating_sub(1)) as u32;
    match kind {
        "text" => {
            let content = match output.content.last() {
                Some(AssistantContentBlock::Text(t)) => t.text.clone(),
                _ => String::new(),
            };
            events.push(AssistantMessageEvent::TextEnd {
                content_index: idx,
                content,
                partial: output.clone(),
            });
        }
        "thinking" => {
            let content = match output.content.last() {
                Some(AssistantContentBlock::Thinking(t)) => t.thinking.clone(),
                _ => String::new(),
            };
            events.push(AssistantMessageEvent::ThinkingEnd {
                content_index: idx,
                content,
                partial: output.clone(),
            });
        }
        _ => {}
    }
}

fn map_stop_reason(reason: &str) -> StopReason {
    match reason {
        "stop" => StopReason::Stop,
        "length" | "model_length" => StopReason::Length,
        "tool_calls" => StopReason::ToolUse,
        "error" => StopReason::Error,
        _ => StopReason::Stop,
    }
}

fn parse_streaming_json(input: &str) -> Value {
    if input.is_empty() {
        return serde_json::json!({});
    }
    if let Ok(v) = serde_json::from_str::<Value>(input) {
        return v;
    }
    // Retry with the shared repair pass — escapes raw control bytes
    // and doubles invalid backslash escapes inside string literals so
    // a malformed tool-call payload doesn't collapse to `{}`.
    if let Some(v) = crate::transform::parse_json_with_repair(input) {
        return v;
    }
    if input.trim_start().starts_with('{') {
        serde_json::json!({})
    } else {
        Value::String(input.to_string())
    }
}

fn sanitize_surrogates(text: &str) -> String {
    text.chars()
        .filter(|&c| !(0xD800..=0xDFFF).contains(&(c as u32)))
        .collect()
}

fn current_timestamp_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Cost, Provider, UserMessage};

    fn test_model() -> Model {
        Model {
            id: "magistral-medium".into(),
            name: "Magistral Medium".into(),
            api: Api::MistralConversations,
            provider: Provider::Mistral,
            base_url: "https://api.mistral.ai".into(),
            reasoning: true,
            input: vec![InputType::Text],
            cost: Cost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_window: 32_000,
            max_tokens: 8_192,
            headers: None,
            compat: None,
            thinking_level_map: None,
        }
    }

    #[test]
    fn build_request_body_basic() {
        let model = test_model();
        let context = Context {
            system_prompt: Some("You are helpful.".into()),
            messages: vec![Message::User(UserMessage::new_text("Hello"))],
            tools: None,
        };
        let options = MistralOptions::default();
        let body = build_request_body(&model, &context, &options).unwrap();

        assert_eq!(body["model"], "magistral-medium");
        assert_eq!(body["stream"], true);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "You are helpful.");
        assert_eq!(messages[1]["role"], "user");
        assert!(body.get("prompt_mode").is_none());
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn build_request_body_includes_prompt_mode_reasoning() {
        let model = test_model();
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new_text("Hi"))],
            tools: None,
        };
        let options = MistralOptions {
            prompt_mode: Some(MistralPromptMode::Reasoning),
            ..Default::default()
        };
        let body = build_request_body(&model, &context, &options).unwrap();
        assert_eq!(body["prompt_mode"], "reasoning");
    }

    #[test]
    fn build_request_body_includes_reasoning_effort_high() {
        let model = test_model();
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new_text("Hi"))],
            tools: None,
        };
        let options = MistralOptions {
            reasoning_effort: Some(MistralReasoningEffort::High),
            ..Default::default()
        };
        let body = build_request_body(&model, &context, &options).unwrap();
        assert_eq!(body["reasoning_effort"], "high");
    }

    #[test]
    fn convert_tool_uses_function_envelope() {
        let tool = Tool::new(
            "calculator",
            "A calculator",
            serde_json::json!({"type": "object"}),
        );
        let value = convert_tool(&tool);
        assert_eq!(value["type"], "function");
        assert_eq!(value["function"]["name"], "calculator");
        assert_eq!(value["function"]["strict"], false);
    }

    #[test]
    fn map_stop_reason_variants() {
        assert_eq!(map_stop_reason("stop"), StopReason::Stop);
        assert_eq!(map_stop_reason("length"), StopReason::Length);
        assert_eq!(map_stop_reason("model_length"), StopReason::Length);
        assert_eq!(map_stop_reason("tool_calls"), StopReason::ToolUse);
        assert_eq!(map_stop_reason("error"), StopReason::Error);
        assert_eq!(map_stop_reason("unknown"), StopReason::Stop);
    }

    #[test]
    fn uses_reasoning_effort_only_for_listed_models() {
        let mut model = test_model();
        model.id = "mistral-small-latest".into();
        assert!(uses_reasoning_effort(&model));
        model.id = "mistral-medium-3.5".into();
        assert!(uses_reasoning_effort(&model));
        model.id = "magistral-medium".into();
        assert!(!uses_reasoning_effort(&model));
    }

    #[test]
    fn uses_prompt_mode_reasoning_when_not_effort_model() {
        let mut model = test_model();
        assert!(uses_prompt_mode_reasoning(&model));
        model.id = "mistral-small-latest".into();
        assert!(!uses_prompt_mode_reasoning(&model));
        model.reasoning = false;
        assert!(!uses_prompt_mode_reasoning(&model));
    }

    #[test]
    fn parse_streaming_json_handles_partial_object() {
        let result = parse_streaming_json("{\"k\":");
        assert_eq!(result, serde_json::json!({}));
    }

    #[test]
    fn parse_streaming_json_handles_complete_object() {
        let result = parse_streaming_json("{\"k\": 1}");
        assert_eq!(result, serde_json::json!({"k": 1}));
    }

    #[test]
    fn build_tool_result_text_no_output() {
        assert_eq!(
            build_tool_result_text("", false, false, false),
            "(no tool output)"
        );
        assert_eq!(
            build_tool_result_text("", false, false, true),
            "[tool error] (no tool output)"
        );
    }
}
