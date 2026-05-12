//! Shared helpers for the OpenAI Responses API used by both
//! `openai_responses` and `azure_openai_responses` providers.
//!
//! The wire format (request body shape, SSE event names, output-item parsing)
//! is identical across the two endpoints — only the URL construction and
//! authentication header differ. Those provider-specific concerns stay in
//! each provider module; everything below is the common payload and stream
//! decoder.

use crate::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Context, Message, Model,
    StopReason, StreamOptions, TextContent, ThinkingContent, ToolCall,
};
use futures::StreamExt;
use serde_json::Value;

/// Build the JSON request body for the Responses API.
///
/// Mirrors the request shape used by `pi-mono`'s
/// `openai-responses-shared.ts`. Values that are `None`/empty are omitted so
/// the wire payload stays compact.
pub(crate) fn build_request_body(
    model: &Model,
    context: &Context,
    options: &StreamOptions,
) -> Value {
    let mut body = serde_json::json!({
        "model": model.id,
        "stream": true,
    });

    body["input"] = convert_to_input(context);

    if let Some(ref prompt) = context.system_prompt
        && !prompt.is_empty()
    {
        body["instructions"] = Value::String(prompt.clone());
    }

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

    if let Some(temp) = options.temperature {
        body["temperature"] = Value::from(temp);
    }

    if let Some(max) = options.max_tokens.or(Some(model.max_tokens as u32)) {
        body["max_output_tokens"] = Value::from(max);
    }

    body
}

/// Convert a `Context` into the `input` array accepted by the Responses API.
pub(crate) fn convert_to_input(context: &Context) -> Value {
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
                for block in &a.content {
                    match block {
                        AssistantContentBlock::Text(t) => {
                            input.push(serde_json::json!({
                                "type": "message",
                                "role": "assistant",
                                "content": t.text,
                            }));
                        }
                        AssistantContentBlock::ToolCall(tc) => {
                            input.push(serde_json::json!({
                                "type": "function_call",
                                "name": tc.name,
                                "call_id": tc.id,
                                "arguments": serde_json::to_string(&tc.arguments).unwrap_or_default(),
                            }));
                        }
                        AssistantContentBlock::Thinking(_) => {
                            // Thinking blocks are not echoed back to the API.
                        }
                    }
                }
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

/// Drive the SSE stream of a successful Responses API response.
///
/// Returns a stream of `AssistantMessageEvent`s for the body of the
/// conversation (text deltas, thinking deltas, etc.). The terminal
/// `Start` / `Done` / `Error` events are the caller's responsibility — they
/// vary by provider and depend on state the caller owns. `output` is the
/// shared assistant-message accumulator; the parser mutates it in place
/// (appending content blocks, recording usage, updating `stop_reason`) so
/// the caller can emit a fully-populated `Done` event after the stream
/// completes.
pub(crate) fn drive_sse_stream(
    response: reqwest::Response,
    output: &mut AssistantMessage,
) -> impl futures::Stream<Item = AssistantMessageEvent> + '_ {
    async_stream::stream! {
        let mut text_buffer = String::new();
        let mut current_tool_name = String::new();
        let mut current_tool_id = String::new();
        let mut current_tool_args = String::new();
        let mut thinking_buffer = String::new();

        let mut byte_stream = response.bytes_stream();
        let mut line_buffer = String::new();

        'outer: while let Some(chunk) = byte_stream.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    output.error_message = Some(format!("Stream error: {}", e));
                    output.stop_reason = StopReason::Error;
                    yield AssistantMessageEvent::Error {
                        reason: StopReason::Error,
                        error: output.clone(),
                    };
                    break 'outer;
                }
            };

            let text = String::from_utf8_lossy(&chunk);
            line_buffer.push_str(&text);

            while let Some(idx) = line_buffer.find("\n\n") {
                let event_block = line_buffer[..idx].to_string();
                line_buffer = line_buffer[idx + 2..].to_string();

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
                            && item.get("type").and_then(|t| t.as_str()) == Some("function_call")
                        {
                            current_tool_name = item
                                .get("name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("")
                                .to_string();
                            current_tool_id = item
                                .get("call_id")
                                .and_then(|c| c.as_str())
                                .unwrap_or("")
                                .to_string();
                            current_tool_args.clear();
                        }
                    }

                    "response.output_item.done" => {
                        if let Some(item) = data.get("item") {
                            let item_type =
                                item.get("type").and_then(|t| t.as_str()).unwrap_or("");
                            if item_type == "function_call" {
                                // Apply the same control-byte / invalid-escape
                                // repair the other streaming providers use,
                                // so a malformed argument payload doesn't
                                // silently drop the entire tool call to `{}`.
                                let args: Value = if current_tool_args.is_empty() {
                                    Value::Object(Default::default())
                                } else {
                                    serde_json::from_str(&current_tool_args)
                                        .ok()
                                        .or_else(|| {
                                            crate::transform::parse_json_with_repair(
                                                &current_tool_args,
                                            )
                                        })
                                        .unwrap_or(Value::Object(Default::default()))
                                };
                                output
                                    .content
                                    .push(AssistantContentBlock::ToolCall(ToolCall {
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

                    "response.content_part.done" if !text_buffer.is_empty() => {
                        output
                            .content
                            .push(AssistantContentBlock::Text(TextContent {
                                content_type: "text".to_string(),
                                text: text_buffer.clone(),
                                text_signature: None,
                            }));
                    }

                    "response.completed" => {
                        if let Some(response) = data.get("response")
                            && let Some(usage) = response.get("usage")
                        {
                            output.usage.input = usage
                                .get("input_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                            output.usage.output = usage
                                .get("output_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                        }
                    }

                    _ => {}
                }
            }
        }

        if !thinking_buffer.is_empty() {
            output.content.insert(
                0,
                AssistantContentBlock::Thinking(ThinkingContent {
                    content_type: "thinking".to_string(),
                    thinking: thinking_buffer,
                    thinking_signature: None,
                    redacted: None,
                }),
            );
        }

        if !text_buffer.is_empty()
            && !output
                .content
                .iter()
                .any(|c| matches!(c, AssistantContentBlock::Text(_)))
        {
            output
                .content
                .push(AssistantContentBlock::Text(TextContent {
                    content_type: "text".to_string(),
                    text: text_buffer,
                    text_signature: None,
                }));
        }
    }
}

/// Best-effort millisecond Unix timestamp; used for assistant-message
/// timestamps where strict monotonicity is not required.
pub(crate) fn current_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
