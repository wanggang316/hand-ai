//! Anthropic Messages API provider.
//!
//! Implements streaming chat completions for the Anthropic Messages API.
//! Supports tool use, thinking/reasoning blocks, and cache control.

use crate::api_registry::AssistantMessageEventStream;
use crate::types::{
    Api, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Context, InputType,
    Message, Model, Provider, SimpleStreamOptions, StopReason, StreamOptions, TextContent,
    ThinkingContent, ThinkingLevel, Tool, ToolCall, ToolResultContent, UserContentBlock,
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

    // Add beta features
    let mut betas = vec!["fine-grained-tool-streaming-2025-05-14"];
    if reasoning.is_some() {
        betas.push("interleaved-thinking-2025-05-14");
    }
    headers.insert(
        "anthropic-beta",
        HeaderValue::from_str(&betas.join(",")).map_err(|e| e.to_string())?,
    );

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

    // System prompt
    if let Some(system_prompt) = &context.system_prompt
        && !system_prompt.is_empty()
    {
        body.insert("system".to_string(), Value::String(system_prompt.clone()));
    }

    // Temperature
    if let Some(opts) = options
        && let Some(temp) = opts.temperature
    {
        body.insert(
            "temperature".to_string(),
            Value::Number(
                serde_json::Number::from_f64(temp as f64)
                    .unwrap_or_else(|| serde_json::Number::from(0)),
            ),
        );
    }

    // Messages
    let messages = convert_messages_to_anthropic(&context.messages, model);
    body.insert("messages".to_string(), Value::Array(messages));

    // Tools
    if let Some(tools) = &context.tools
        && !tools.is_empty()
    {
        let tool_defs: Vec<Value> = tools.iter().map(convert_tool_to_anthropic).collect();
        body.insert("tools".to_string(), Value::Array(tool_defs));
    }

    // Thinking/reasoning configuration
    if let Some(level) = reasoning
        && model.reasoning
    {
        let thinking_config = build_thinking_config(level, model);
        body.insert("thinking".to_string(), thinking_config);
    }

    Ok(Value::Object(body))
}

fn build_thinking_config(level: ThinkingLevel, model: &Model) -> Value {
    // Check if model supports adaptive thinking (Opus 4.6, Sonnet 4.6)
    let supports_adaptive = model.id.contains("opus-4") || model.id.contains("sonnet-4");

    if supports_adaptive {
        let effort = match level {
            ThinkingLevel::Minimal | ThinkingLevel::Low => "low",
            ThinkingLevel::Medium => "medium",
            ThinkingLevel::High => "high",
            ThinkingLevel::Xhigh => "max",
        };

        serde_json::json!({
            "type": "adaptive",
            "effort": effort,
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
                            // Redacted thinking or normal thinking with signature
                            if tc.thinking.contains("[Reasoning redacted]")
                                || tc.thinking.is_empty()
                            {
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
                            // No signature - convert to text to avoid API rejection
                            blocks.push(serde_json::json!({
                                "type": "text",
                                "text": format!("<thinking>{}</thinking>", &tc.thinking),
                            }));
                        }
                    } else if !tc.thinking.is_empty() {
                        // No signature - convert to text
                        blocks.push(serde_json::json!({
                            "type": "text",
                            "text": format!("<thinking>{}</thinking>", &tc.thinking),
                        }));
                    }
                } else if !tc.thinking.is_empty() {
                    // Cross-model: convert to text
                    blocks.push(serde_json::json!({
                        "type": "text",
                        "text": format!("<thinking>{}</thinking>", &tc.thinking),
                    }));
                }
            }
            AssistantContentBlock::ToolCall(tc) => {
                blocks.push(serde_json::json!({
                    "type": "tool_use",
                    "id": normalize_tool_call_id(&tc.id),
                    "name": &tc.name,
                    "input": &tc.arguments,
                }));
            }
        }
    }

    blocks
}

fn convert_tool_result_content(content: &[ToolResultContent]) -> Vec<Value> {
    content
        .iter()
        .map(|block| match block {
            ToolResultContent::Text(tc) => serde_json::json!({
                "type": "text",
                "text": sanitize_surrogates(&tc.text),
            }),
            ToolResultContent::Image(img) => serde_json::json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": &img.mime_type,
                    "data": &img.data,
                }
            }),
        })
        .collect()
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
    };

    let mut content_blocks: HashMap<usize, ContentBlockState> = HashMap::new();
    let mut current_stop_reason = StopReason::Stop;

    // Read SSE stream
    let body = response.text().await.map_err(|e| e.to_string())?;

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
                                // Final parse of accumulated JSON
                                if !args_buf.is_empty() {
                                    tc.arguments = serde_json::from_str(&args_buf)
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
    use crate::types::{AssistantContentBlock, Cost, InputType, ToolResultMessage, UserMessage};

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
        }
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
        let model = Model {
            id: "claude-opus-4-20250514".to_string(),
            ..test_model()
        };
        let config = build_thinking_config(ThinkingLevel::High, &model);
        assert_eq!(config["type"], "adaptive");
        assert_eq!(config["effort"], "high");
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
        assert_eq!(body["system"], "You are helpful.");
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

    #[test]
    fn test_thinking_cross_model_converted_to_text() {
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
        };
        let result = convert_assistant_content(&asst, &model);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["type"], "text");
        assert!(result[0]["text"].as_str().unwrap().contains("<thinking>"));
    }
}
