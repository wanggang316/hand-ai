//! Shared helpers for the Google Generative AI / Gemini wire format used by
//! both `google_generative_ai` and `google_vertex` providers.
//!
//! The wire format (request body shape, SSE event names, candidate/part
//! parsing, thought-signature handling) is identical across the two
//! endpoints — only the URL construction and authentication strategy differ.
//! Those provider-specific concerns stay in each provider module; everything
//! below is the common payload and stream decoder.

use crate::calculate_cost;
use crate::types::{
    Api, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Context, InputType,
    Message, Model, StopReason, StreamOptions, TextContent, ThinkingContent, ThinkingLevel,
    ToolCall, ToolResultContent, Usage, UserContentBlock,
};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};

/// Counter for generating unique tool call IDs across both providers.
static TOOL_CALL_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Thinking level strings shared by both Google providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GoogleThinkingLevel {
    Minimal,
    Low,
    Medium,
    High,
}

impl GoogleThinkingLevel {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            GoogleThinkingLevel::Minimal => "MINIMAL",
            GoogleThinkingLevel::Low => "LOW",
            GoogleThinkingLevel::Medium => "MEDIUM",
            GoogleThinkingLevel::High => "HIGH",
        }
    }
}

/// Subset of options consumed by the shared request-body builder. The two
/// providers share these knobs even though they expose different public
/// option types.
#[derive(Debug, Clone, Default)]
pub(crate) struct SharedGoogleOptions {
    pub base: StreamOptions,
    pub tool_choice: Option<String>,
    pub thinking_enabled: bool,
    pub thinking_budget_tokens: Option<i32>,
    pub thinking_level: Option<GoogleThinkingLevel>,
}

// =============================================================================
// Request Building
// =============================================================================

pub(crate) fn build_request_body(
    model: &Model,
    context: &Context,
    options: &SharedGoogleOptions,
) -> Result<Value, String> {
    let mut body = serde_json::Map::new();

    let contents = convert_messages(&context.messages, model);
    body.insert("contents".to_string(), Value::Array(contents));

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

    if let Some(tools) = &context.tools
        && !tools.is_empty()
    {
        let tool_defs = convert_tools(tools);
        body.insert("tools".to_string(), Value::Array(vec![tool_defs]));
    }

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

        let gen_config = body
            .entry("generationConfig")
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        if let Value::Object(gc) = gen_config {
            gc.insert("thinkingConfig".to_string(), Value::Object(thinking_config));
        }
    } else if model.reasoning && !options.thinking_enabled {
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

pub(crate) fn get_disabled_thinking_config(model_id: &str) -> Value {
    if is_gemini3_pro_model(model_id) {
        serde_json::json!({"thinkingLevel": "LOW"})
    } else if is_gemini3_flash_model(model_id) {
        serde_json::json!({"thinkingLevel": "MINIMAL"})
    } else if is_gemma4_model(model_id) {
        // Gemma 4 mirrors the Gemini-3 surface and exposes the same
        // `thinkingLevel` knob, but it only supports MINIMAL / HIGH.
        // "Disabled" maps to MINIMAL — the smallest knob that still
        // accepts the request shape.
        serde_json::json!({"thinkingLevel": "MINIMAL"})
    } else if is_gemini25_pro_model(model_id) {
        // gemini-2.5-pro is thinking-only — `thinkingBudget: 0` is
        // rejected with "Budget 0 is invalid. This model only works in
        // thinking mode." Fall back to `-1` (dynamic) so Google picks a
        // sensible budget, which matches the API's own default.
        serde_json::json!({"thinkingBudget": -1})
    } else {
        serde_json::json!({"thinkingBudget": 0})
    }
}

/// Match `gemini-2.5-pro` (and any future minor variant — `2.5-pro-001`,
/// `2.5-pro-preview`, …). Used to skip `thinkingBudget: 0` for the
/// thinking-only Pro family.
pub(crate) fn is_gemini25_pro_model(model_id: &str) -> bool {
    let lower = model_id.to_lowercase();
    lower.contains("2.5-pro") || lower.contains("2-5-pro")
}

/// Gemma 4 models accept the same `thinkingLevel` knob as Gemini-3
/// but only expose two settings: `MINIMAL` and `HIGH`. Matched on a
/// case-insensitive prefix so both `gemma-4` and `gemma4` forms map
/// onto the same branch.
pub(crate) fn is_gemma4_model(model_id: &str) -> bool {
    let lower = model_id.to_lowercase();
    lower.starts_with("gemma-4") || lower.starts_with("gemma4")
}

// =============================================================================
// Message Conversion
// =============================================================================

pub(crate) fn convert_messages(messages: &[Message], model: &Model) -> Vec<Value> {
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
                                parts.push(
                                    serde_json::json!({"text": sanitize_surrogates(&t.thinking)}),
                                );
                            }
                        }
                        AssistantContentBlock::ToolCall(tc) => {
                            // Gemini rejects `args: null` on function calls;
                            // default Null arguments to an empty object so
                            // argless tool-call history replays cleanly.
                            let args = if tc.arguments.is_null() {
                                serde_json::Value::Object(serde_json::Map::new())
                            } else {
                                tc.arguments.clone()
                            };
                            let mut fc = serde_json::json!({
                                "name": tc.name,
                                "args": args,
                            });
                            if requires_tool_call_id(&model.id) {
                                fc.as_object_mut()
                                    .unwrap()
                                    .insert("id".to_string(), Value::String(tc.id.clone()));
                            }

                            let mut part = serde_json::json!({"functionCall": fc});

                            if is_same_provider_and_model
                                && let Some(sig) = &tc.thought_signature
                                && is_valid_thought_signature(sig)
                            {
                                part.as_object_mut().unwrap().insert(
                                    "thoughtSignature".to_string(),
                                    Value::String(sig.clone()),
                                );
                            }
                            // Gemini-3 previously took a `skip_thought_signature_validator`
                            // sentinel for unsigned tool calls. Vertex now rejects the
                            // sentinel; omit `thoughtSignature` entirely for unsigned
                            // replays and let the validator skip the check on its own.
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

pub(crate) fn convert_tools(tools: &[crate::types::Tool]) -> Value {
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

pub(crate) async fn parse_sse_stream(
    response: reqwest::Response,
    model: &Model,
    api: Api,
) -> Result<Vec<AssistantMessageEvent>, String> {
    let mut events = Vec::new();

    let mut output = AssistantMessage {
        role: "assistant".to_string(),
        content: vec![],
        api,
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

    // Note: `Start` is emitted by the outer stream wrappers in each provider
    // (`google_generative_ai`, `google_vertex`) so the
    // `Start -> ... -> Done|Error` shape holds even on early failures (e.g.
    // network errors that never open the SSE channel). Emitting it here
    // would duplicate the event on the happy path.

    let body = response
        .text()
        .await
        .map_err(|e| format!("Failed to read response: {}", e))?;

    let mut current_block_type: Option<&str> = None; // "text" or "thinking"

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

        capture_chunk_response_id(&chunk, &mut output.response_id);

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
                        let is_thinking = is_thinking_part(part);

                        let block_type = if is_thinking { "thinking" } else { "text" };

                        if current_block_type != Some(block_type) {
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

                        let idx = (output.content.len() - 1) as u32;
                        if is_thinking {
                            if let Some(AssistantContentBlock::Thinking(t)) =
                                output.content.last_mut()
                            {
                                t.thinking.push_str(text);
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

                    if let Some(fc) = part.get("functionCall") {
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

        if let Some(usage) = chunk.get("usageMetadata") {
            apply_google_usage_metadata(usage, &mut output.usage);
            calculate_cost(model, &mut output.usage);
        }
    }

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

/// Models reached via Google APIs that require explicit tool call IDs in
/// function calls/responses (e.g. Claude / gpt-oss models hosted on Vertex).
pub(crate) fn requires_tool_call_id(model_id: &str) -> bool {
    model_id.starts_with("claude-") || model_id.starts_with("gpt-oss-")
}

/// Whether a streamed Gemini `Part` should be treated as a thinking
/// block. The `thought: true` flag is the definitive marker; a
/// `thoughtSignature` is an opaque context-replay handle that can
/// appear on *any* part type (text, functionCall, ...) and must NOT
/// be used to reclassify the part — text parts that carry a signature
/// stay as text and persist the signature into `text_signature` for
/// the next turn's replay.
///
/// See: <https://ai.google.dev/gemini-api/docs/thought-signatures>
pub(crate) fn is_thinking_part(part: &Value) -> bool {
    part.get("thought")
        .and_then(|t| t.as_bool())
        .unwrap_or(false)
}

/// Capture the first non-empty `responseId` from a streamed Gemini
/// chunk into `current`. @google/genai documents
/// `GenerateContentResponse.responseId` as an output-only identifier
/// for each response; surfacing it on the assistant message lets
/// downstream observability / replay correlate this turn with
/// Google's logs. Keeping only the first non-empty value matches the
/// upstream `??=` semantics — later chunks may repeat the same id.
pub(crate) fn capture_chunk_response_id(chunk: &Value, current: &mut Option<String>) {
    if current.is_some() {
        return;
    }
    if let Some(rid) = chunk.get("responseId").and_then(|v| v.as_str())
        && !rid.is_empty()
    {
        *current = Some(rid.to_string());
    }
}

pub(crate) fn is_gemini3_pro_model(model_id: &str) -> bool {
    let lower = model_id.to_lowercase();
    if let Some(rest) = lower.strip_prefix("gemini-3") {
        if let Some(after) = rest.strip_prefix('.') {
            after.contains("-pro")
        } else {
            rest.starts_with("-pro")
        }
    } else {
        false
    }
}

pub(crate) fn is_gemini3_flash_model(model_id: &str) -> bool {
    let lower = model_id.to_lowercase();
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

pub(crate) fn get_gemini3_thinking_level(
    effort: ThinkingLevel,
    model_id: &str,
) -> GoogleThinkingLevel {
    if is_gemini3_pro_model(model_id) {
        match effort {
            ThinkingLevel::Minimal | ThinkingLevel::Low => GoogleThinkingLevel::Low,
            ThinkingLevel::Medium
            | ThinkingLevel::High
            | ThinkingLevel::Xhigh
            | ThinkingLevel::Max => GoogleThinkingLevel::High,
        }
    } else if is_gemma4_model(model_id) {
        // Gemma 4 collapses the four-level effort surface onto just
        // MINIMAL / HIGH. Map low and below to MINIMAL, medium and up
        // to HIGH so the four-tier callers still produce a valid value.
        match effort {
            ThinkingLevel::Minimal | ThinkingLevel::Low => GoogleThinkingLevel::Minimal,
            ThinkingLevel::Medium
            | ThinkingLevel::High
            | ThinkingLevel::Xhigh
            | ThinkingLevel::Max => GoogleThinkingLevel::High,
        }
    } else {
        match effort {
            ThinkingLevel::Minimal => GoogleThinkingLevel::Minimal,
            ThinkingLevel::Low => GoogleThinkingLevel::Low,
            ThinkingLevel::Medium => GoogleThinkingLevel::Medium,
            ThinkingLevel::High | ThinkingLevel::Xhigh | ThinkingLevel::Max => {
                GoogleThinkingLevel::High
            }
        }
    }
}

pub(crate) fn get_google_budget(
    model_id: &str,
    effort: ThinkingLevel,
    custom_budgets: Option<&crate::types::ThinkingBudgets>,
) -> i32 {
    let level = match effort {
        ThinkingLevel::Xhigh | ThinkingLevel::Max => ThinkingLevel::High,
        other => other,
    };

    if let Some(budgets) = custom_budgets {
        let budget = match level {
            ThinkingLevel::Minimal => budgets.minimal,
            ThinkingLevel::Low => budgets.low,
            ThinkingLevel::Medium => budgets.medium,
            ThinkingLevel::High | ThinkingLevel::Xhigh | ThinkingLevel::Max => budgets.high,
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
            ThinkingLevel::High | ThinkingLevel::Xhigh | ThinkingLevel::Max => 32768,
        }
    } else if model_id.contains("2.5-flash-lite") {
        // Gemini 2.5 Flash Lite's minimum thinking budget is 512, not
        // 128. The full Flash variant accepts 128 down to ~512 floor.
        // Match this branch BEFORE the more-permissive `2.5-flash`
        // arm so a request for "gemini-2.5-flash-lite" doesn't fall
        // through and submit an invalid 128-token minimal budget.
        match level {
            ThinkingLevel::Minimal => 512,
            ThinkingLevel::Low => 2048,
            ThinkingLevel::Medium => 8192,
            ThinkingLevel::High | ThinkingLevel::Xhigh | ThinkingLevel::Max => 24576,
        }
    } else if model_id.contains("2.5-flash") {
        match level {
            ThinkingLevel::Minimal => 128,
            ThinkingLevel::Low => 2048,
            ThinkingLevel::Medium => 8192,
            ThinkingLevel::High | ThinkingLevel::Xhigh | ThinkingLevel::Max => 24576,
        }
    } else {
        -1
    }
}

/// Apply a Google `usageMetadata` JSON object onto the assistant
/// message's `Usage`. Google reports `promptTokenCount` INCLUDING
/// the `cachedContentTokenCount` cache hits — counting both the
/// raw sum as `input` AND cache_read separately would double-bill
/// the cache portion at the `input` rate. Subtract cache_read so
/// `input` carries only the non-cached prompt tokens.
pub(crate) fn apply_google_usage_metadata(usage_meta: &Value, target: &mut crate::types::Usage) {
    let prompt_total = usage_meta
        .get("promptTokenCount")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let candidates_tokens = usage_meta
        .get("candidatesTokenCount")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let thoughts_tokens = usage_meta
        .get("thoughtsTokenCount")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cache_read = usage_meta
        .get("cachedContentTokenCount")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let total = usage_meta
        .get("totalTokenCount")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    target.input = prompt_total.saturating_sub(cache_read);
    target.output = candidates_tokens + thoughts_tokens;
    target.cache_read = cache_read;
    target.total_tokens = total;
}

pub(crate) fn map_stop_reason(reason: &str) -> StopReason {
    match reason {
        "STOP" => StopReason::Stop,
        "MAX_TOKENS" => StopReason::Length,
        _ => StopReason::Error,
    }
}

/// Check if a thought signature is valid base64.
pub(crate) fn is_valid_thought_signature(sig: &str) -> bool {
    if sig.is_empty() || !sig.len().is_multiple_of(4) {
        return false;
    }
    sig.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
}

/// Sanitize unicode surrogate pairs in text. Rust strings are already valid
/// UTF-8, so this is currently a no-op kept for API compatibility.
pub(crate) fn sanitize_surrogates(text: &str) -> String {
    text.to_string()
}

pub(crate) fn current_timestamp_ms() -> u64 {
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
        Api, AssistantContentBlock, AssistantMessage, Cost, InputType, Message, Provider,
        StopReason, ToolCall, Usage,
    };

    fn gemini3_model(id: &str) -> Model {
        Model {
            id: id.to_string(),
            name: id.to_string(),
            api: Api::GoogleGenerativeAi,
            provider: Provider::Google,
            base_url: "https://example.com".to_string(),
            reasoning: true,
            input: vec![InputType::Text],
            cost: Cost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_window: 0,
            max_tokens: 1000,
            headers: None,
            compat: None,
            thinking_level_map: None,
        }
    }

    fn assistant_with_tool_call(
        model_id: &str,
        provider: Provider,
        signature: Option<&str>,
    ) -> Message {
        let mut tc = ToolCall::new("call-1", "lookup", serde_json::json!({"q": "x"}));
        tc.thought_signature = signature.map(|s| s.to_string());
        Message::Assistant(AssistantMessage {
            role: "assistant".to_string(),
            content: vec![AssistantContentBlock::ToolCall(tc)],
            api: Api::GoogleGenerativeAi,
            provider,
            model: model_id.to_string(),
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        })
    }

    /// Gemini-3 used to receive a `skip_thought_signature_validator`
    /// sentinel for any unsigned tool call so the validator wouldn't
    /// reject the replay. Vertex started rejecting the sentinel itself,
    /// so the upstream now omits `thoughtSignature` instead. Pin the
    /// new behavior: no sentinel on unsigned Gemini-3 tool calls.
    #[test]
    fn gemini3_unsigned_tool_call_drops_signature_field() {
        let model = gemini3_model("gemini-3-pro");
        let msg = assistant_with_tool_call("gemini-3-pro", Provider::Google, None);
        let contents = convert_messages(&[msg], &model);
        assert_eq!(contents.len(), 1, "expected one content entry");
        let parts = contents[0]
            .get("parts")
            .and_then(Value::as_array)
            .expect("parts array");
        assert_eq!(parts.len(), 1, "expected one part");
        let part = &parts[0];
        assert!(
            part.get("functionCall").is_some(),
            "must keep functionCall: {part}"
        );
        assert!(
            part.get("thoughtSignature").is_none(),
            "unsigned Gemini-3 tool call must not carry sentinel signature: {part}"
        );
    }

    /// Gemini rejects `functionCall.args = null` (the field must be an
    /// object). Replay history may carry a Null arguments value when a
    /// previous turn issued an argless tool call. The converter must emit
    /// `{}` in that case so the wire payload stays well-formed.
    #[test]
    fn google_argless_tool_call_defaults_args_to_empty_object() {
        let model = gemini3_model("gemini-2.5-flash");
        // Build an assistant message with a Null arguments value (mimics
        // history serialized from a model that emitted no arguments).
        let mut tc = ToolCall::new("call-x", "now", serde_json::Value::Null);
        tc.thought_signature = None;
        let msg = Message::Assistant(AssistantMessage {
            role: "assistant".to_string(),
            content: vec![AssistantContentBlock::ToolCall(tc)],
            api: Api::GoogleGenerativeAi,
            provider: Provider::Google,
            model: "gemini-2.5-flash".to_string(),
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        });
        let contents = convert_messages(&[msg], &model);
        let fc = contents[0]["parts"][0]
            .get("functionCall")
            .expect("functionCall present");
        let args = fc.get("args").expect("args present");
        assert!(args.is_object(), "args must be an object, got: {args}");
        assert_eq!(args.as_object().unwrap().len(), 0);
    }

    /// A real, valid base64 thought signature from the same provider/
    /// model must still flow through — only the unsigned-replay path
    /// loses the sentinel.
    #[test]
    fn gemini3_signed_tool_call_preserves_signature() {
        let model = gemini3_model("gemini-3-pro");
        let msg = assistant_with_tool_call("gemini-3-pro", Provider::Google, Some("dGVzdA=="));
        let contents = convert_messages(&[msg], &model);
        let part = &contents[0]["parts"][0];
        assert_eq!(
            part.get("thoughtSignature").and_then(Value::as_str),
            Some("dGVzdA==")
        );
    }

    /// Google's `promptTokenCount` already INCLUDES
    /// `cachedContentTokenCount` cache hits. Without subtracting the
    /// cache portion, calculate_cost would charge the same tokens at
    /// BOTH the input rate AND the cache_read rate. Verify the
    /// extracted helper applies the subtraction.
    #[test]
    fn google_usage_metadata_subtracts_cache_from_input() {
        let mut usage = crate::types::Usage::default();
        apply_google_usage_metadata(
            &serde_json::json!({
                "promptTokenCount": 1000,
                "candidatesTokenCount": 200,
                "thoughtsTokenCount": 50,
                "cachedContentTokenCount": 400,
                "totalTokenCount": 1250,
            }),
            &mut usage,
        );
        assert_eq!(usage.input, 600, "input should drop cache portion");
        assert_eq!(usage.cache_read, 400, "cache_read carries the full cache");
        assert_eq!(usage.output, 250, "candidates + thoughts");
        assert_eq!(usage.total_tokens, 1250);
    }

    /// When the upstream omits the cache field, `input` keeps the
    /// full prompt count and `cache_read` stays zero.
    #[test]
    fn google_usage_metadata_no_cache_keeps_full_input() {
        let mut usage = crate::types::Usage::default();
        apply_google_usage_metadata(
            &serde_json::json!({
                "promptTokenCount": 500,
                "candidatesTokenCount": 100,
                "totalTokenCount": 600,
            }),
            &mut usage,
        );
        assert_eq!(usage.input, 500);
        assert_eq!(usage.cache_read, 0);
        assert_eq!(usage.output, 100);
    }

    /// Defensive: if a provider somehow reports
    /// `cachedContentTokenCount` larger than `promptTokenCount`
    /// (mismatched chunks, partial usage), the subtraction must
    /// saturate at zero instead of overflowing.
    #[test]
    fn google_usage_metadata_saturates_when_cache_exceeds_prompt() {
        let mut usage = crate::types::Usage::default();
        apply_google_usage_metadata(
            &serde_json::json!({
                "promptTokenCount": 100,
                "cachedContentTokenCount": 999,
                "candidatesTokenCount": 0,
                "totalTokenCount": 999,
            }),
            &mut usage,
        );
        assert_eq!(usage.input, 0, "saturating_sub must not panic");
        assert_eq!(usage.cache_read, 999);
    }

    /// Gemini 2.5 Flash Lite's minimum thinking budget is 512, not
    /// 128. The full Flash variant accepts 128. Without a dedicated
    /// branch the more permissive `2.5-flash` arm captured Flash Lite
    /// too and submitted an invalid 128-token minimal budget — the
    /// upstream rejected it with "thinking budget 128 is invalid".
    #[test]
    fn google_budget_flash_lite_minimal_is_512_not_128() {
        assert_eq!(
            get_google_budget("gemini-2.5-flash-lite", ThinkingLevel::Minimal, None),
            512,
            "flash-lite minimal must be 512 (not 128) to clear the upstream's 512 floor"
        );
        // Sanity-check the other Flash Lite levels match the upstream
        // table: 2048 / 8192 / 24576.
        assert_eq!(
            get_google_budget("gemini-2.5-flash-lite", ThinkingLevel::Low, None),
            2048
        );
        assert_eq!(
            get_google_budget("gemini-2.5-flash-lite", ThinkingLevel::Medium, None),
            8192
        );
        assert_eq!(
            get_google_budget("gemini-2.5-flash-lite", ThinkingLevel::High, None),
            24576
        );
    }

    /// gemini-2.5-pro is a thinking-only model. The Google API
    /// rejects `thinkingBudget: 0` with "Budget 0 is invalid. This
    /// model only works in thinking mode." When the user runs without
    /// an explicit thinking level, the disabled-thinking config must
    /// not emit `0` for this family — fall back to `-1` (dynamic) so
    /// Google picks a sensible default and the call still succeeds.
    #[test]
    fn disabled_thinking_config_for_2_5_pro_uses_dynamic_budget() {
        let config = get_disabled_thinking_config("gemini-2.5-pro");
        assert_eq!(
            config,
            serde_json::json!({"thinkingBudget": -1}),
            "gemini-2.5-pro must not emit thinkingBudget: 0 in disabled mode"
        );
        // Preview / dated variants should land on the same branch so
        // a future minor bump doesn't silently regress to budget=0.
        let config = get_disabled_thinking_config("gemini-2.5-pro-preview-05-06");
        assert_eq!(config, serde_json::json!({"thinkingBudget": -1}));
    }

    /// 2.5-flash variants accept `thinkingBudget: 0` cleanly and
    /// disable thinking — that's the desired behaviour and the
    /// pro-family fix above must not touch them.
    #[test]
    fn disabled_thinking_config_for_2_5_flash_keeps_zero() {
        assert_eq!(
            get_disabled_thinking_config("gemini-2.5-flash"),
            serde_json::json!({"thinkingBudget": 0})
        );
        assert_eq!(
            get_disabled_thinking_config("gemini-2.5-flash-lite"),
            serde_json::json!({"thinkingBudget": 0})
        );
    }

    /// The regular Flash variant still gets the 128-token minimal —
    /// only Flash Lite raises the floor.
    #[test]
    fn google_budget_regular_flash_minimal_is_128() {
        assert_eq!(
            get_google_budget("gemini-2.5-flash", ThinkingLevel::Minimal, None),
            128
        );
    }

    /// The first non-empty `responseId` from a streamed chunk lands
    /// in the assistant message's `response_id` field; subsequent
    /// chunks with the same (or another) id don't overwrite it. An
    /// empty string in the field is treated as missing so a stray
    /// empty payload doesn't fool the capture into producing
    /// `Some("")` (which would mis-signal availability downstream).
    #[test]
    fn capture_chunk_response_id_keeps_first_non_empty_only() {
        let mut current: Option<String> = None;

        // Empty payload: skip.
        capture_chunk_response_id(&serde_json::json!({ "responseId": "" }), &mut current);
        assert!(current.is_none());

        // First real id wins.
        capture_chunk_response_id(&serde_json::json!({ "responseId": "abc123" }), &mut current);
        assert_eq!(current.as_deref(), Some("abc123"));

        // Later non-empty id must not overwrite.
        capture_chunk_response_id(&serde_json::json!({ "responseId": "def456" }), &mut current);
        assert_eq!(current.as_deref(), Some("abc123"));
    }

    /// `thought: true` is the definitive marker for thinking parts.
    /// The `thoughtSignature` field is a context-replay handle that
    /// can appear on any part type and MUST NOT be used to
    /// reclassify the part — text parts that carry a signature stay
    /// as text and persist the signature into `text_signature`.
    #[test]
    fn is_thinking_part_keys_only_on_thought_true() {
        let part_thought_true = serde_json::json!({
            "text": "let me reason",
            "thought": true,
        });
        assert!(is_thinking_part(&part_thought_true));

        // Signature only (no thought flag) -> NOT thinking; the
        // signature still rides on the text block via the converter.
        let part_with_sig_only = serde_json::json!({
            "text": "answer",
            "thoughtSignature": "dGVzdA==",
        });
        assert!(!is_thinking_part(&part_with_sig_only));

        // Both -> thinking (the explicit flag wins).
        let part_both = serde_json::json!({
            "text": "thinking text",
            "thought": true,
            "thoughtSignature": "dGVzdA==",
        });
        assert!(is_thinking_part(&part_both));
    }

    /// Plain text parts without `thought: true` are normal assistant
    /// text. Missing flag, explicit `false`, and a signature-only part
    /// all stay as text.
    #[test]
    fn is_thinking_part_treats_plain_text_as_non_thinking() {
        let plain_text = serde_json::json!({ "text": "Hello there" });
        assert!(!is_thinking_part(&plain_text));

        let empty_sig = serde_json::json!({
            "text": "Hello there",
            "thoughtSignature": "",
        });
        assert!(!is_thinking_part(&empty_sig));

        let explicit_false = serde_json::json!({
            "text": "Hello there",
            "thought": false,
        });
        assert!(!is_thinking_part(&explicit_false));
    }

    /// `is_gemma4_model` recognises both spellings — `gemma-4` and
    /// `gemma4` — but doesn't false-positive on Gemma 3, Gemini, or
    /// unrelated ids.
    #[test]
    fn is_gemma4_recognises_both_dash_and_squashed_forms() {
        for id in ["gemma-4", "gemma-4-9b", "gemma4", "gemma4-9b", "Gemma-4-2b"] {
            assert!(is_gemma4_model(id), "{id} should match gemma 4");
        }
        for id in [
            "gemma-3",
            "gemma-3-9b",
            "gemma",
            "gemini-3-pro",
            "gemini-2.5-flash",
            "",
        ] {
            assert!(!is_gemma4_model(id), "{id} must NOT match gemma 4");
        }
    }

    /// Gemma 4 collapses the four effort levels onto MINIMAL / HIGH:
    /// minimal+low → MINIMAL, medium+high+xhigh → HIGH. This is the
    /// upstream's documented two-bucket mapping.
    #[test]
    fn gemma4_thinking_level_collapses_to_minimal_or_high() {
        assert_eq!(
            get_gemini3_thinking_level(ThinkingLevel::Minimal, "gemma-4"),
            GoogleThinkingLevel::Minimal
        );
        assert_eq!(
            get_gemini3_thinking_level(ThinkingLevel::Low, "gemma-4"),
            GoogleThinkingLevel::Minimal
        );
        assert_eq!(
            get_gemini3_thinking_level(ThinkingLevel::Medium, "gemma-4"),
            GoogleThinkingLevel::High
        );
        assert_eq!(
            get_gemini3_thinking_level(ThinkingLevel::High, "gemma-4"),
            GoogleThinkingLevel::High
        );
        assert_eq!(
            get_gemini3_thinking_level(ThinkingLevel::Xhigh, "gemma-4"),
            GoogleThinkingLevel::High
        );
    }

    /// Disabling thinking on a Gemma 4 model emits the same `thinkingLevel`
    /// payload shape as Gemini 3 — not the legacy `thinkingBudget` form
    /// — and pins the value to MINIMAL.
    #[test]
    fn gemma4_disabled_thinking_uses_minimal_level() {
        let config = get_disabled_thinking_config("gemma-4-9b");
        assert_eq!(config["thinkingLevel"], "MINIMAL");
        assert!(
            config.get("thinkingBudget").is_none(),
            "Gemma 4 must not use the legacy thinkingBudget knob: {config}"
        );
    }
}
