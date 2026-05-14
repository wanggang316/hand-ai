//! Shared helpers for the OpenAI Responses API used by both
//! `openai_responses` and `azure_openai_responses` providers.
//!
//! The wire format (request body shape, SSE event names, output-item parsing)
//! is identical across the two endpoints — only the URL construction and
//! authentication header differ. Those provider-specific concerns stay in
//! each provider module; everything below is the common payload and stream
//! decoder.

use crate::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, CacheRetention, Compat,
    Context, Message, Model, StopReason, StreamOptions, TextContent, ThinkingContent, ToolCall,
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
        // Default to `store: false` so OpenAI doesn't keep a
        // server-side copy of the request / response. Callers that
        // want history retention (replay, audit, ...) can re-enable
        // it on the model.compat side. The Codex Responses variant
        // also overwrites this to false explicitly because that
        // backend rejects `store: true`.
        "store": false,
    });

    body["input"] = convert_to_input_for_model(context, model);

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

    // Prompt caching:
    // - `prompt_cache_key` lets the upstream key its cache to a specific
    //   client session. Emit it whenever caching is requested AND the
    //   caller passed a session id.
    // - `prompt_cache_retention` opts in to a longer-lived cache.
    //   "24h" is only valid on endpoints whose compat block says
    //   `supportsLongCacheRetention: true` (defaults to true for direct
    //   OpenAI; models.dev sets it to false for proxies that reject the
    //   field).
    let retention = CacheRetention::resolve(options.cache_retention);
    if retention != CacheRetention::None
        && let Some(session_id) = options.session_id.as_deref()
        && !session_id.is_empty()
    {
        body["prompt_cache_key"] = Value::String(session_id.to_string());
    }
    if retention == CacheRetention::Long && responses_supports_long_cache_retention(model) {
        body["prompt_cache_retention"] = Value::String("24h".to_string());
    }

    body
}

/// Whether the model's OpenAI Responses compat block opts in to the
/// long (24h) prompt-cache retention. Default is `true` — only
/// proxies that explicitly reject the `prompt_cache_retention` field
/// disable it via models.dev compat metadata.
fn responses_supports_long_cache_retention(model: &Model) -> bool {
    if let Some(Compat::OpenAIResponses(c)) = model.compat.as_ref()
        && let Some(v) = c.supports_long_cache_retention
    {
        return v;
    }
    true
}

/// Convert a `Context` into the `input` array accepted by the Responses API.
///
/// Kept as the legacy single-arg surface so tests that don't care
/// about vision routing don't have to thread a model through. New
/// callers should prefer `convert_to_input_for_model`, which forwards
/// tool-result image blocks when the target model advertises image
/// input support.
pub(crate) fn convert_to_input(context: &Context) -> Value {
    convert_to_input_for_model_inner(context, false)
}

/// Like [`convert_to_input`] but routes tool-result images into the
/// `function_call_output` content array when the model accepts image
/// input. This lets vision-capable Responses models see screenshots,
/// PDFs, and other binary tool-output payloads inline.
pub(crate) fn convert_to_input_for_model(context: &Context, model: &Model) -> Value {
    let supports_images = model.input.contains(&crate::types::InputType::Image);
    convert_to_input_for_model_inner(context, supports_images)
}

fn convert_to_input_for_model_inner(context: &Context, supports_images: bool) -> Value {
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
                                "call_id": normalize_responses_tool_id(&tc.id),
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
                let text_result = tr
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        crate::types::ToolResultContent::Text(t) => Some(t.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let has_text = !text_result.is_empty();
                let has_images = tr
                    .content
                    .iter()
                    .any(|c| matches!(c, crate::types::ToolResultContent::Image(_)));

                let output_value = if has_images && supports_images {
                    // Vision-capable model: ship images inline as a
                    // `function_call_output` content list. Mirrors
                    // the OpenAI Responses
                    // `ResponseFunctionCallOutputItemList` shape:
                    // input_text and input_image parts side by side.
                    let mut parts: Vec<Value> = Vec::new();
                    if has_text {
                        parts.push(serde_json::json!({
                            "type": "input_text",
                            "text": text_result,
                        }));
                    }
                    for c in &tr.content {
                        if let crate::types::ToolResultContent::Image(img) = c {
                            parts.push(serde_json::json!({
                                "type": "input_image",
                                "detail": "auto",
                                "image_url": format!(
                                    "data:{};base64,{}",
                                    img.mime_type, img.data,
                                ),
                            }));
                        }
                    }
                    Value::Array(parts)
                } else if has_text {
                    Value::String(text_result)
                } else if has_images {
                    // Non-vision model with image-only tool result:
                    // emit a placeholder string so the function_call
                    // still gets matched. The transform layer should
                    // ideally have downgraded this earlier but the
                    // placeholder avoids sending a confusing empty
                    // function_call_output that the upstream rejects.
                    Value::String("(see attached image)".to_string())
                } else {
                    Value::String(String::new())
                };

                input.push(serde_json::json!({
                    "type": "function_call_output",
                    "call_id": normalize_responses_tool_id(&tr.tool_call_id),
                    "output": output_value,
                }));
            }
        }
    }

    Value::Array(input)
}

/// Normalize a tool-call id for replay through the Responses API.
///
/// OpenAI Responses requires `call_id` to be alphanumeric (plus `_`
/// and `-`), 64 chars or fewer, with no trailing underscore. Foreign
/// providers (Anthropic `toolu_01...`, Google function-call ids that
/// can carry `:` or `.`, etc.) sometimes produce ids that violate
/// the spec. Sanitize once and truncate, then strip trailing
/// underscores left over from truncation or character replacement.
pub(crate) fn normalize_responses_tool_id(id: &str) -> String {
    let sanitized: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let truncated = if sanitized.len() > 64 {
        sanitized[..64].to_string()
    } else {
        sanitized
    };
    truncated.trim_end_matches('_').to_string()
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

        // Some Responses servers skip the `.delta` events entirely and
        // ship the full arguments payload only in `.done`. Without this
        // branch the accumulator stays empty and `output_item.done`
        // parses the tool call as `{}`, silently dropping every arg.
        // When the server DID stream deltas first, treat `.done` as the
        // authoritative final value — it covers transports that send a
        // condensed/cleaned-up form after the partial stream.
        "response.function_call_arguments.done" => {
            if let Some(arguments) = data.get("arguments").and_then(|a| a.as_str()) {
                state.current_tool_args = arguments.to_string();
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

        // `response.created` is the first event in a stream and
        // carries the upstream's stable response identifier
        // (`resp_...`). Capture it eagerly so callers can correlate
        // the turn even when the stream is aborted before
        // `response.completed`.
        "response.created" => {
            if let Some(rid) = data
                .get("response")
                .and_then(|r| r.get("id"))
                .and_then(|v| v.as_str())
                && !rid.is_empty()
            {
                output.response_id = Some(rid.to_string());
            }
        }

        "response.completed" => {
            if let Some(response) = data.get("response") {
                // Re-capture the id on completion in case
                // `response.created` was missed (some proxies skip
                // the open event when they replay a cached response).
                if let Some(rid) = response.get("id").and_then(|v| v.as_str())
                    && !rid.is_empty()
                {
                    output.response_id = Some(rid.to_string());
                }
                if let Some(usage) = response.get("usage") {
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
        }

        // `response.failed` is the upstream's signal that the
        // response could not be produced — e.g. content moderation,
        // upstream model errors. The event carries the actual cause
        // in `response.error` (code + message) or
        // `response.incomplete_details.reason`. Surface as an Error
        // event with the cleaned-up reason so callers see WHY the
        // turn failed instead of a generic "unknown error".
        //
        // The previous behaviour silently ignored the event and let
        // the stream finish, producing a successful-looking Done
        // with empty content.
        "response.failed" => {
            let response = data.get("response");
            let msg = response
                .and_then(|r| r.get("error"))
                .map(|err| {
                    let code = err
                        .get("code")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    let message = err
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("no message");
                    format!("{code}: {message}")
                })
                .or_else(|| {
                    response
                        .and_then(|r| r.get("incomplete_details"))
                        .and_then(|d| d.get("reason"))
                        .and_then(|v| v.as_str())
                        .map(|reason| format!("incomplete: {reason}"))
                })
                .unwrap_or_else(|| {
                    "response.failed event without error details".to_string()
                });
            output.stop_reason = StopReason::Error;
            output.error_message = Some(msg);
            emitted.push(AssistantMessageEvent::Error {
                reason: StopReason::Error,
                error: output.clone(),
            });
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

    /// `response.created` carries the upstream's stable response id
    /// (`resp_...`). Capture it eagerly on the first event so callers
    /// can correlate the turn even when the stream is aborted before
    /// `response.completed`.
    #[test]
    fn response_created_captures_response_id() {
        let output = run_events(&[(
            "response.created",
            json!({ "response": { "id": "resp_abc123" } }),
        )]);
        assert_eq!(output.response_id.as_deref(), Some("resp_abc123"));
    }

    /// Some proxies skip `response.created` (especially when replaying
    /// a cached response). `response.completed` must re-capture the id
    /// in that case so the assistant message still carries a value.
    #[test]
    fn response_completed_captures_response_id_when_created_missed() {
        let output = run_events(&[(
            "response.completed",
            json!({
                "response": {
                    "id": "resp_xyz789",
                    "usage": { "input_tokens": 5, "output_tokens": 7 }
                }
            }),
        )]);
        assert_eq!(output.response_id.as_deref(), Some("resp_xyz789"));
        assert_eq!(output.usage.input, 5);
        assert_eq!(output.usage.output, 7);
    }

    /// When both events arrive, the `created` value wins — it lands
    /// first and a later `completed` with the same id should be a
    /// no-op (in practice the upstream sends matching ids).
    #[test]
    fn response_created_value_survives_completed() {
        let output = run_events(&[
            ("response.created", json!({ "response": { "id": "resp_first" } })),
            (
                "response.completed",
                json!({ "response": { "id": "resp_first" } }),
            ),
        ]);
        assert_eq!(output.response_id.as_deref(), Some("resp_first"));
    }

    /// Empty / missing ids must not produce `Some("")` — downstream
    /// observability uses presence of the field to decide whether
    /// to log a correlation hint.
    #[test]
    fn response_created_skips_empty_id() {
        let output = run_events(&[(
            "response.created",
            json!({ "response": { "id": "" } }),
        )]);
        assert!(output.response_id.is_none());
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

    /// Some Responses servers ship the entire function-call arguments
    /// payload only in `response.function_call_arguments.done` and skip
    /// the streaming `.delta` events. Before, the args accumulator stayed
    /// empty, `output_item.done` parsed to `{}`, and every tool argument
    /// disappeared. Pin the new behavior: `.done` populates the
    /// accumulator so the subsequent `output_item.done` sees the real
    /// arguments.
    #[test]
    fn function_call_arguments_done_populates_empty_accumulator() {
        let output = run_events(&[
            (
                "response.output_item.added",
                json!({
                    "item": {
                        "type": "function_call",
                        "name": "lookup",
                        "call_id": "call-1"
                    }
                }),
            ),
            (
                "response.function_call_arguments.done",
                json!({ "arguments": "{\"q\":\"hello\"}" }),
            ),
            (
                "response.output_item.done",
                json!({
                    "item": {
                        "type": "function_call",
                        "name": "lookup",
                        "call_id": "call-1",
                        "arguments": "{\"q\":\"hello\"}"
                    }
                }),
            ),
        ]);
        let tc = output
            .content
            .iter()
            .find_map(|c| match c {
                AssistantContentBlock::ToolCall(tc) => Some(tc),
                _ => None,
            })
            .expect("tool call missing");
        assert_eq!(tc.name, "lookup");
        assert_eq!(tc.arguments, json!({"q": "hello"}));
    }

    /// When deltas DID arrive first, `.done` is treated as the
    /// authoritative final value (some transports clean up trailing
    /// whitespace or condense the payload between the partial stream and
    /// `.done`). The tool call carries the `.done` view, not the
    /// concatenated delta stream.
    #[test]
    fn function_call_arguments_done_overrides_streamed_deltas() {
        let output = run_events(&[
            (
                "response.output_item.added",
                json!({
                    "item": {
                        "type": "function_call",
                        "name": "lookup",
                        "call_id": "call-2"
                    }
                }),
            ),
            (
                "response.function_call_arguments.delta",
                json!({ "delta": "{\"q" }),
            ),
            (
                "response.function_call_arguments.delta",
                json!({ "delta": "\":\"par" }),
            ),
            (
                "response.function_call_arguments.done",
                json!({ "arguments": "{\"q\":\"partial\"}" }),
            ),
            (
                "response.output_item.done",
                json!({
                    "item": {
                        "type": "function_call",
                        "name": "lookup",
                        "call_id": "call-2",
                        "arguments": "{\"q\":\"partial\"}"
                    }
                }),
            ),
        ]);
        let tc = output
            .content
            .iter()
            .find_map(|c| match c {
                AssistantContentBlock::ToolCall(tc) => Some(tc),
                _ => None,
            })
            .expect("tool call missing");
        assert_eq!(tc.arguments, json!({"q": "partial"}));
    }

    fn responses_test_model() -> Model {
        use crate::types::{Cost, InputType};
        Model {
            id: "gpt-5".to_string(),
            name: "gpt-5".to_string(),
            api: Api::OpenAIResponses,
            provider: Provider::OpenAI,
            base_url: String::new(),
            reasoning: false,
            input: vec![InputType::Text],
            cost: Cost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_window: 0,
            max_tokens: 0,
            headers: None,
            compat: None,
            thinking_level_map: None,
        }
    }

    fn responses_test_context() -> Context {
        use crate::types::{Message, UserMessage};
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new_text("hi"))],
            tools: None,
        }
    }

    fn vision_responses_model() -> Model {
        use crate::types::{Api, Cost, InputType, Provider};
        Model {
            id: "gpt-5".to_string(),
            name: "gpt-5".to_string(),
            api: Api::OpenAIResponses,
            provider: Provider::OpenAI,
            base_url: String::new(),
            reasoning: false,
            input: vec![InputType::Text, InputType::Image],
            cost: Cost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_window: 0,
            max_tokens: 0,
            headers: None,
            compat: None,
            thinking_level_map: None,
        }
    }

    fn tool_result_with_image(text: &str, mime: &str, data: &str) -> Context {
        use crate::types::{
            ImageContent, Message, TextContent, ToolResultContent, ToolResultMessage,
        };
        let mut content: Vec<ToolResultContent> = Vec::new();
        if !text.is_empty() {
            content.push(ToolResultContent::Text(TextContent::new(text)));
        }
        content.push(ToolResultContent::Image(ImageContent {
            content_type: "image".to_string(),
            mime_type: mime.to_string(),
            data: data.to_string(),
        }));
        Context {
            system_prompt: None,
            messages: vec![Message::ToolResult(ToolResultMessage {
                role: "toolResult".to_string(),
                tool_call_id: "call_abc".to_string(),
                tool_name: "screenshot".to_string(),
                content,
                details: None,
                is_error: false,
                timestamp: 0,
            })],
            tools: None,
        }
    }

    /// On vision-capable Responses models, tool-result images must
    /// land in the `function_call_output` content list as
    /// `input_image` parts alongside any `input_text` part for the
    /// text portion. Sending only the text would drop the screenshot
    /// the agent needs to reason over.
    #[test]
    fn tool_result_images_inline_for_vision_capable_model() {
        let model = vision_responses_model();
        let context = tool_result_with_image("found 3 items", "image/png", "AAA");
        let input = convert_to_input_for_model(&context, &model);
        let arr = input.as_array().expect("array");
        assert_eq!(arr.len(), 1);
        let out = &arr[0];
        assert_eq!(out["type"], "function_call_output");
        let parts = out["output"].as_array().expect("output is content list");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["type"], "input_text");
        assert_eq!(parts[0]["text"], "found 3 items");
        assert_eq!(parts[1]["type"], "input_image");
        assert_eq!(parts[1]["detail"], "auto");
        assert_eq!(parts[1]["image_url"], "data:image/png;base64,AAA");
    }

    /// Image-only tool result on a vision model: emit just the
    /// `input_image` part — no empty `input_text`.
    #[test]
    fn tool_result_image_only_skips_input_text_part() {
        let model = vision_responses_model();
        let context = tool_result_with_image("", "image/jpeg", "ZZZ");
        let input = convert_to_input_for_model(&context, &model);
        let parts = input.as_array().unwrap()[0]["output"].as_array().unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["type"], "input_image");
    }

    /// Non-vision Responses models keep the legacy plain-string
    /// output shape; image content gets dropped into the placeholder.
    #[test]
    fn tool_result_images_collapse_to_placeholder_on_non_vision_model() {
        let mut model = vision_responses_model();
        model.input = vec![crate::types::InputType::Text];
        let context = tool_result_with_image("", "image/png", "AAA");
        let input = convert_to_input_for_model(&context, &model);
        let out = &input.as_array().unwrap()[0]["output"];
        assert!(
            out.is_string(),
            "non-vision model must emit plain string output: {out}"
        );
        assert_eq!(out.as_str(), Some("(see attached image)"));
    }

    /// Text-only tool result keeps the plain-string shape even when
    /// the model is vision-capable — there's no image to inline.
    #[test]
    fn tool_result_text_only_keeps_plain_string_shape() {
        use crate::types::{Message, TextContent, ToolResultContent, ToolResultMessage};
        let context = Context {
            system_prompt: None,
            messages: vec![Message::ToolResult(ToolResultMessage {
                role: "toolResult".to_string(),
                tool_call_id: "call_xyz".to_string(),
                tool_name: "read".to_string(),
                content: vec![ToolResultContent::Text(TextContent::new("hello"))],
                details: None,
                is_error: false,
                timestamp: 0,
            })],
            tools: None,
        };
        let input = convert_to_input_for_model(&context, &vision_responses_model());
        let out = &input.as_array().unwrap()[0]["output"];
        assert_eq!(out.as_str(), Some("hello"));
    }

    /// The Responses API surfaces a `response.failed` SSE event when
    /// it cannot produce a response (content moderation, upstream
    /// model errors, ...). The event body carries the actual cause
    /// in `response.error` — silently dropping the event made the
    /// stream look successful even though the turn failed. Pin the
    /// Error event so callers see WHY the turn failed.
    #[test]
    fn response_failed_event_emits_error_with_code_and_message() {
        let mut state = ResponsesParseState::default();
        let mut output = empty_assistant_message();
        let events = dispatch_responses_event(
            &mut state,
            &mut output,
            "response.failed",
            &json!({
                "response": {
                    "error": {
                        "code": "content_filter",
                        "message": "Request flagged by safety policy."
                    }
                }
            }),
        );
        assert_eq!(events.len(), 1);
        match &events[0] {
            AssistantMessageEvent::Error { error, .. } => {
                let msg = error.error_message.as_deref().unwrap_or_default();
                assert!(
                    msg.contains("content_filter") && msg.contains("safety policy"),
                    "error msg must carry code and message: {msg}"
                );
            }
            other => panic!("expected Error event, got {other:?}"),
        }
        assert_eq!(output.stop_reason, StopReason::Error);
    }

    /// Some failures report only `incomplete_details.reason` without
    /// an `error` object. The decoder must still surface a meaningful
    /// message instead of a generic "unknown error".
    #[test]
    fn response_failed_event_falls_back_to_incomplete_details() {
        let mut state = ResponsesParseState::default();
        let mut output = empty_assistant_message();
        let events = dispatch_responses_event(
            &mut state,
            &mut output,
            "response.failed",
            &json!({
                "response": {
                    "incomplete_details": { "reason": "max_output_tokens" }
                }
            }),
        );
        assert_eq!(events.len(), 1);
        match &events[0] {
            AssistantMessageEvent::Error { error, .. } => {
                let msg = error.error_message.as_deref().unwrap_or_default();
                assert!(
                    msg.contains("incomplete") && msg.contains("max_output_tokens"),
                    "fallback message must carry the incomplete reason: {msg}"
                );
            }
            other => panic!("expected Error event, got {other:?}"),
        }
    }

    /// OpenAI's Responses API stores the request / response on the
    /// server unless the caller sets `store: false`. Pinning it off
    /// by default avoids surprising server-side history logging.
    #[test]
    fn build_request_body_pins_store_false_by_default() {
        let body = build_request_body(
            &responses_test_model(),
            &responses_test_context(),
            &StreamOptions::default(),
        );
        assert_eq!(
            body["store"], serde_json::Value::Bool(false),
            "store must default to false: {body}"
        );
    }

    /// `call_id` on a Responses request must be alphanumeric, `_`,
    /// or `-`, max 64 chars, with no trailing underscore. Foreign
    /// providers can produce ids with `:`, `.`, `|`, or other
    /// punctuation that the upstream rejects. Sanitize them so a
    /// cross-provider replay (Anthropic → OpenAI Responses) keeps
    /// flowing instead of failing the second turn.
    #[test]
    fn normalize_tool_id_replaces_disallowed_characters() {
        assert_eq!(
            normalize_responses_tool_id("toolu_01:abc.def|123"),
            "toolu_01_abc_def_123",
        );
        assert_eq!(normalize_responses_tool_id("call_a/b@c#"), "call_a_b_c",);
    }

    /// Ids longer than 64 chars get truncated.
    #[test]
    fn normalize_tool_id_truncates_to_64_chars() {
        let long_id = "a".repeat(120);
        let out = normalize_responses_tool_id(&long_id);
        assert_eq!(out.len(), 64);
        assert!(out.chars().all(|c| c == 'a'));
    }

    /// Trailing underscores from truncation or replacement are
    /// stripped — OpenAI Codex rejects them.
    #[test]
    fn normalize_tool_id_strips_trailing_underscores() {
        assert_eq!(normalize_responses_tool_id("call_abc___"), "call_abc");
        assert_eq!(normalize_responses_tool_id("call___"), "call");
        // Truncate-then-strip: after truncating to 64 chars the tail
        // may be all underscores from the replacement step.
        let mut s = "good_id".to_string();
        s.push_str(&"!".repeat(60));
        let out = normalize_responses_tool_id(&s);
        assert!(!out.ends_with('_'), "must not end with underscore: {out}");
    }

    /// Direct OpenAI Responses calls emit `prompt_cache_key` when the
    /// caller supplied a session id and caching wasn't explicitly
    /// disabled. The key lets the upstream pin its cache to a single
    /// client session so cache hits stay deterministic.
    #[test]
    fn build_request_body_emits_prompt_cache_key_for_session() {
        let mut options = StreamOptions::default();
        options.session_id = Some("sess-abc".to_string());
        let body = build_request_body(&responses_test_model(), &responses_test_context(), &options);
        assert_eq!(body["prompt_cache_key"].as_str(), Some("sess-abc"));
    }

    /// When the caller explicitly opts out of caching via
    /// `cache_retention: none`, the key must be omitted regardless of
    /// whether a session id was supplied.
    #[test]
    fn build_request_body_omits_prompt_cache_key_when_caching_disabled() {
        let mut options = StreamOptions::default();
        options.session_id = Some("sess-abc".to_string());
        options.cache_retention = Some(CacheRetention::None);
        let body = build_request_body(&responses_test_model(), &responses_test_context(), &options);
        assert!(body.get("prompt_cache_key").is_none(), "body: {body}");
        assert!(body.get("prompt_cache_retention").is_none(), "body: {body}");
    }

    /// `cache_retention: long` opts in to the 24-hour cache window on
    /// endpoints that accept it. Default-compat models support it; the
    /// builder emits `prompt_cache_retention: "24h"`.
    #[test]
    fn build_request_body_emits_24h_retention_for_long_cache() {
        let mut options = StreamOptions::default();
        options.session_id = Some("sess-long".to_string());
        options.cache_retention = Some(CacheRetention::Long);
        let body = build_request_body(&responses_test_model(), &responses_test_context(), &options);
        assert_eq!(body["prompt_cache_retention"].as_str(), Some("24h"));
    }

    /// Some proxies reject `prompt_cache_retention` entirely. Models
    /// served by such proxies opt out via
    /// `OpenAIResponsesCompat.supportsLongCacheRetention = false` —
    /// the builder must honour that flag and omit the field.
    #[test]
    fn build_request_body_omits_24h_retention_when_compat_opts_out() {
        use crate::types::OpenAIResponsesCompat;
        let mut model = responses_test_model();
        model.compat = Some(Compat::OpenAIResponses(OpenAIResponsesCompat {
            send_session_id_header: None,
            supports_long_cache_retention: Some(false),
        }));
        let mut options = StreamOptions::default();
        options.session_id = Some("sess-long".to_string());
        options.cache_retention = Some(CacheRetention::Long);
        let body = build_request_body(&model, &responses_test_context(), &options);
        // Key still emits (short-cache equivalent) but the 24h
        // retention does not.
        assert_eq!(body["prompt_cache_key"].as_str(), Some("sess-long"));
        assert!(
            body.get("prompt_cache_retention").is_none(),
            "compat-opt-out must drop retention field, got: {body}"
        );
    }

    /// `cache_retention: short` (the default) only requests the key;
    /// the 24h retention is opt-in.
    #[test]
    fn build_request_body_omits_24h_retention_for_short_cache() {
        let mut options = StreamOptions::default();
        options.session_id = Some("sess-short".to_string());
        options.cache_retention = Some(CacheRetention::Short);
        let body = build_request_body(&responses_test_model(), &responses_test_context(), &options);
        assert_eq!(body["prompt_cache_key"].as_str(), Some("sess-short"));
        assert!(body.get("prompt_cache_retention").is_none());
    }

    /// No session id means no prompt_cache_key — the field is only
    /// useful if the upstream can group calls by client session.
    #[test]
    fn build_request_body_omits_prompt_cache_key_without_session() {
        let body = build_request_body(
            &responses_test_model(),
            &responses_test_context(),
            &StreamOptions::default(),
        );
        assert!(body.get("prompt_cache_key").is_none());
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
