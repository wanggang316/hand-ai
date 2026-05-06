//! OpenAI Responses API provider.
//!
//! Implements the OpenAI Responses API format used by o1, o3, and newer models.
//! This API uses `/v1/responses` endpoint with a different event/streaming format
//! than the `/v1/chat/completions` endpoint.
//!
//! Key differences from OpenAI Completions:
//! - Endpoint: POST /v1/responses (not /v1/chat/completions)
//! - Request format: "input" array instead of "messages"
//! - Function calls use "function_call" output items (not tool_calls in delta)
//! - Streaming events: response.created, response.output_item.added,
//!   response.content_part.added, response.output_text.delta,
//!   response.function_call_arguments.delta, response.completed
//! - Built-in reasoning/thinking support with "reasoning" parameter

use crate::api_registry::{ApiProvider, AssistantMessageEventStream};
use crate::env_api_keys;
use crate::types::{
    Api, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Context, Message, Model,
    Provider, SimpleStreamOptions, StopReason, StreamOptions, TextContent, ThinkingContent,
    ToolCall, Usage,
};
use futures::StreamExt;
use serde_json::Value;

/// Provider for OpenAI Responses API.
#[derive(Debug, Clone, Copy, Default)]
pub struct OpenAIResponsesProvider;

impl OpenAIResponsesProvider {
    pub fn new() -> Self {
        Self
    }
}

impl ApiProvider for OpenAIResponsesProvider {
    fn stream(
        &self,
        model: Model,
        context: Context,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        stream_openai_responses(model, context, options)
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

        stream_openai_responses(model, context, Some(base))
    }
}

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

/// Build the request body for the Responses API.
fn build_request_body(model: &Model, context: &Context, options: &StreamOptions) -> Value {
    let mut body = serde_json::json!({
        "model": model.id,
        "stream": true,
    });

    // Convert messages to "input" format
    let input = convert_to_input(context);
    body["input"] = input;

    // System prompt goes as "instructions"
    if let Some(ref prompt) = context.system_prompt
        && !prompt.is_empty()
    {
        body["instructions"] = Value::String(prompt.clone());
    }

    // Tools
    if let Some(ref tools_list) = context.tools
        && !tools_list.is_empty()
    {
        let tools: Vec<Value> = tools_list
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                })
            })
            .collect();
        body["tools"] = Value::Array(tools);
    }

    // Temperature
    if let Some(temp) = options.temperature {
        body["temperature"] = Value::from(temp);
    }

    // Max tokens
    if let Some(max) = options.max_tokens.or(Some(model.max_tokens as u32)) {
        body["max_output_tokens"] = Value::from(max);
    }

    body
}

/// Convert messages to the Responses API "input" format.
fn convert_to_input(context: &Context) -> Value {
    let mut input = Vec::new();

    for msg in &context.messages {
        match msg {
            Message::User(u) => {
                let text = match &u.content {
                    crate::types::UserContent::Text(s) => s.clone(),
                    crate::types::UserContent::Blocks(blocks) => blocks
                        .iter()
                        .filter_map(|b| match b {
                            crate::types::UserContentBlock::Text(t) => Some(t.text.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                };
                input.push(serde_json::json!({
                    "type": "message",
                    "role": "user",
                    "content": text,
                }));
            }
            Message::Assistant(a) => {
                // Convert assistant content blocks to output items
                let mut items = Vec::new();
                for block in &a.content {
                    match block {
                        AssistantContentBlock::Text(t) => {
                            items.push(serde_json::json!({
                                "type": "message",
                                "role": "assistant",
                                "content": t.text,
                            }));
                        }
                        AssistantContentBlock::ToolCall(tc) => {
                            items.push(serde_json::json!({
                                "type": "function_call",
                                "name": tc.name,
                                "call_id": tc.id,
                                "arguments": serde_json::to_string(&tc.arguments).unwrap_or_default(),
                            }));
                        }
                        AssistantContentBlock::Thinking(_) => {
                            // Thinking blocks are not sent back
                        }
                    }
                }
                input.extend(items);
            }
            Message::ToolResult(tr) => {
                input.push(serde_json::json!({
                    "type": "function_call_output",
                    "call_id": tr.tool_call_id,
                    "output": tr.content.iter().filter_map(|c| match c {
                        crate::types::ToolResultContent::Text(t) => Some(t.text.clone()),
                        _ => None,
                    }).collect::<Vec<_>>().join("\n"),
                }));
            }
        }
    }

    Value::Array(input)
}

/// Stream from the OpenAI Responses API using SSE.
fn stream_openai_responses(
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

        // Build request
        let body = build_request_body(&model, &context, &options);
        let base_url = if model.base_url.is_empty() {
            "https://api.openai.com".to_string()
        } else {
            model.base_url.clone()
        };
        let url = format!("{}/v1/responses", base_url);

        let api_key = options.api_key
            .or_else(|| env_api_keys::get_env_api_key(&model.provider))
            .unwrap_or_default();

        // Make HTTP request
        let client = reqwest::Client::new();
        let mut builder = client.post(&url)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", api_key))
            .body(serde_json::to_string(&body).unwrap_or_default());

        // Add custom headers
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

        // Parse SSE stream
        let mut text_buffer = String::new();
        let mut current_tool_name = String::new();
        let mut current_tool_id = String::new();
        let mut current_tool_args = String::new();
        let mut thinking_buffer = String::new();

        let mut byte_stream = response.bytes_stream();
        let mut line_buffer = String::new();

        while let Some(chunk) = byte_stream.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    output.error_message = Some(format!("Stream error: {}", e));
                    break;
                }
            };

            let text = String::from_utf8_lossy(&chunk);
            line_buffer.push_str(&text);

            while let Some(idx) = line_buffer.find("\n\n") {
                let event_block = line_buffer[..idx].to_string();
                line_buffer = line_buffer[idx + 2..].to_string();

                // Parse SSE event
                let mut event_type = String::new();
                let mut event_data = String::new();

                for line in event_block.lines() {
                    if let Some(rest) = line.strip_prefix("event: ") {
                        event_type = rest.to_string();
                    } else if let Some(rest) = line.strip_prefix("data: ") {
                        event_data = rest.to_string();
                    }
                }

                if event_data.is_empty() || event_data == "[DONE]" {
                    continue;
                }

                let data: Value = match serde_json::from_str(&event_data) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                match event_type.as_str() {
                    "response.output_text.delta" => {
                        if let Some(delta) = data.get("delta").and_then(|d| d.as_str()) {
                            text_buffer.push_str(delta);
                            yield AssistantMessageEvent::TextDelta {
                                content_index: 0,
                                delta: delta.to_string(),
                                partial: output.clone(),
                            };
                        }
                    }

                    "response.reasoning_summary_text.delta" => {
                        if let Some(delta) = data.get("delta").and_then(|d| d.as_str()) {
                            thinking_buffer.push_str(delta);
                            yield AssistantMessageEvent::ThinkingDelta {
                                content_index: 0,
                                delta: delta.to_string(),
                                partial: output.clone(),
                            };
                        }
                    }

                    "response.function_call_arguments.delta" => {
                        if let Some(delta) = data.get("delta").and_then(|d| d.as_str()) {
                            current_tool_args.push_str(delta);
                        }
                    }

                    "response.output_item.added" => {
                        if let Some(item) = data.get("item")
                            && item.get("type").and_then(|t| t.as_str()) == Some("function_call") {
                                current_tool_name = item.get("name")
                                    .and_then(|n| n.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                current_tool_id = item.get("call_id")
                                    .and_then(|c| c.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                current_tool_args.clear();
                            }
                    }

                    "response.output_item.done" => {
                        if let Some(item) = data.get("item") {
                            let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
                            if item_type == "function_call" {
                                let args: Value = serde_json::from_str(&current_tool_args)
                                    .unwrap_or(Value::Object(Default::default()));
                                output.content.push(AssistantContentBlock::ToolCall(ToolCall {
                                    content_type: "tool_call".to_string(),
                                    id: current_tool_id.clone(),
                                    name: current_tool_name.clone(),
                                    arguments: args,
                                    thought_signature: None,
                                }));
                                output.stop_reason = StopReason::ToolUse;
                                current_tool_name.clear();
                                current_tool_id.clear();
                                current_tool_args.clear();
                            }
                        }
                    }

                    "response.content_part.done" => {
                        // Finalize text part
                        if !text_buffer.is_empty() {
                            output.content.push(AssistantContentBlock::Text(TextContent {
                                content_type: "text".to_string(),
                                text: text_buffer.clone(),
                                text_signature: None,
                            }));
                        }
                    }

                    "response.completed" => {
                        // Extract usage
                        if let Some(response) = data.get("response")
                            && let Some(usage) = response.get("usage") {
                                output.usage.input = usage.get("input_tokens")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0);
                                output.usage.output = usage.get("output_tokens")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0);
                            }
                    }

                    _ => {}
                }
            }
        }

        // Finalize thinking
        if !thinking_buffer.is_empty() {
            output.content.insert(0, AssistantContentBlock::Thinking(ThinkingContent {
                content_type: "thinking".to_string(),
                thinking: thinking_buffer,
                thinking_signature: None,
                redacted: None,
            }));
        }

        // Finalize text if not already done
        if !text_buffer.is_empty() && !output.content.iter().any(|c| matches!(c, AssistantContentBlock::Text(_))) {
            output.content.push(AssistantContentBlock::Text(TextContent {
                content_type: "text".to_string(),
                text: text_buffer,
                text_signature: None,
            }));
        }

        yield AssistantMessageEvent::Done {
            reason: output.stop_reason,
            message: output,
        };
    })
}

fn current_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Cost, InputType, UserMessage};

    fn test_model() -> Model {
        Model {
            id: "o3-mini".to_string(),
            name: "o3-mini".to_string(),
            api: Api::OpenAIResponses,
            provider: Provider::OpenAI,
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

    fn test_context() -> Context {
        Context {
            system_prompt: Some("You are a helpful assistant.".to_string()),
            messages: vec![Message::User(UserMessage::new_text("hello"))],
            tools: None,
        }
    }

    #[test]
    fn test_build_request_body() {
        let model = test_model();
        let context = test_context();
        let options = StreamOptions::default();

        let body = build_request_body(&model, &context, &options);
        assert_eq!(body["model"], "o3-mini");
        assert_eq!(body["stream"], true);
        assert!(body["instructions"].is_string());
        assert!(body["input"].is_array());
    }

    #[test]
    fn test_convert_to_input_user() {
        let context = test_context();
        let input = convert_to_input(&context);
        let arr = input.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["role"], "user");
        assert_eq!(arr[0]["content"], "hello");
    }

    #[test]
    fn test_convert_to_input_tool_result() {
        let context = Context {
            system_prompt: None,
            messages: vec![Message::ToolResult(crate::types::ToolResultMessage {
                role: "toolResult".to_string(),
                tool_call_id: "call_123".to_string(),
                tool_name: "read".to_string(),
                content: vec![crate::types::ToolResultContent::Text(TextContent {
                    content_type: "text".to_string(),
                    text: "file contents".to_string(),
                    text_signature: None,
                })],
                details: None,
                is_error: false,
                timestamp: 0,
            })],
            tools: None,
        };
        let input = convert_to_input(&context);
        let arr = input.as_array().unwrap();
        assert_eq!(arr[0]["type"], "function_call_output");
        assert_eq!(arr[0]["call_id"], "call_123");
    }

    #[test]
    fn test_provider_creation() {
        let _provider = OpenAIResponsesProvider::new();
    }

    #[test]
    fn test_build_request_with_tools() {
        let model = test_model();
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new_text("hi"))],
            tools: Some(vec![crate::types::Tool {
                name: "read".to_string(),
                description: "Read a file".to_string(),
                parameters: serde_json::json!({"type": "object", "properties": {}}),
            }]),
        };
        let options = StreamOptions::default();
        let body = build_request_body(&model, &context, &options);
        assert!(body["tools"].is_array());
        assert_eq!(body["tools"].as_array().unwrap().len(), 1);
    }
}
