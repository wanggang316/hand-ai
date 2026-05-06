//! Google Generative AI provider.
//!
//! Implements streaming chat completions for the Google Generative AI (Gemini) REST API.
//! Supports thinking/reasoning, tool use, and thought signatures.

use crate::api_registry::AssistantMessageEventStream;
use crate::types::{
    Api, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Context, InputType,
    Message, Model, Provider, SimpleStreamOptions, StopReason, StreamOptions, TextContent,
    ThinkingContent, ThinkingLevel, Tool, ToolCall, ToolResultContent, Usage, UserContentBlock,
};
use crate::{calculate_cost, env_api_keys};
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};

// Counter for generating unique tool call IDs.
static TOOL_CALL_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Thinking level strings for Gemini 3 models.
#[derive(Debug, Clone, Copy)]
pub enum GoogleThinkingLevel {
    Minimal,
    Low,
    Medium,
    High,
}

impl GoogleThinkingLevel {
    fn as_str(self) -> &'static str {
        match self {
            GoogleThinkingLevel::Minimal => "MINIMAL",
            GoogleThinkingLevel::Low => "LOW",
            GoogleThinkingLevel::Medium => "MEDIUM",
            GoogleThinkingLevel::High => "HIGH",
        }
    }
}

/// Google-specific stream options.
#[derive(Debug, Clone, Default)]
pub struct GoogleOptions {
    pub base: StreamOptions,
    pub tool_choice: Option<String>,
    pub thinking_enabled: bool,
    pub thinking_budget_tokens: Option<i32>,
    pub thinking_level: Option<GoogleThinkingLevel>,
}

// =============================================================================
// Provider
// =============================================================================

/// Provider implementation for Google Generative AI.
#[derive(Debug, Clone, Copy, Default)]
pub struct GoogleGenerativeAiProvider;

impl GoogleGenerativeAiProvider {
    pub fn new() -> Self {
        Self
    }
}

impl crate::api_registry::ApiProvider for GoogleGenerativeAiProvider {
    fn stream(
        &self,
        model: Model,
        context: Context,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        let google_options = GoogleOptions {
            base: options.unwrap_or_default(),
            thinking_enabled: false,
            ..Default::default()
        };
        Box::pin(stream_google(model, context, google_options))
    }

    fn stream_simple(
        &self,
        model: Model,
        context: Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        let google_options = build_google_options(&model, options.as_ref());
        Box::pin(stream_google(model, context, google_options))
    }
}

fn build_google_options(model: &Model, options: Option<&SimpleStreamOptions>) -> GoogleOptions {
    let (base, reasoning) = match options {
        Some(opts) => {
            let api_key = env_api_keys::get_env_api_key(&model.provider);
            let base = opts.build_base_options(model, api_key);
            (base, opts.clamp_reasoning())
        }
        None => (StreamOptions::default(), None),
    };

    let mut google_opts = GoogleOptions {
        base,
        ..Default::default()
    };

    match reasoning {
        None => {
            google_opts.thinking_enabled = false;
        }
        Some(effort) => {
            google_opts.thinking_enabled = true;

            if is_gemini3_pro_model(&model.id) || is_gemini3_flash_model(&model.id) {
                google_opts.thinking_level = Some(get_gemini3_thinking_level(effort, &model.id));
            } else {
                google_opts.thinking_budget_tokens = Some(get_google_budget(
                    &model.id,
                    effort,
                    options.and_then(|o| o.thinking_budgets.as_ref()),
                ));
            }
        }
    }

    google_opts
}

// =============================================================================
// Streaming
// =============================================================================

fn stream_google(
    model: Model,
    context: Context,
    options: GoogleOptions,
) -> impl futures::Stream<Item = AssistantMessageEvent> + Send + 'static {
    async_stream::stream! {
        let result = stream_google_inner(model, context, options).await;
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
                    api: Api::GoogleGenerativeAi,
                    provider: Provider::Google,
                    model: String::new(),
                    usage: Usage::default(),
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

async fn stream_google_inner(
    model: Model,
    context: Context,
    options: GoogleOptions,
) -> Result<Vec<AssistantMessageEvent>, String> {
    let api_key = options
        .base
        .api_key
        .clone()
        .or_else(|| env_api_keys::get_env_api_key(&model.provider))
        .ok_or_else(|| format!("No API key found for provider: {}", model.provider.as_str()))?;

    let base_url = if model.base_url.is_empty() {
        "https://generativelanguage.googleapis.com".to_string()
    } else {
        model.base_url.trim_end_matches('/').to_string()
    };

    let url = format!(
        "{}/v1beta/models/{}:streamGenerateContent?alt=sse&key={}",
        base_url, model.id, api_key
    );

    // Build request body
    let body = build_request_body(&model, &context, &options)?;

    // Build headers
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

    // Add model-level headers
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

    // Add option-level headers
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
        return Err(format!("Google API error ({}): {}", status, body));
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
    options: &GoogleOptions,
) -> Result<Value, String> {
    let mut body = serde_json::Map::new();

    // Convert messages to Gemini contents format
    let contents = convert_messages(&context.messages, model);
    body.insert("contents".to_string(), Value::Array(contents));

    // Generation config
    let mut generation_config = serde_json::Map::new();
    if let Some(temp) = options.base.temperature
        && let Some(n) = serde_json::Number::from_f64(temp as f64)
    {
        generation_config.insert("temperature".to_string(), Value::Number(n));
    }
    if let Some(max_tokens) = options.base.max_tokens {
        generation_config.insert(
            "maxOutputTokens".to_string(),
            Value::Number(max_tokens.into()),
        );
    }
    if !generation_config.is_empty() {
        body.insert(
            "generationConfig".to_string(),
            Value::Object(generation_config),
        );
    }

    // System instruction
    if let Some(system_prompt) = &context.system_prompt
        && !system_prompt.is_empty()
    {
        body.insert(
            "systemInstruction".to_string(),
            serde_json::json!({
                "parts": [{"text": system_prompt}]
            }),
        );
    }

    // Tools
    if let Some(tools) = &context.tools
        && !tools.is_empty()
    {
        let tool_defs = convert_tools(tools);
        body.insert("tools".to_string(), Value::Array(vec![tool_defs]));
    }

    // Tool choice
    if let Some(tools) = &context.tools
        && !tools.is_empty()
        && let Some(choice) = &options.tool_choice
    {
        let mode = match choice.as_str() {
            "auto" => "AUTO",
            "none" => "NONE",
            "any" => "ANY",
            _ => "AUTO",
        };
        body.insert(
            "toolConfig".to_string(),
            serde_json::json!({
                "functionCallingConfig": {"mode": mode}
            }),
        );
    }

    // Thinking config
    if options.thinking_enabled && model.reasoning {
        let mut thinking_config = serde_json::Map::new();
        thinking_config.insert("includeThoughts".to_string(), Value::Bool(true));

        if let Some(level) = options.thinking_level {
            thinking_config.insert(
                "thinkingLevel".to_string(),
                Value::String(level.as_str().to_string()),
            );
        } else if let Some(budget) = options.thinking_budget_tokens {
            thinking_config.insert("thinkingBudget".to_string(), Value::Number(budget.into()));
        }

        // Insert into generationConfig
        let gen_config = body
            .entry("generationConfig")
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        if let Value::Object(gc) = gen_config {
            gc.insert("thinkingConfig".to_string(), Value::Object(thinking_config));
        }
    } else if model.reasoning && !options.thinking_enabled {
        // Disable thinking for reasoning models
        let disabled_config = get_disabled_thinking_config(&model.id);
        let gen_config = body
            .entry("generationConfig")
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        if let Value::Object(gc) = gen_config {
            gc.insert("thinkingConfig".to_string(), disabled_config);
        }
    }

    Ok(Value::Object(body))
}

fn get_disabled_thinking_config(model_id: &str) -> Value {
    if is_gemini3_pro_model(model_id) {
        serde_json::json!({"thinkingLevel": "LOW"})
    } else if is_gemini3_flash_model(model_id) {
        serde_json::json!({"thinkingLevel": "MINIMAL"})
    } else {
        // Gemini 2.x
        serde_json::json!({"thinkingBudget": 0})
    }
}

// =============================================================================
// Message Conversion
// =============================================================================

fn convert_messages(messages: &[Message], model: &Model) -> Vec<Value> {
    let mut contents: Vec<Value> = Vec::new();

    for msg in messages {
        match msg {
            Message::User(user) => {
                let parts = match &user.content {
                    crate::types::UserContent::Text(s) => {
                        vec![serde_json::json!({"text": sanitize_surrogates(s)})]
                    }
                    crate::types::UserContent::Blocks(blocks) => {
                        let mut parts = Vec::new();
                        for block in blocks {
                            match block {
                                UserContentBlock::Text(t) => {
                                    parts.push(
                                        serde_json::json!({"text": sanitize_surrogates(&t.text)}),
                                    );
                                }
                                UserContentBlock::Image(img) => {
                                    if model.input.contains(&InputType::Image) {
                                        parts.push(serde_json::json!({
                                            "inlineData": {
                                                "mimeType": img.mime_type,
                                                "data": img.data,
                                            }
                                        }));
                                    }
                                }
                            }
                        }
                        if parts.is_empty() {
                            continue;
                        }
                        parts
                    }
                };
                contents.push(serde_json::json!({
                    "role": "user",
                    "parts": parts,
                }));
            }
            Message::Assistant(assistant) => {
                let mut parts = Vec::new();
                let is_same_provider_and_model =
                    assistant.provider == model.provider && assistant.model == model.id;

                for block in &assistant.content {
                    match block {
                        AssistantContentBlock::Text(t) => {
                            if t.text.trim().is_empty() {
                                continue;
                            }
                            let mut part =
                                serde_json::json!({"text": sanitize_surrogates(&t.text)});
                            if let Some(sig) = &t.text_signature
                                && is_same_provider_and_model
                                && is_valid_thought_signature(sig)
                            {
                                part.as_object_mut().unwrap().insert(
                                    "thoughtSignature".to_string(),
                                    Value::String(sig.clone()),
                                );
                            }
                            parts.push(part);
                        }
                        AssistantContentBlock::Thinking(t) => {
                            if t.thinking.trim().is_empty() {
                                continue;
                            }
                            if is_same_provider_and_model {
                                let mut part = serde_json::json!({
                                    "thought": true,
                                    "text": sanitize_surrogates(&t.thinking),
                                });
                                if let Some(sig) = &t.thinking_signature
                                    && is_valid_thought_signature(sig)
                                {
                                    part.as_object_mut().unwrap().insert(
                                        "thoughtSignature".to_string(),
                                        Value::String(sig.clone()),
                                    );
                                }
                                parts.push(part);
                            } else {
                                // Convert thinking to plain text for cross-provider replay
                                parts.push(
                                    serde_json::json!({"text": sanitize_surrogates(&t.thinking)}),
                                );
                            }
                        }
                        AssistantContentBlock::ToolCall(tc) => {
                            let mut fc = serde_json::json!({
                                "name": tc.name,
                                "args": tc.arguments,
                            });
                            if requires_tool_call_id(&model.id) {
                                fc.as_object_mut()
                                    .unwrap()
                                    .insert("id".to_string(), Value::String(tc.id.clone()));
                            }

                            let mut part = serde_json::json!({"functionCall": fc});

                            // Handle thought signatures for Gemini 3
                            if is_same_provider_and_model
                                && let Some(sig) = &tc.thought_signature
                                && is_valid_thought_signature(sig)
                            {
                                part.as_object_mut().unwrap().insert(
                                    "thoughtSignature".to_string(),
                                    Value::String(sig.clone()),
                                );
                            }
                            if model.id.to_lowercase().contains("gemini-3") {
                                // Gemini 3 requires thoughtSignature on all function calls
                                if part.get("thoughtSignature").is_none() {
                                    part.as_object_mut().unwrap().insert(
                                        "thoughtSignature".to_string(),
                                        Value::String(
                                            "skip_thought_signature_validator".to_string(),
                                        ),
                                    );
                                }
                            }
                            parts.push(part);
                        }
                    }
                }

                if parts.is_empty() {
                    continue;
                }
                contents.push(serde_json::json!({
                    "role": "model",
                    "parts": parts,
                }));
            }
            Message::ToolResult(tr) => {
                let text_result: String = tr
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        ToolResultContent::Text(t) => Some(t.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                let response_value = if !text_result.is_empty() {
                    sanitize_surrogates(&text_result)
                } else {
                    String::new()
                };

                let response = if tr.is_error {
                    serde_json::json!({"error": response_value})
                } else {
                    serde_json::json!({"output": response_value})
                };

                let mut fc_response = serde_json::json!({
                    "name": tr.tool_name,
                    "response": response,
                });
                if requires_tool_call_id(&model.id) {
                    fc_response
                        .as_object_mut()
                        .unwrap()
                        .insert("id".to_string(), Value::String(tr.tool_call_id.clone()));
                }

                let part = serde_json::json!({"functionResponse": fc_response});

                // Merge tool results into the same user turn
                if let Some(last) = contents.last_mut()
                    && last.get("role").and_then(|r| r.as_str()) == Some("user")
                    && let Some(parts) = last.get("parts").and_then(|p| p.as_array())
                    && parts.iter().any(|p| p.get("functionResponse").is_some())
                {
                    last.as_object_mut()
                        .unwrap()
                        .get_mut("parts")
                        .unwrap()
                        .as_array_mut()
                        .unwrap()
                        .push(part);
                    continue;
                }

                contents.push(serde_json::json!({
                    "role": "user",
                    "parts": [part],
                }));
            }
        }
    }

    contents
}

fn convert_tools(tools: &[Tool]) -> Value {
    let declarations: Vec<Value> = tools
        .iter()
        .map(|tool| {
            serde_json::json!({
                "name": tool.name,
                "description": tool.description,
                "parametersJsonSchema": tool.parameters,
            })
        })
        .collect();

    serde_json::json!({"functionDeclarations": declarations})
}

// =============================================================================
// SSE Stream Parsing
// =============================================================================

async fn parse_sse_stream(
    response: reqwest::Response,
    model: &Model,
) -> Result<Vec<AssistantMessageEvent>, String> {
    let mut events = Vec::new();

    let mut output = AssistantMessage {
        role: "assistant".to_string(),
        content: vec![],
        api: Api::GoogleGenerativeAi,
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

    events.push(AssistantMessageEvent::Start {
        partial: output.clone(),
    });

    let body = response
        .text()
        .await
        .map_err(|e| format!("Failed to read response: {}", e))?;

    // Track current block state
    let mut current_block_type: Option<&str> = None; // "text" or "thinking"

    // Parse SSE events from the response
    for line in body.lines() {
        let line = line.trim();
        if !line.starts_with("data: ") {
            continue;
        }
        let data = &line[6..];
        if data == "[DONE]" {
            break;
        }

        let chunk: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Process candidates
        if let Some(candidates) = chunk.get("candidates").and_then(|c| c.as_array())
            && let Some(candidate) = candidates.first()
        {
            if let Some(parts) = candidate
                .get("content")
                .and_then(|c| c.get("parts"))
                .and_then(|p| p.as_array())
            {
                for part in parts {
                    if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                        let is_thinking = part
                            .get("thought")
                            .and_then(|t| t.as_bool())
                            .unwrap_or(false);

                        let block_type = if is_thinking { "thinking" } else { "text" };

                        // Handle block transitions
                        if current_block_type != Some(block_type) {
                            // End previous block
                            if let Some(prev_type) = current_block_type {
                                let idx = (output.content.len() - 1) as u32;
                                match prev_type {
                                    "text" => {
                                        let content = get_last_text_content(&output);
                                        events.push(AssistantMessageEvent::TextEnd {
                                            content_index: idx,
                                            content,
                                            partial: output.clone(),
                                        });
                                    }
                                    "thinking" => {
                                        let content = get_last_thinking_content(&output);
                                        events.push(AssistantMessageEvent::ThinkingEnd {
                                            content_index: idx,
                                            content,
                                            partial: output.clone(),
                                        });
                                    }
                                    _ => {}
                                }
                            }

                            // Start new block
                            if is_thinking {
                                output.content.push(AssistantContentBlock::Thinking(
                                    ThinkingContent::new(""),
                                ));
                                let idx = (output.content.len() - 1) as u32;
                                events.push(AssistantMessageEvent::ThinkingStart {
                                    content_index: idx,
                                    partial: output.clone(),
                                });
                            } else {
                                output
                                    .content
                                    .push(AssistantContentBlock::Text(TextContent::new("")));
                                let idx = (output.content.len() - 1) as u32;
                                events.push(AssistantMessageEvent::TextStart {
                                    content_index: idx,
                                    partial: output.clone(),
                                });
                            }
                            current_block_type =
                                Some(if is_thinking { "thinking" } else { "text" });
                        }

                        // Append delta
                        let idx = (output.content.len() - 1) as u32;
                        if is_thinking {
                            if let Some(AssistantContentBlock::Thinking(t)) =
                                output.content.last_mut()
                            {
                                t.thinking.push_str(text);
                                // Retain thought signature
                                if let Some(sig) =
                                    part.get("thoughtSignature").and_then(|s| s.as_str())
                                    && !sig.is_empty()
                                {
                                    t.thinking_signature = Some(sig.to_string());
                                }
                            }
                            events.push(AssistantMessageEvent::ThinkingDelta {
                                content_index: idx,
                                delta: text.to_string(),
                                partial: output.clone(),
                            });
                        } else {
                            if let Some(AssistantContentBlock::Text(t)) = output.content.last_mut()
                            {
                                t.text.push_str(text);
                                if let Some(sig) =
                                    part.get("thoughtSignature").and_then(|s| s.as_str())
                                    && !sig.is_empty()
                                {
                                    t.text_signature = Some(sig.to_string());
                                }
                            }
                            events.push(AssistantMessageEvent::TextDelta {
                                content_index: idx,
                                delta: text.to_string(),
                                partial: output.clone(),
                            });
                        }
                    }

                    // Handle function calls
                    if let Some(fc) = part.get("functionCall") {
                        // End any current text/thinking block
                        if let Some(prev_type) = current_block_type.take() {
                            let idx = (output.content.len() - 1) as u32;
                            match prev_type {
                                "text" => {
                                    let content = get_last_text_content(&output);
                                    events.push(AssistantMessageEvent::TextEnd {
                                        content_index: idx,
                                        content,
                                        partial: output.clone(),
                                    });
                                }
                                "thinking" => {
                                    let content = get_last_thinking_content(&output);
                                    events.push(AssistantMessageEvent::ThinkingEnd {
                                        content_index: idx,
                                        content,
                                        partial: output.clone(),
                                    });
                                }
                                _ => {}
                            }
                        }

                        let name = fc
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string();
                        let args = fc
                            .get("args")
                            .cloned()
                            .unwrap_or(Value::Object(serde_json::Map::new()));

                        // Generate unique tool call ID
                        let provided_id = fc.get("id").and_then(|id| id.as_str()).map(String::from);
                        let needs_new_id = provided_id.is_none()
                            || output.content.iter().any(|b| {
                                if let AssistantContentBlock::ToolCall(tc) = b {
                                    Some(tc.id.as_str()) == provided_id.as_deref()
                                } else {
                                    false
                                }
                            });

                        let tool_call_id = if needs_new_id {
                            let counter = TOOL_CALL_COUNTER.fetch_add(1, Ordering::Relaxed);
                            format!("{}_{}", name, counter)
                        } else {
                            provided_id.unwrap()
                        };

                        let thought_signature = part
                            .get("thoughtSignature")
                            .and_then(|s| s.as_str())
                            .map(String::from);

                        let tool_call = ToolCall {
                            content_type: "toolCall".to_string(),
                            id: tool_call_id,
                            name,
                            arguments: args.clone(),
                            thought_signature,
                        };

                        output
                            .content
                            .push(AssistantContentBlock::ToolCall(tool_call.clone()));
                        let idx = (output.content.len() - 1) as u32;

                        events.push(AssistantMessageEvent::ToolCallStart {
                            content_index: idx,
                            partial: output.clone(),
                        });
                        events.push(AssistantMessageEvent::ToolCallDelta {
                            content_index: idx,
                            delta: serde_json::to_string(&args).unwrap_or_default(),
                            partial: output.clone(),
                        });
                        events.push(AssistantMessageEvent::ToolCallEnd {
                            content_index: idx,
                            tool_call,
                            partial: output.clone(),
                        });
                    }
                }
            }

            // Check finish reason
            if let Some(reason) = candidate.get("finishReason").and_then(|r| r.as_str()) {
                output.stop_reason = map_stop_reason(reason);
                if output
                    .content
                    .iter()
                    .any(|b| matches!(b, AssistantContentBlock::ToolCall(_)))
                {
                    output.stop_reason = StopReason::ToolUse;
                }
            }
        }

        // Update usage
        if let Some(usage) = chunk.get("usageMetadata") {
            let input = usage
                .get("promptTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let candidates_tokens = usage
                .get("candidatesTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let thoughts_tokens = usage
                .get("thoughtsTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let cache_read = usage
                .get("cachedContentTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let total = usage
                .get("totalTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            output.usage.input = input;
            output.usage.output = candidates_tokens + thoughts_tokens;
            output.usage.cache_read = cache_read;
            output.usage.total_tokens = total;
            calculate_cost(model, &mut output.usage);
        }
    }

    // End the last block
    if let Some(prev_type) = current_block_type {
        let idx = (output.content.len() - 1) as u32;
        match prev_type {
            "text" => {
                let content = get_last_text_content(&output);
                events.push(AssistantMessageEvent::TextEnd {
                    content_index: idx,
                    content,
                    partial: output.clone(),
                });
            }
            "thinking" => {
                let content = get_last_thinking_content(&output);
                events.push(AssistantMessageEvent::ThinkingEnd {
                    content_index: idx,
                    content,
                    partial: output.clone(),
                });
            }
            _ => {}
        }
    }

    events.push(AssistantMessageEvent::Done {
        reason: output.stop_reason,
        message: output,
    });

    Ok(events)
}

// =============================================================================
// Helpers
// =============================================================================

fn get_last_text_content(output: &AssistantMessage) -> String {
    output
        .content
        .last()
        .and_then(|b| match b {
            AssistantContentBlock::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

fn get_last_thinking_content(output: &AssistantMessage) -> String {
    output
        .content
        .last()
        .and_then(|b| match b {
            AssistantContentBlock::Thinking(t) => Some(t.thinking.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// Models via Google APIs that require explicit tool call IDs in function calls/responses.
fn requires_tool_call_id(model_id: &str) -> bool {
    model_id.starts_with("claude-") || model_id.starts_with("gpt-oss-")
}

fn is_gemini3_pro_model(model_id: &str) -> bool {
    let lower = model_id.to_lowercase();
    // Match gemini-3-pro, gemini-3.0-pro, gemini-3.1-pro, etc.
    if let Some(rest) = lower.strip_prefix("gemini-3") {
        if let Some(after) = rest.strip_prefix('.') {
            // gemini-3.X-pro
            after.contains("-pro")
        } else {
            // gemini-3-pro
            rest.starts_with("-pro")
        }
    } else {
        false
    }
}

fn is_gemini3_flash_model(model_id: &str) -> bool {
    let lower = model_id.to_lowercase();
    // Match gemini-3-flash, gemini-3.0-flash, gemini-3.1-flash, etc.
    if let Some(rest) = lower.strip_prefix("gemini-3") {
        if let Some(after) = rest.strip_prefix('.') {
            after.contains("-flash")
        } else {
            rest.starts_with("-flash")
        }
    } else {
        false
    }
}

fn get_gemini3_thinking_level(effort: ThinkingLevel, model_id: &str) -> GoogleThinkingLevel {
    if is_gemini3_pro_model(model_id) {
        match effort {
            ThinkingLevel::Minimal | ThinkingLevel::Low => GoogleThinkingLevel::Low,
            ThinkingLevel::Medium | ThinkingLevel::High | ThinkingLevel::Xhigh => {
                GoogleThinkingLevel::High
            }
        }
    } else {
        match effort {
            ThinkingLevel::Minimal => GoogleThinkingLevel::Minimal,
            ThinkingLevel::Low => GoogleThinkingLevel::Low,
            ThinkingLevel::Medium => GoogleThinkingLevel::Medium,
            ThinkingLevel::High | ThinkingLevel::Xhigh => GoogleThinkingLevel::High,
        }
    }
}

fn get_google_budget(
    model_id: &str,
    effort: ThinkingLevel,
    custom_budgets: Option<&crate::types::ThinkingBudgets>,
) -> i32 {
    let level = match effort {
        ThinkingLevel::Xhigh => ThinkingLevel::High,
        other => other,
    };

    // Check custom budgets
    if let Some(budgets) = custom_budgets {
        let budget = match level {
            ThinkingLevel::Minimal => budgets.minimal,
            ThinkingLevel::Low => budgets.low,
            ThinkingLevel::Medium => budgets.medium,
            ThinkingLevel::High | ThinkingLevel::Xhigh => budgets.high,
        };
        if let Some(b) = budget {
            return b as i32;
        }
    }

    if model_id.contains("2.5-pro") {
        match level {
            ThinkingLevel::Minimal => 128,
            ThinkingLevel::Low => 2048,
            ThinkingLevel::Medium => 8192,
            ThinkingLevel::High | ThinkingLevel::Xhigh => 32768,
        }
    } else if model_id.contains("2.5-flash") {
        match level {
            ThinkingLevel::Minimal => 128,
            ThinkingLevel::Low => 2048,
            ThinkingLevel::Medium => 8192,
            ThinkingLevel::High | ThinkingLevel::Xhigh => 24576,
        }
    } else {
        -1 // Dynamic
    }
}

fn map_stop_reason(reason: &str) -> StopReason {
    match reason {
        "STOP" => StopReason::Stop,
        "MAX_TOKENS" => StopReason::Length,
        _ => StopReason::Error,
    }
}

/// Check if a thought signature is valid base64.
fn is_valid_thought_signature(sig: &str) -> bool {
    if sig.is_empty() || sig.len() % 4 != 0 {
        return false;
    }
    sig.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
}

/// Sanitize unicode surrogate pairs in text.
fn sanitize_surrogates(text: &str) -> String {
    // Rust strings are already valid UTF-8, so surrogate pairs don't exist.
    // This is a no-op but kept for API compatibility.
    text.to_string()
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
    use crate::types::{Cost, UserMessage};

    fn test_model() -> Model {
        Model {
            id: "gemini-2.5-flash".into(),
            name: "Gemini 2.5 Flash".into(),
            api: Api::GoogleGenerativeAi,
            provider: Provider::Google,
            base_url: String::new(),
            reasoning: true,
            input: vec![InputType::Text, InputType::Image],
            cost: Cost {
                input: 0.15,
                output: 0.6,
                cache_read: 0.0375,
                cache_write: 0.0,
            },
            context_window: 1_048_576,
            max_tokens: 65536,
            headers: None,
            compat: None,
            thinking_level_map: None,
        }
    }

    #[test]
    fn test_convert_messages_user_text() {
        let model = test_model();
        let messages = vec![Message::User(UserMessage::new_text("Hello"))];
        let contents = convert_messages(&messages, &model);
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[0]["parts"][0]["text"], "Hello");
    }

    #[test]
    fn test_convert_messages_assistant() {
        let model = test_model();
        let messages = vec![Message::Assistant(AssistantMessage {
            role: "assistant".into(),
            content: vec![AssistantContentBlock::Text(TextContent::new("Hi"))],
            api: Api::GoogleGenerativeAi,
            provider: Provider::Google,
            model: "gemini-2.5-flash".into(),
            stop_reason: StopReason::Stop,
            usage: Usage::default(),
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        })];
        let contents = convert_messages(&messages, &model);
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "model");
        assert_eq!(contents[0]["parts"][0]["text"], "Hi");
    }

    #[test]
    fn test_convert_messages_tool_result_merges() {
        let model = test_model();
        let messages = vec![
            Message::ToolResult(crate::types::ToolResultMessage::new(
                "tc1",
                "read",
                vec![ToolResultContent::Text(TextContent::new("file contents"))],
            )),
            Message::ToolResult(crate::types::ToolResultMessage::new(
                "tc2",
                "ls",
                vec![ToolResultContent::Text(TextContent::new("dir listing"))],
            )),
        ];
        let contents = convert_messages(&messages, &model);
        // Both tool results should be merged into a single user turn
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "user");
        let parts = contents[0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert!(parts[0].get("functionResponse").is_some());
        assert!(parts[1].get("functionResponse").is_some());
    }

    #[test]
    fn test_convert_tools() {
        let tools = vec![Tool::new(
            "read",
            "Read a file",
            serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        )];
        let result = convert_tools(&tools);
        let decls = result["functionDeclarations"].as_array().unwrap();
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0]["name"], "read");
    }

    #[test]
    fn test_map_stop_reason() {
        assert_eq!(map_stop_reason("STOP"), StopReason::Stop);
        assert_eq!(map_stop_reason("MAX_TOKENS"), StopReason::Length);
        assert_eq!(map_stop_reason("SAFETY"), StopReason::Error);
    }

    #[test]
    fn test_is_valid_thought_signature() {
        assert!(is_valid_thought_signature("YWJj")); // "abc" in base64
        assert!(!is_valid_thought_signature(""));
        assert!(!is_valid_thought_signature("abc")); // not padded to 4
    }

    #[test]
    fn test_requires_tool_call_id() {
        assert!(requires_tool_call_id("claude-3-haiku"));
        assert!(requires_tool_call_id("gpt-oss-4o"));
        assert!(!requires_tool_call_id("gemini-2.5-flash"));
    }

    #[test]
    fn test_is_gemini3_models() {
        assert!(is_gemini3_pro_model("gemini-3-pro"));
        assert!(is_gemini3_pro_model("gemini-3.1-pro-latest"));
        assert!(!is_gemini3_pro_model("gemini-2.5-pro"));
        assert!(is_gemini3_flash_model("gemini-3-flash"));
        assert!(!is_gemini3_flash_model("gemini-2.5-flash"));
    }

    #[test]
    fn test_get_google_budget() {
        assert_eq!(
            get_google_budget("gemini-2.5-pro", ThinkingLevel::Low, None),
            2048
        );
        assert_eq!(
            get_google_budget("gemini-2.5-flash", ThinkingLevel::High, None),
            24576
        );
        assert_eq!(
            get_google_budget("gemini-2.5-pro", ThinkingLevel::Minimal, None),
            128
        );

        // Custom budgets override
        let budgets = crate::types::ThinkingBudgets {
            low: Some(4096),
            ..Default::default()
        };
        assert_eq!(
            get_google_budget("gemini-2.5-pro", ThinkingLevel::Low, Some(&budgets)),
            4096
        );
    }

    #[test]
    fn test_get_gemini3_thinking_level() {
        // Pro model maps low -> LOW, high -> HIGH
        let level = get_gemini3_thinking_level(ThinkingLevel::Low, "gemini-3-pro");
        assert!(matches!(level, GoogleThinkingLevel::Low));
        let level = get_gemini3_thinking_level(ThinkingLevel::High, "gemini-3-pro");
        assert!(matches!(level, GoogleThinkingLevel::High));

        // Flash model has all levels
        let level = get_gemini3_thinking_level(ThinkingLevel::Minimal, "gemini-3-flash");
        assert!(matches!(level, GoogleThinkingLevel::Minimal));
        let level = get_gemini3_thinking_level(ThinkingLevel::Medium, "gemini-3-flash");
        assert!(matches!(level, GoogleThinkingLevel::Medium));
    }

    #[test]
    fn test_disabled_thinking_config() {
        let config = get_disabled_thinking_config("gemini-3-pro");
        assert_eq!(config["thinkingLevel"], "LOW");

        let config = get_disabled_thinking_config("gemini-3-flash");
        assert_eq!(config["thinkingLevel"], "MINIMAL");

        let config = get_disabled_thinking_config("gemini-2.5-flash");
        assert_eq!(config["thinkingBudget"], 0);
    }

    #[test]
    fn test_build_request_body_basic() {
        let model = test_model();
        let context = Context {
            system_prompt: Some("You are helpful.".into()),
            messages: vec![Message::User(UserMessage::new_text("Hi"))],
            tools: None,
        };
        let options = GoogleOptions::default();
        let body = build_request_body(&model, &context, &options).unwrap();

        assert!(body.get("contents").is_some());
        assert!(body.get("systemInstruction").is_some());
        assert_eq!(
            body["systemInstruction"]["parts"][0]["text"],
            "You are helpful."
        );
    }

    #[test]
    fn test_build_request_body_with_thinking() {
        let model = test_model();
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new_text("Think about this"))],
            tools: None,
        };
        let options = GoogleOptions {
            thinking_enabled: true,
            thinking_budget_tokens: Some(8192),
            ..Default::default()
        };
        let body = build_request_body(&model, &context, &options).unwrap();

        let gen_config = &body["generationConfig"];
        assert_eq!(gen_config["thinkingConfig"]["includeThoughts"], true);
        assert_eq!(gen_config["thinkingConfig"]["thinkingBudget"], 8192);
    }

    #[test]
    fn test_thinking_block_cross_provider() {
        let model = test_model();
        // Assistant message from a different provider
        let messages = vec![Message::Assistant(AssistantMessage {
            role: "assistant".into(),
            content: vec![AssistantContentBlock::Thinking(ThinkingContent::new(
                "reasoning here",
            ))],
            api: Api::AnthropicMessages,
            provider: Provider::Anthropic,
            model: "claude-sonnet-4-20250514".into(),
            stop_reason: StopReason::Stop,
            usage: Usage::default(),
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        })];
        let contents = convert_messages(&messages, &model);
        // Should convert thinking to plain text since different provider
        assert_eq!(contents.len(), 1);
        let parts = contents[0]["parts"].as_array().unwrap();
        assert_eq!(parts[0]["text"], "reasoning here");
        assert!(parts[0].get("thought").is_none());
    }
}
