//! Message transformation for cross-provider compatibility.
//!
//! Normalizes tool call IDs, converts thinking blocks, and inserts synthetic
//! tool results for orphaned tool calls across provider boundaries.

use crate::types::{
    AssistantContentBlock, AssistantMessage, Message, Model, StopReason, TextContent, ToolCall,
    ToolResultMessage,
};
use std::collections::HashSet;

/// Optional callback for normalizing tool call IDs across providers.
pub type NormalizeToolCallIdFn = Box<dyn Fn(&str, &Model, &AssistantMessage) -> String>;

/// Transform messages for cross-provider compatibility.
///
/// This function:
/// 1. Converts thinking blocks to plain text for different model/provider combinations
/// 2. Drops redacted thinking from cross-model messages
/// 3. Normalizes tool call IDs if a normalizer is provided
/// 4. Strips thought signatures from cross-model tool calls
/// 5. Skips errored/aborted assistant messages
/// 6. Inserts synthetic tool results for orphaned tool calls
pub fn transform_messages(
    messages: &[Message],
    model: &Model,
    normalize_tool_call_id: Option<&NormalizeToolCallIdFn>,
) -> Vec<Message> {
    let mut tool_call_id_map: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    // First pass: transform message content
    let transformed: Vec<Message> = messages
        .iter()
        .map(|msg| match msg {
            Message::User(u) => Message::User(u.clone()),

            Message::ToolResult(tr) => {
                if let Some(normalized) = tool_call_id_map.get(&tr.tool_call_id) {
                    let mut new_tr = tr.clone();
                    new_tr.tool_call_id = normalized.clone();
                    Message::ToolResult(new_tr)
                } else {
                    Message::ToolResult(tr.clone())
                }
            }

            Message::Assistant(assistant) => {
                let is_same_model = assistant.provider == model.provider
                    && assistant.api == model.api
                    && assistant.model == model.id;

                let transformed_content: Vec<AssistantContentBlock> = assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        AssistantContentBlock::Thinking(t) => {
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
                                // Convert to plain text for cross-provider
                                Some(AssistantContentBlock::Text(TextContent::new(&t.thinking)))
                            }
                        }

                        AssistantContentBlock::Text(t) => {
                            if is_same_model {
                                Some(block.clone())
                            } else {
                                // Strip text signature for cross-provider
                                Some(AssistantContentBlock::Text(TextContent::new(&t.text)))
                            }
                        }

                        AssistantContentBlock::ToolCall(tc) => {
                            let mut new_tc = tc.clone();

                            // Strip thought signature for cross-model
                            if !is_same_model {
                                new_tc.thought_signature = None;
                            }

                            // Normalize tool call ID for cross-model
                            if !is_same_model && let Some(normalizer) = normalize_tool_call_id {
                                let normalized = normalizer(&tc.id, model, assistant);
                                if normalized != tc.id {
                                    tool_call_id_map.insert(tc.id.clone(), normalized.clone());
                                    new_tc.id = normalized;
                                }
                            }

                            Some(AssistantContentBlock::ToolCall(new_tc))
                        }
                    })
                    .collect();

                let mut new_msg = assistant.clone();
                new_msg.content = transformed_content;
                Message::Assistant(new_msg)
            }
        })
        .collect();

    // Second pass: handle orphaned tool calls and skip errored messages
    let mut result: Vec<Message> = Vec::new();
    let mut pending_tool_calls: Vec<ToolCall> = Vec::new();
    let mut existing_tool_result_ids: HashSet<String> = HashSet::new();

    for msg in &transformed {
        match msg {
            Message::Assistant(assistant) => {
                // Insert synthetic results for previous orphaned tool calls
                flush_orphaned_tool_calls(
                    &mut result,
                    &mut pending_tool_calls,
                    &existing_tool_result_ids,
                );
                existing_tool_result_ids.clear();

                // Skip errored/aborted assistant messages
                if assistant.stop_reason == StopReason::Error
                    || assistant.stop_reason == StopReason::Aborted
                {
                    continue;
                }

                // Track tool calls from this assistant message
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

                result.push(msg.clone());
            }

            Message::ToolResult(tr) => {
                existing_tool_result_ids.insert(tr.tool_call_id.clone());
                result.push(msg.clone());
            }

            Message::User(_) => {
                // User message interrupts tool flow
                flush_orphaned_tool_calls(
                    &mut result,
                    &mut pending_tool_calls,
                    &existing_tool_result_ids,
                );
                existing_tool_result_ids.clear();
                result.push(msg.clone());
            }
        }
    }

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
            // Thinking should be converted to text
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
            // No tool result, then a user message
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
}
