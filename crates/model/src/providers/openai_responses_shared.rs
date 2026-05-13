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
/// Values that are `None` or empty are omitted so the wire payload stays
/// compact.
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

/// Mutable parser state shared across SSE event dispatches. Extracted out
/// of [`drive_sse_stream`] so the per-event branching can be unit-tested
/// without standing up an HTTP server.
#[derive(Default)]
pub(crate) struct ResponsesParseState {
    pub(crate) text_buffer: String,
    pub(crate) thinking_buffer: String,
    pub(crate) current_tool_name: String,
    pub(crate) current_tool_id: String,
    pub(crate) current_tool_args: String,
}

/// Dispatch a single decoded SSE event into the parser. Returns the events
/// the caller should yield (zero or one for every supported event type).
/// Handles both `response.reasoning_summary_text.delta` (OpenAI's native
/// reasoning summary stream) and `response.reasoning_text.delta` (LM Studio
/// and other Responses-compatible providers).
pub(crate) fn dispatch_responses_event(
    state: &mut ResponsesParseState,
    output: &mut AssistantMessage,
    event_type: &str,
    data: &Value,
) -> Vec<AssistantMessageEvent> {
    let mut emitted = Vec::new();
    match event_type {
        "response.output_text.delta" => {
            if let Some(delta) = data.get("delta").and_then(|d| d.as_str()) {
                state.text_buffer.push_str(delta);
                emitted.push(AssistantMessageEvent::TextDelta {
                    content_index: 0,
                    delta: delta.to_string(),
                    partial: output.clone(),
                });
            }
        }

        // Two reasoning-text channels share the same accumulator. OpenAI's
        // native Responses endpoint emits `reasoning_summary_text.delta`;
        // LM Studio and other Responses-compatible servers emit
        // `reasoning_text.delta`.
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            if let Some(delta) = data.get("delta").and_then(|d| d.as_str()) {
                state.thinking_buffer.push_str(delta);
                emitted.push(AssistantMessageEvent::ThinkingDelta {
                    content_index: 0,
                    delta: delta.to_string(),
                    partial: output.clone(),
                });
            }
        }

        "response.function_call_arguments.delta" => {
            if let Some(delta) = data.get("delta").and_then(|d| d.as_str()) {
                state.current_tool_args.push_str(delta);
            }
        }

        "response.output_item.added" => {
            if let Some(item) = data.get("item")
                && item.get("type").and_then(|t| t.as_str()) == Some("function_call")
            {
                state.current_tool_name = item
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                state.current_tool_id = item
                    .get("call_id")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string();
                state.current_tool_args.clear();
            }
        }

        "response.output_item.done" => {
            if let Some(item) = data.get("item") {
                let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
                if item_type == "function_call" {
                    // Apply the same control-byte / invalid-escape repair the
                    // other streaming providers use, so a malformed argument
                    // payload doesn't silently drop the entire tool call to `{}`.
                    let args: Value = if state.current_tool_args.is_empty() {
                        Value::Object(Default::default())
                    } else {
                        serde_json::from_str(&state.current_tool_args)
                            .ok()
                            .or_else(|| {
                                crate::transform::parse_json_with_repair(&state.current_tool_args)
                            })
                            .unwrap_or(Value::Object(Default::default()))
                    };
                    output
                        .content
                        .push(AssistantContentBlock::ToolCall(ToolCall {
                            content_type: "tool_call".to_string(),
                            id: state.current_tool_id.clone(),
                            name: state.current_tool_name.clone(),
                            arguments: args,
                            thought_signature: None,
                        }));
                    output.stop_reason = StopReason::ToolUse;
                    state.current_tool_name.clear();
                    state.current_tool_id.clear();
                    state.current_tool_args.clear();
                } else if item_type == "reasoning" {
                    // Prefer the server-authoritative summary/content text
                    // over whatever streamed via deltas. Some servers emit
                    // a final `item.content[].text` even when they did not
                    // stream `*_text.delta` events at all, so this branch
                    // is the only path that captures the thinking text for
                    // those transports.
                    let summary_text = item
                        .get("summary")
                        .and_then(|s| s.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.get("text").and_then(|t| t.as_str()))
                                .collect::<Vec<_>>()
                                .join("\n\n")
                        })
                        .unwrap_or_default();
                    let content_text = item
                        .get("content")
                        .and_then(|c| c.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.get("text").and_then(|t| t.as_str()))
                                .collect::<Vec<_>>()
                                .join("\n\n")
                        })
                        .unwrap_or_default();
                    if !summary_text.is_empty() {
                        state.thinking_buffer = summary_text;
                    } else if !content_text.is_empty() {
                        state.thinking_buffer = content_text;
                    }
                }
            }
        }

        "response.content_part.done" if !state.text_buffer.is_empty() => {
            output
                .content
                .push(AssistantContentBlock::Text(TextContent {
                    content_type: "text".to_string(),
                    text: state.text_buffer.clone(),
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
    emitted
}

/// Flush trailing buffered text/thinking into the output message at the end
/// of an SSE stream. Mirrors the post-loop logic the previous monolithic
/// `drive_sse_stream` ran inline.
pub(crate) fn finalize_responses_output(state: ResponsesParseState, output: &mut AssistantMessage) {
    if !state.thinking_buffer.is_empty() {
        output.content.insert(
            0,
            AssistantContentBlock::Thinking(ThinkingContent {
                content_type: "thinking".to_string(),
                thinking: state.thinking_buffer,
                thinking_signature: None,
                redacted: None,
            }),
        );
    }

    if !state.text_buffer.is_empty()
        && !output
            .content
            .iter()
            .any(|c| matches!(c, AssistantContentBlock::Text(_)))
    {
        output
            .content
            .push(AssistantContentBlock::Text(TextContent {
                content_type: "text".to_string(),
                text: state.text_buffer,
                text_signature: None,
            }));
    }
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
        let mut state = ResponsesParseState::default();

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

                for ev in dispatch_responses_event(&mut state, output, &event_type, &data) {
                    yield ev;
                }
            }
        }

        finalize_responses_output(state, output);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Api, Provider, Usage};
    use serde_json::json;

    fn empty_assistant_message() -> AssistantMessage {
        AssistantMessage {
            role: "assistant".to_string(),
            content: vec![],
            api: Api::OpenAIResponses,
            provider: Provider::OpenAI,
            model: "test".to_string(),
            stop_reason: StopReason::Stop,
            usage: Usage::default(),
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        }
    }

    fn run_events(events: &[(&str, Value)]) -> AssistantMessage {
        let mut state = ResponsesParseState::default();
        let mut output = empty_assistant_message();
        for (ty, data) in events {
            let _ = dispatch_responses_event(&mut state, &mut output, ty, data);
        }
        finalize_responses_output(state, &mut output);
        output
    }

    /// LM Studio and other Responses-compatible servers emit
    /// `response.reasoning_text.delta` instead of OpenAI's native
    /// `response.reasoning_summary_text.delta`. Both must produce
    /// ThinkingDelta events and accumulate into the final thinking block.
    #[test]
    fn reasoning_text_delta_emits_thinking_and_persists_block() {
        let mut state = ResponsesParseState::default();
        let mut output = empty_assistant_message();

        let ev1 = dispatch_responses_event(
            &mut state,
            &mut output,
            "response.reasoning_text.delta",
            &json!({ "delta": "Let me " }),
        );
        let ev2 = dispatch_responses_event(
            &mut state,
            &mut output,
            "response.reasoning_text.delta",
            &json!({ "delta": "think." }),
        );

        assert_eq!(ev1.len(), 1, "first delta must emit one event");
        assert_eq!(ev2.len(), 1, "second delta must emit one event");
        assert!(matches!(
            ev1[0],
            AssistantMessageEvent::ThinkingDelta { .. }
        ));
        assert!(matches!(
            ev2[0],
            AssistantMessageEvent::ThinkingDelta { .. }
        ));

        finalize_responses_output(state, &mut output);

        let thinking_block = output
            .content
            .iter()
            .find_map(|c| match c {
                AssistantContentBlock::Thinking(t) => Some(t),
                _ => None,
            })
            .expect("thinking block should be present after finalize");
        assert_eq!(thinking_block.thinking, "Let me think.");
    }

    /// Both `reasoning_text.delta` and `reasoning_summary_text.delta` must
    /// share the same thinking accumulator so providers that mix-and-match
    /// (or change their event names mid-stream) still produce one coherent
    /// thinking block.
    #[test]
    fn reasoning_text_and_summary_share_accumulator() {
        let output = run_events(&[
            (
                "response.reasoning_text.delta",
                json!({ "delta": "step 1; " }),
            ),
            (
                "response.reasoning_summary_text.delta",
                json!({ "delta": "step 2." }),
            ),
        ]);
        let thinking = output
            .content
            .iter()
            .find_map(|c| match c {
                AssistantContentBlock::Thinking(t) => Some(t.thinking.clone()),
                _ => None,
            })
            .expect("thinking block missing");
        assert_eq!(thinking, "step 1; step 2.");
    }

    /// When `response.output_item.done` arrives for a `reasoning` item,
    /// the server-authoritative summary text replaces whatever streamed
    /// via deltas. This matters because some servers emit the final
    /// canonical text only in the done item.
    #[test]
    fn output_item_done_reasoning_uses_summary_text() {
        let mut state = ResponsesParseState::default();
        let mut output = empty_assistant_message();

        let _ = dispatch_responses_event(
            &mut state,
            &mut output,
            "response.reasoning_text.delta",
            &json!({ "delta": "partial..." }),
        );
        let _ = dispatch_responses_event(
            &mut state,
            &mut output,
            "response.output_item.done",
            &json!({
                "item": {
                    "type": "reasoning",
                    "summary": [{ "text": "final summary" }],
                }
            }),
        );
        finalize_responses_output(state, &mut output);

        let thinking = output
            .content
            .iter()
            .find_map(|c| match c {
                AssistantContentBlock::Thinking(t) => Some(t.thinking.clone()),
                _ => None,
            })
            .expect("thinking block missing");
        assert_eq!(thinking, "final summary");
    }

    /// When `summary` is absent on the done item, fall back to
    /// `content[].text`. Some providers (LM Studio variants) deliver the
    /// canonical reasoning body only in `content`.
    #[test]
    fn output_item_done_reasoning_falls_back_to_content_text() {
        let mut state = ResponsesParseState::default();
        let mut output = empty_assistant_message();

        let _ = dispatch_responses_event(
            &mut state,
            &mut output,
            "response.output_item.done",
            &json!({
                "item": {
                    "type": "reasoning",
                    "content": [
                        { "text": "part a" },
                        { "text": "part b" }
                    ],
                }
            }),
        );
        finalize_responses_output(state, &mut output);

        let thinking = output
            .content
            .iter()
            .find_map(|c| match c {
                AssistantContentBlock::Thinking(t) => Some(t.thinking.clone()),
                _ => None,
            })
            .expect("thinking block missing");
        assert_eq!(thinking, "part a\n\npart b");
    }

    /// When neither `summary` nor `content` is present on a reasoning done
    /// item, the streamed delta accumulator must be preserved (no clobber).
    #[test]
    fn output_item_done_reasoning_preserves_streamed_thinking() {
        let mut state = ResponsesParseState::default();
        let mut output = empty_assistant_message();

        let _ = dispatch_responses_event(
            &mut state,
            &mut output,
            "response.reasoning_text.delta",
            &json!({ "delta": "kept" }),
        );
        let _ = dispatch_responses_event(
            &mut state,
            &mut output,
            "response.output_item.done",
            &json!({ "item": { "type": "reasoning" } }),
        );
        finalize_responses_output(state, &mut output);

        let thinking = output
            .content
            .iter()
            .find_map(|c| match c {
                AssistantContentBlock::Thinking(t) => Some(t.thinking.clone()),
                _ => None,
            })
            .expect("thinking block missing");
        assert_eq!(thinking, "kept");
    }
}
