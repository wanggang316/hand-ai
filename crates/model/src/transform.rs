//! Message transformation for cross-provider compatibility.
//!
//! The pipeline normalizes a slice of `Message`s so they can be safely replayed
//! against an arbitrary target `Model`. The public entry point keeps the
//! original signature and routes the input through a sequence of focused
//! transforms; each step has a single responsibility and is unit-tested in
//! isolation:
//!
//! 1. `downgrade_unsupported_tool_result_images` — drop image blocks from
//!    tool-results when the target model lacks image input.
//! 2. `drop_response_id_on_cross_api` — clear `responseId` when an assistant
//!    message is being replayed against a different API.
//! 3. `apply_eager_tool_input_streaming_compat` — wired through so future
//!    Anthropic providers that opt out of eager-streaming can drop partial
//!    tool-input markers; today this is a content-preserving pass.
//! 4. `transform_assistant_content` — the legacy core: thinking-block
//!    conversion, redacted-thinking drop, thought-signature stripping for
//!    cross-model replay (including Google targets — see D-07 in the
//!    model-package ExecPlan), and tool-call-id normalization.
//! 5. `flush_orphans_and_skip_errored` — drop errored/aborted assistant
//!    messages and synthesize "no result provided" tool results for orphaned
//!    tool calls.
//!
//! Per-stage iteration is intentional: each pass is small, self-describing,
//! and independently testable. A future single-pass fusion is possible if
//! benchmarks call for it; deferred for now.
//!
//! Parity test ports for cross-provider-handoff and tool-call-id-normalization
//! are deferred to M5 (the parity test harness milestone).

use crate::types::{
    AnthropicMessagesCompat, Api, AssistantContentBlock, AssistantMessage, Compat, InputType,
    Message, Model, StopReason, TextContent, ToolCall, ToolResultContent, ToolResultMessage,
    UserContent, UserContentBlock,
};
use std::collections::{HashMap, HashSet};

/// Optional callback for normalizing tool call IDs across providers.
pub type NormalizeToolCallIdFn = Box<dyn Fn(&str, &Model, &AssistantMessage) -> String>;

const TOOL_RESULT_IMAGE_PLACEHOLDER: &str = "(tool image omitted: model does not support images)";
const USER_IMAGE_PLACEHOLDER: &str = "(image omitted: model does not support images)";

/// Transform messages for cross-provider compatibility.
///
/// See module docs for the list of stages this pipeline applies. The public
/// signature is preserved for backwards compatibility with existing callers.
pub fn transform_messages(
    messages: &[Message],
    model: &Model,
    normalize_tool_call_id: Option<&NormalizeToolCallIdFn>,
) -> Vec<Message> {
    let staged: Vec<Message> = messages.to_vec();
    let staged = downgrade_unsupported_user_images(staged, model);
    let staged = downgrade_unsupported_tool_result_images(staged, model);
    let staged = drop_response_id_on_cross_api(staged, model);
    let staged = apply_eager_tool_input_streaming_compat(staged, model);
    let staged = transform_assistant_content(staged, model, normalize_tool_call_id);
    flush_orphans_and_skip_errored(staged)
}

/// Replace image blocks inside user messages with a text placeholder when
/// the target model does not advertise `InputType::Image` support.
///
/// Previously each provider's `convert_messages` silently filtered image
/// blocks out, so non-vision models received a user turn with no signal
/// that the user had attached an image. Producing a placeholder keeps
/// the conversation auditable and lets the model acknowledge the
/// limitation in its reply.
fn downgrade_unsupported_user_images(messages: Vec<Message>, model: &Model) -> Vec<Message> {
    if model.input.contains(&InputType::Image) {
        return messages;
    }
    messages
        .into_iter()
        .map(|msg| match msg {
            Message::User(mut u) => {
                if let UserContent::Blocks(blocks) = u.content {
                    let downgraded = replace_user_images_with_placeholder(blocks);
                    u.content = UserContent::Blocks(downgraded);
                }
                Message::User(u)
            }
            other => other,
        })
        .collect()
}

fn replace_user_images_with_placeholder(blocks: Vec<UserContentBlock>) -> Vec<UserContentBlock> {
    let mut out: Vec<UserContentBlock> = Vec::with_capacity(blocks.len());
    let mut previous_was_placeholder = false;
    for block in blocks {
        match block {
            UserContentBlock::Image(_) => {
                if !previous_was_placeholder {
                    out.push(UserContentBlock::Text(TextContent::new(
                        USER_IMAGE_PLACEHOLDER,
                    )));
                }
                previous_was_placeholder = true;
            }
            other => {
                if let UserContentBlock::Text(ref t) = other {
                    previous_was_placeholder = t.text == USER_IMAGE_PLACEHOLDER;
                } else {
                    previous_was_placeholder = false;
                }
                out.push(other);
            }
        }
    }
    out
}

/// Normalize a tool call ID to be compatible with Anthropic's requirements.
///
/// Anthropic requires IDs matching `^[a-zA-Z0-9_-]+$` (max 64 chars).
/// OpenAI Responses API generates IDs that are 450+ chars with special characters.
pub fn normalize_tool_call_id_for_anthropic(id: &str) -> String {
    let normalized: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();

    if normalized.len() > 64 {
        normalized[..64].to_string()
    } else {
        normalized
    }
}

/// Whether the resolved compat for `model` opts in to eager tool-input
/// streaming. Anthropic Messages defaults to `true` (the historical Anthropic
/// behavior); other APIs default to `false`.
///
/// An explicit `Compat::AnthropicMessages` override always wins.
/// Repair a JSON payload that may contain raw control characters and
/// invalid backslash escapes inside string literals.
///
/// Streaming providers (notably Anthropic with `input_json_delta`) sometimes
/// emit tool-call arguments containing literal `\t`/`\n`/`\r` bytes or
/// invalid escape sequences such as `\H`. Plain `serde_json::from_str` fails
/// on these and we'd otherwise drop the entire payload to `{}`, silently
/// breaking the tool call. The repair pass escapes raw control characters
/// inside string literals and doubles backslashes before invalid escapes so
/// the same arguments round-trip end-to-end.
pub fn repair_json(json: &str) -> String {
    const VALID_ESCAPES: &[char] = &['"', '\\', '/', 'b', 'f', 'n', 'r', 't', 'u'];
    let chars: Vec<char> = json.chars().collect();
    let mut out = String::with_capacity(json.len());
    let mut in_string = false;
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if !in_string {
            out.push(ch);
            if ch == '"' {
                in_string = true;
            }
            i += 1;
            continue;
        }
        if ch == '"' {
            out.push(ch);
            in_string = false;
            i += 1;
            continue;
        }
        if ch == '\\' {
            if i + 1 >= chars.len() {
                out.push_str("\\\\");
                i += 1;
                continue;
            }
            let next = chars[i + 1];
            if next == 'u' && i + 5 < chars.len() {
                let hex: String = chars[i + 2..i + 6].iter().collect();
                if hex.chars().all(|c| c.is_ascii_hexdigit()) {
                    out.push_str("\\u");
                    out.push_str(&hex);
                    i += 6;
                    continue;
                }
            }
            if VALID_ESCAPES.contains(&next) {
                out.push('\\');
                out.push(next);
                i += 2;
                continue;
            }
            // Invalid escape: double the backslash and keep the original
            // char on the next iteration so it can be control-escaped if
            // needed.
            out.push_str("\\\\");
            i += 1;
            continue;
        }
        let cp = ch as u32;
        if cp < 0x20 {
            match ch {
                '\u{08}' => out.push_str("\\b"),
                '\u{0c}' => out.push_str("\\f"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                _ => out.push_str(&format!("\\u{:04x}", cp)),
            }
        } else {
            out.push(ch);
        }
        i += 1;
    }
    out
}

/// Parse a JSON payload, retrying with [`repair_json`] on failure. Returns
/// `None` if both passes fail.
pub fn parse_json_with_repair(json: &str) -> Option<serde_json::Value> {
    if let Ok(v) = serde_json::from_str(json) {
        return Some(v);
    }
    let repaired = repair_json(json);
    if repaired != json {
        serde_json::from_str(&repaired).ok()
    } else {
        None
    }
}

pub fn supports_eager_tool_input_streaming(model: &Model) -> bool {
    if let Some(Compat::AnthropicMessages(AnthropicMessagesCompat {
        supports_eager_tool_input_streaming: Some(value),
        ..
    })) = &model.compat
    {
        return *value;
    }
    matches!(model.api, Api::AnthropicMessages)
}

// ---------------------------------------------------------------------------
// Stage 1 — image-bearing tool-result routing
// ---------------------------------------------------------------------------

/// Replace image blocks inside tool-results with a text placeholder when the
/// target model does not advertise `InputType::Image` support.
fn downgrade_unsupported_tool_result_images(messages: Vec<Message>, model: &Model) -> Vec<Message> {
    if model.input.contains(&InputType::Image) {
        return messages;
    }

    messages
        .into_iter()
        .map(|msg| match msg {
            Message::ToolResult(tr) => {
                let (downgraded_content, downgrade_info) = downgrade_tool_result_content(&tr);
                if downgrade_info.is_none() {
                    return Message::ToolResult(tr);
                }
                // TODO(M12): emit AssistantMessageDiagnostic {
                //     kind: PayloadDowngraded,
                //     message: "Tool result image stripped: target model does not support image input.",
                //     details: Some({"toolName": tr.tool_name, "toolCallId": tr.tool_call_id}),
                // } and attach it to the *next* AssistantMessage's diagnostics.
                let mut new_tr = tr;
                new_tr.content = downgraded_content;
                Message::ToolResult(new_tr)
            }
            other => other,
        })
        .collect()
}

/// Replace each image block with a text placeholder, collapsing consecutive
/// images into a single placeholder. Returns the new content along with `Some`
/// payload describing the downgrade if any image was replaced.
fn downgrade_tool_result_content(
    tr: &ToolResultMessage,
) -> (Vec<ToolResultContent>, Option<serde_json::Value>) {
    let mut downgraded = false;
    let mut previous_was_placeholder = false;
    let mut out: Vec<ToolResultContent> = Vec::with_capacity(tr.content.len());

    for block in &tr.content {
        match block {
            ToolResultContent::Image(_) => {
                downgraded = true;
                if !previous_was_placeholder {
                    out.push(ToolResultContent::Text(TextContent::new(
                        TOOL_RESULT_IMAGE_PLACEHOLDER,
                    )));
                }
                previous_was_placeholder = true;
            }
            ToolResultContent::Text(t) => {
                previous_was_placeholder = t.text == TOOL_RESULT_IMAGE_PLACEHOLDER;
                out.push(ToolResultContent::Text(t.clone()));
            }
        }
    }

    let info = if downgraded {
        Some(serde_json::json!({
            "toolName": tr.tool_name,
            "toolCallId": tr.tool_call_id,
        }))
    } else {
        None
    };
    (out, info)
}

// ---------------------------------------------------------------------------
// Stage 2 — response-id normalization
// ---------------------------------------------------------------------------

/// Drop `response_id` on any assistant message whose source API differs from
/// the target's. Foreign IDs get rejected (e.g. an OpenAI Responses `resp_…`
/// id replayed against Anthropic Messages).
fn drop_response_id_on_cross_api(messages: Vec<Message>, model: &Model) -> Vec<Message> {
    messages
        .into_iter()
        .map(|msg| match msg {
            Message::Assistant(mut assistant) => {
                if assistant.api != model.api && assistant.response_id.is_some() {
                    assistant.response_id = None;
                }
                Message::Assistant(assistant)
            }
            other => other,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Stage 3 — eager tool-input streaming compat
// ---------------------------------------------------------------------------

/// Wire eager-tool-input-streaming compat through the pipeline.
///
/// When the target is `Api::AnthropicMessages` and the resolved compat opts
/// out of eager streaming, partial-form tool inputs would need to be
/// collapsed to their final form. Today our types do not carry partial markers
/// so this is a no-op data-wise; keeping the stage in place lets future
/// providers that flip `supports_eager_tool_input_streaming = true` plug in
/// without restructuring the pipeline.
fn apply_eager_tool_input_streaming_compat(messages: Vec<Message>, model: &Model) -> Vec<Message> {
    let _eager_supported = supports_eager_tool_input_streaming(model);
    // No-op today; see doc comment.
    messages
}

// ---------------------------------------------------------------------------
// Stage 4 — assistant-content normalization (legacy core)
// ---------------------------------------------------------------------------

/// Apply the historical assistant-content transformations:
/// thinking-block conversion, redacted-thinking drop, cross-model
/// thought-signature stripping, and tool-call-id normalization.
fn transform_assistant_content(
    messages: Vec<Message>,
    model: &Model,
    normalize_tool_call_id: Option<&NormalizeToolCallIdFn>,
) -> Vec<Message> {
    let mut tool_call_id_map: HashMap<String, String> = HashMap::new();

    messages
        .into_iter()
        .map(|msg| match msg {
            Message::User(u) => Message::User(u),

            Message::ToolResult(mut tr) => {
                if let Some(normalized) = tool_call_id_map.get(&tr.tool_call_id) {
                    tr.tool_call_id = normalized.clone();
                }
                Message::ToolResult(tr)
            }

            Message::Assistant(assistant) => {
                let is_same_model = assistant.provider == model.provider
                    && assistant.api == model.api
                    && assistant.model == model.id;

                // Only clone the message when we'll actually need it as the
                // callback context for the tool-id normalizer. Same-model
                // assistants and cases without a normalizer skip the clone.
                let assistant_for_callback = if !is_same_model && normalize_tool_call_id.is_some() {
                    Some(assistant.clone())
                } else {
                    None
                };
                let transformed_content: Vec<AssistantContentBlock> = assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        AssistantContentBlock::Thinking(t) => {
                            if t.redacted == Some(true) {
                                return if is_same_model {
                                    Some(block.clone())
                                } else {
                                    None
                                };
                            }
                            // Keep thinking with signature for same model replay
                            if is_same_model && t.thinking_signature.is_some() {
                                return Some(block.clone());
                            }
                            // Skip empty thinking blocks
                            if t.thinking.trim().is_empty() {
                                return None;
                            }
                            if is_same_model {
                                Some(block.clone())
                            } else {
                                Some(AssistantContentBlock::Text(TextContent::new(&t.thinking)))
                            }
                        }

                        AssistantContentBlock::Text(t) => {
                            if is_same_model {
                                Some(block.clone())
                            } else {
                                Some(AssistantContentBlock::Text(TextContent::new(&t.text)))
                            }
                        }

                        AssistantContentBlock::ToolCall(tc) => {
                            let mut new_tc = tc.clone();

                            // Strip foreign thought signatures unconditionally for
                            // cross-model replay. Provider-specific signatures encode
                            // different opaque payloads and cannot be replayed
                            // against another model. For Google targets this
                            // matches the TS reference (`google-shared.ts`):
                            // foreign signatures are dropped, not replaced with a
                            // fabricated sentinel — see ExecPlan D-07.
                            if !is_same_model {
                                new_tc.thought_signature = None;
                            }

                            if !is_same_model
                                && let Some(normalizer) = normalize_tool_call_id
                                && let Some(source_msg) = assistant_for_callback.as_ref()
                            {
                                let normalized = normalizer(&tc.id, model, source_msg);
                                if normalized != tc.id {
                                    tool_call_id_map.insert(tc.id.clone(), normalized.clone());
                                    new_tc.id = normalized;
                                }
                            }

                            Some(AssistantContentBlock::ToolCall(new_tc))
                        }
                    })
                    .collect();

                let mut new_msg = assistant;
                new_msg.content = transformed_content;
                Message::Assistant(new_msg)
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Stage 5 — orphan flush + errored-message skip
// ---------------------------------------------------------------------------

/// Drop errored/aborted assistant messages and emit synthetic tool results for
/// orphaned tool calls (those without a matching `ToolResult` in the same
/// turn).
///
/// This stage also flushes any trailing orphan tool calls — i.e. tool calls
/// in the final assistant message of the transcript that never received a
/// result — so the returned conversation is well-formed even when the input
/// ends mid-tool-turn. This isn't called out in the M3 bullet list, but
/// matches `transform-messages.ts` parity behavior and is consistent with the
/// overall transform intent (every tool call must be paired with a result).
fn flush_orphans_and_skip_errored(messages: Vec<Message>) -> Vec<Message> {
    let mut result: Vec<Message> = Vec::new();
    let mut pending_tool_calls: Vec<ToolCall> = Vec::new();
    let mut existing_tool_result_ids: HashSet<String> = HashSet::new();

    for msg in messages {
        match msg {
            Message::Assistant(assistant) => {
                flush_orphaned_tool_calls(
                    &mut result,
                    &mut pending_tool_calls,
                    &existing_tool_result_ids,
                );
                existing_tool_result_ids.clear();

                if assistant.stop_reason == StopReason::Error
                    || assistant.stop_reason == StopReason::Aborted
                {
                    continue;
                }

                let tool_calls: Vec<ToolCall> = assistant
                    .content
                    .iter()
                    .filter_map(|b| {
                        if let AssistantContentBlock::ToolCall(tc) = b {
                            Some(tc.clone())
                        } else {
                            None
                        }
                    })
                    .collect();

                if !tool_calls.is_empty() {
                    pending_tool_calls = tool_calls;
                }

                result.push(Message::Assistant(assistant));
            }

            Message::ToolResult(tr) => {
                existing_tool_result_ids.insert(tr.tool_call_id.clone());
                result.push(Message::ToolResult(tr));
            }

            Message::User(u) => {
                flush_orphaned_tool_calls(
                    &mut result,
                    &mut pending_tool_calls,
                    &existing_tool_result_ids,
                );
                existing_tool_result_ids.clear();
                result.push(Message::User(u));
            }
        }
    }

    // If the conversation ends with unresolved tool calls, synthesize results.
    flush_orphaned_tool_calls(
        &mut result,
        &mut pending_tool_calls,
        &existing_tool_result_ids,
    );

    result
}

fn flush_orphaned_tool_calls(
    result: &mut Vec<Message>,
    pending_tool_calls: &mut Vec<ToolCall>,
    existing_ids: &HashSet<String>,
) {
    for tc in pending_tool_calls.drain(..) {
        if !existing_ids.contains(&tc.id) {
            result.push(Message::ToolResult(ToolResultMessage::new_error(
                &tc.id,
                &tc.name,
                "No result provided",
            )));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        Api, AssistantMessage, Provider, StopReason, TextContent, ThinkingContent,
        ToolResultContent, Usage, UserMessage,
    };

    fn test_model() -> Model {
        crate::types::Model {
            id: "claude-sonnet-4-20250514".into(),
            name: "Claude Sonnet 4".into(),
            api: Api::AnthropicMessages,
            provider: Provider::Anthropic,
            base_url: String::new(),
            reasoning: true,
            input: vec![crate::types::InputType::Text],
            cost: crate::types::Cost {
                input: 3.0,
                output: 15.0,
                cache_read: 0.3,
                cache_write: 3.75,
            },
            context_window: 200_000,
            max_tokens: 16384,
            headers: None,
            compat: None,
            thinking_level_map: None,
        }
    }

    fn make_assistant(
        content: Vec<AssistantContentBlock>,
        provider: Provider,
        api: Api,
        model_id: &str,
    ) -> AssistantMessage {
        AssistantMessage {
            role: "assistant".to_string(),
            content,
            api,
            provider,
            model: model_id.to_string(),
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            raw_stop_reason: None,
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        }
    }

    #[test]
    fn test_same_model_preserves_thinking() {
        let model = test_model();
        let messages = vec![Message::Assistant(make_assistant(
            vec![
                AssistantContentBlock::Thinking(ThinkingContent::new("reasoning")),
                AssistantContentBlock::Text(TextContent::new("response")),
            ],
            Provider::Anthropic,
            Api::AnthropicMessages,
            "claude-sonnet-4-20250514",
        ))];

        let result = transform_messages(&messages, &model, None);
        assert_eq!(result.len(), 1);
        if let Message::Assistant(a) = &result[0] {
            assert!(matches!(&a.content[0], AssistantContentBlock::Thinking(_)));
            assert!(matches!(&a.content[1], AssistantContentBlock::Text(_)));
        }
    }

    #[test]
    fn test_cross_provider_converts_thinking_to_text() {
        let model = test_model();
        let messages = vec![Message::Assistant(make_assistant(
            vec![
                AssistantContentBlock::Thinking(ThinkingContent::new("reasoning")),
                AssistantContentBlock::Text(TextContent::new("response")),
            ],
            Provider::Google,
            Api::GoogleGenerativeAi,
            "gemini-2.5-flash",
        ))];

        let result = transform_messages(&messages, &model, None);
        assert_eq!(result.len(), 1);
        if let Message::Assistant(a) = &result[0] {
            assert!(
                matches!(&a.content[0], AssistantContentBlock::Text(t) if t.text == "reasoning")
            );
            assert!(matches!(&a.content[1], AssistantContentBlock::Text(_)));
        }
    }

    #[test]
    fn test_empty_thinking_blocks_removed() {
        let model = test_model();
        let messages = vec![Message::Assistant(make_assistant(
            vec![
                AssistantContentBlock::Thinking(ThinkingContent::new("  ")),
                AssistantContentBlock::Text(TextContent::new("response")),
            ],
            Provider::Google,
            Api::GoogleGenerativeAi,
            "gemini-2.5-flash",
        ))];

        let result = transform_messages(&messages, &model, None);
        if let Message::Assistant(a) = &result[0] {
            assert_eq!(a.content.len(), 1);
            assert!(matches!(&a.content[0], AssistantContentBlock::Text(_)));
        }
    }

    #[test]
    fn test_errored_assistant_messages_skipped() {
        let model = test_model();
        let mut errored = make_assistant(
            vec![AssistantContentBlock::Text(TextContent::new("partial"))],
            Provider::Anthropic,
            Api::AnthropicMessages,
            "claude-sonnet-4-20250514",
        );
        errored.stop_reason = StopReason::Error;

        let messages = vec![
            Message::User(UserMessage::new_text("Hello")),
            Message::Assistant(errored),
        ];

        let result = transform_messages(&messages, &model, None);
        assert_eq!(result.len(), 1);
        assert!(matches!(&result[0], Message::User(_)));
    }

    /// Non-vision models previously had image blocks silently filtered
    /// inside each provider's `convert_messages`. The user lost any
    /// signal that their attached image was dropped. Now the transform
    /// pipeline replaces each image with a text placeholder so the
    /// model can acknowledge the limitation and the user sees what
    /// happened.
    #[test]
    fn user_image_blocks_become_placeholder_for_non_vision_models() {
        use crate::types::{ImageContent, UserContent, UserContentBlock};
        let mut model = test_model();
        model.input = vec![InputType::Text]; // non-vision

        let user_msg = Message::User(UserMessage {
            role: "user".into(),
            content: UserContent::Blocks(vec![
                UserContentBlock::Text(TextContent::new("Look at this:")),
                UserContentBlock::Image(ImageContent::new("base64data", "image/png")),
                UserContentBlock::Image(ImageContent::new("base64data2", "image/png")),
                UserContentBlock::Text(TextContent::new("Thoughts?")),
            ]),
            timestamp: 0,
        });
        let out = transform_messages(&[user_msg], &model, None);
        let Message::User(user) = &out[0] else {
            panic!("expected user message");
        };
        let UserContent::Blocks(blocks) = &user.content else {
            panic!("expected block content");
        };
        // Two consecutive images collapse into a single placeholder.
        assert_eq!(blocks.len(), 3, "blocks: {blocks:?}");
        match &blocks[1] {
            UserContentBlock::Text(t) => assert!(
                t.text.contains("image omitted"),
                "expected placeholder, got {:?}",
                t.text
            ),
            other => panic!("expected text placeholder, got {other:?}"),
        }
        // No image block survives.
        for b in blocks {
            assert!(
                !matches!(b, UserContentBlock::Image(_)),
                "image block should not survive: {b:?}"
            );
        }
    }

    /// Vision-capable models keep their image blocks intact — the
    /// downgrade only fires when `InputType::Image` is missing.
    #[test]
    fn user_image_blocks_passthrough_for_vision_models() {
        use crate::types::{ImageContent, UserContent, UserContentBlock};
        let mut model = test_model();
        model.input = vec![InputType::Text, InputType::Image];

        let user_msg = Message::User(UserMessage {
            role: "user".into(),
            content: UserContent::Blocks(vec![
                UserContentBlock::Text(TextContent::new("look:")),
                UserContentBlock::Image(ImageContent::new("data", "image/png")),
            ]),
            timestamp: 0,
        });
        let out = transform_messages(&[user_msg], &model, None);
        let Message::User(user) = &out[0] else {
            panic!("user expected");
        };
        let UserContent::Blocks(blocks) = &user.content else {
            panic!("blocks expected");
        };
        assert!(
            blocks
                .iter()
                .any(|b| matches!(b, UserContentBlock::Image(_))),
            "image must survive for vision models"
        );
    }

    #[test]
    fn test_orphaned_tool_calls_get_synthetic_results() {
        let model = test_model();
        let messages = vec![
            Message::Assistant(make_assistant(
                vec![AssistantContentBlock::ToolCall(ToolCall::new(
                    "tc1",
                    "read",
                    serde_json::json!({"path": "/tmp/test"}),
                ))],
                Provider::Anthropic,
                Api::AnthropicMessages,
                "claude-sonnet-4-20250514",
            )),
            Message::User(UserMessage::new_text("Continue")),
        ];

        let result = transform_messages(&messages, &model, None);
        assert_eq!(result.len(), 3);
        assert!(matches!(&result[0], Message::Assistant(_)));
        assert!(
            matches!(&result[1], Message::ToolResult(tr) if tr.tool_call_id == "tc1" && tr.is_error)
        );
        assert!(matches!(&result[2], Message::User(_)));
    }

    #[test]
    fn test_tool_call_id_normalization() {
        let model = test_model();
        let normalizer: NormalizeToolCallIdFn =
            Box::new(|id: &str, _model: &Model, _msg: &AssistantMessage| {
                normalize_tool_call_id_for_anthropic(id)
            });

        let messages = vec![
            Message::Assistant(make_assistant(
                vec![AssistantContentBlock::ToolCall(ToolCall::new(
                    "call|with|special|chars",
                    "read",
                    serde_json::json!({}),
                ))],
                Provider::OpenAI,
                Api::OpenAICompletions,
                "gpt-4o",
            )),
            Message::ToolResult(ToolResultMessage::new(
                "call|with|special|chars",
                "read",
                vec![ToolResultContent::Text(TextContent::new("result"))],
            )),
        ];

        let result = transform_messages(&messages, &model, Some(&normalizer));
        if let Message::Assistant(a) = &result[0]
            && let AssistantContentBlock::ToolCall(tc) = &a.content[0]
        {
            assert_eq!(tc.id, "call_with_special_chars");
        }
        if let Message::ToolResult(tr) = &result[1] {
            assert_eq!(tr.tool_call_id, "call_with_special_chars");
        }
    }

    #[test]
    fn test_normalize_tool_call_id_truncation() {
        let long_id = "a".repeat(100);
        let result = normalize_tool_call_id_for_anthropic(&long_id);
        assert_eq!(result.len(), 64);
    }

    /// OpenAI Responses generates 450+ character pipe-separated tool-call
    /// IDs that crash Anthropic's validator (which caps IDs at 64 chars
    /// matching `^[a-zA-Z0-9_-]+$`). Pin the exact pathological payload so
    /// the normalizer can never regress on it again.
    #[test]
    fn normalize_handles_long_pipe_separated_tool_call_id() {
        let failing_id = "call_pAYbIr76hXIjncD9UE4eGfnS|t5nnb2qYMFWGSsr13fhCd1CaCu3t3qONEPuOudu4HSVEtA8YJSL6FAZUxvoOoD792VIJWl91g87EdqsCWp9krVsdBysQoDaf9lMCLb8BS4EYi4gQd5kBQBYLlgD71PYwvf+TbMD9J9/5OMD42oxSRj8H+vRf78/l2Xla33LWz4nOgsddBlbvabICRs8GHt5C9PK5keFtzyi3lsyVKNlfduK3iphsZqs4MLv4zyGJnvZo/+QzShyk5xnMSQX/f98+aEoNflEApCdEOXipipgeiNWnpFSHbcwmMkZoJhURNu+JEz3xCh1mrXeYoN5o+trLL3IXJacSsLYXDrYTipZZbJFRPAucgbnjYBC+/ZzJOfkwCs+Gkw7EoZR7ZQgJ8ma+9586n4tT4cI8DEhBSZsWMjrCt8dxKg==";
        let result = normalize_tool_call_id_for_anthropic(failing_id);
        // Length cap.
        assert_eq!(
            result.len(),
            64,
            "truncated to fit Anthropic's 64-char limit"
        );
        // No special characters survive the normalization (only [a-zA-Z0-9_-]).
        assert!(
            result
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
            "unexpected chars in {result:?}"
        );
        // Prefix preserved so the truncated ID is still recognisable as
        // originating from a `call_*` ID (deterministic mapping).
        assert!(result.starts_with("call_"), "got: {result:?}");
    }

    /// Pipe characters are not legal in Anthropic IDs — verify the canonical
    /// substitution char is `_` (not `-` or anything else) so cross-provider
    /// handoff stays deterministic and round-trip stable.
    #[test]
    fn normalize_replaces_pipes_with_underscores() {
        let id = "call_abc|def|ghi";
        let result = normalize_tool_call_id_for_anthropic(id);
        assert_eq!(result, "call_abc_def_ghi");
    }

    /// `=` / `+` / `/` (base64 alphabet) all collapse to `_`. This is what
    /// breaks naive ID round-tripping when OpenAI Responses sends a base64
    /// blob through Anthropic.
    #[test]
    fn normalize_replaces_base64_alphabet_with_underscores() {
        let id = "call_a+b/c=d";
        let result = normalize_tool_call_id_for_anthropic(id);
        assert_eq!(result, "call_a_b_c_d");
    }

    /// Empty input returns empty output rather than panicking. This is a
    /// defensive contract for malformed provider responses.
    #[test]
    fn normalize_empty_id_returns_empty() {
        let result = normalize_tool_call_id_for_anthropic("");
        assert_eq!(result, "");
    }

    /// Already-clean IDs pass through unchanged. Important for the common
    /// case so we don't accidentally regress short Anthropic-shaped IDs.
    #[test]
    fn normalize_passes_clean_ids_through_unchanged() {
        let id = "toolu_01abc-DEF_ghi";
        let result = normalize_tool_call_id_for_anthropic(id);
        assert_eq!(result, id);
    }

    #[test]
    fn test_cross_model_strips_thought_signature() {
        let model = test_model();
        let mut tc = ToolCall::new("tc1", "read", serde_json::json!({}));
        tc.thought_signature = Some("signature123".to_string());

        let messages = vec![Message::Assistant(make_assistant(
            vec![AssistantContentBlock::ToolCall(tc)],
            Provider::Google,
            Api::GoogleGenerativeAi,
            "gemini-2.5-flash",
        ))];

        let result = transform_messages(&messages, &model, None);
        if let Message::Assistant(a) = &result[0]
            && let AssistantContentBlock::ToolCall(tc) = &a.content[0]
        {
            assert!(tc.thought_signature.is_none());
        }
    }

    // -----------------------------------------------------------------------
    // JSON repair
    // -----------------------------------------------------------------------

    #[test]
    fn repair_passes_through_valid_json() {
        let input = r#"{"key":"value","n":42}"#;
        assert_eq!(repair_json(input), input);
    }

    #[test]
    fn repair_escapes_raw_tab_inside_string() {
        let input = "{\"text\":\"col1\tcol2\"}";
        let repaired = repair_json(input);
        assert_eq!(repaired, r#"{"text":"col1\tcol2"}"#);
        let parsed: serde_json::Value = serde_json::from_str(&repaired).unwrap();
        assert_eq!(parsed["text"], "col1\tcol2");
    }

    #[test]
    fn repair_escapes_raw_newline_and_carriage_return() {
        let input = "{\"text\":\"a\nb\rc\"}";
        let parsed: serde_json::Value = serde_json::from_str(&repair_json(input)).unwrap();
        assert_eq!(parsed["text"], "a\nb\rc");
    }

    #[test]
    fn repair_doubles_invalid_backslash_escape() {
        let input = r#"{"path":"A\H"}"#;
        let parsed: serde_json::Value = serde_json::from_str(&repair_json(input)).unwrap();
        // \H is invalid → repaired as literal "\H" (two chars: backslash + H).
        assert_eq!(parsed["path"], "A\\H");
    }

    #[test]
    fn repair_preserves_valid_unicode_escape() {
        let input = r#"{"s":"é"}"#;
        let parsed: serde_json::Value = serde_json::from_str(&repair_json(input)).unwrap();
        assert_eq!(parsed["s"], "é");
    }

    #[test]
    fn repair_handles_trailing_backslash() {
        let input = "{\"s\":\"a\\";
        let repaired = repair_json(input);
        // Trailing lone backslash gets doubled. Result still isn't valid JSON
        // (truncated mid-string), but it's no longer malformed in a way that
        // poisons later passes.
        assert!(repaired.ends_with("\\\\"));
    }

    #[test]
    fn repair_combines_invalid_escape_and_raw_control_in_one_payload() {
        // path="A\H" (invalid escape) and text="col1<TAB>col2" (raw control)
        // co-occurred in real-world Anthropic SSE streams.
        let input = "{\"path\":\"A\\H\",\"text\":\"col1\tcol2\"}";
        let parsed = parse_json_with_repair(input).expect("must repair successfully");
        assert_eq!(parsed["path"], "A\\H");
        assert_eq!(parsed["text"], "col1\tcol2");
    }

    #[test]
    fn parse_json_with_repair_returns_none_on_truncated_input() {
        // Repair can't conjure a closing brace.
        assert!(parse_json_with_repair("{\"k\":").is_none());
    }

    #[test]
    fn parse_json_with_repair_passes_clean_input_through() {
        let v = parse_json_with_repair(r#"{"k":1}"#).unwrap();
        assert_eq!(v["k"], 1);
    }
}
