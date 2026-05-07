//! Context compaction — summarizes old messages to stay within token limits.

use crate::core::settings::CompactionSettings;
use model::Message;

/// Result of a compaction operation.
#[derive(Debug, Clone)]
pub struct CompactionResult {
    /// Summary of compacted messages.
    pub summary: String,
    /// ID of the first message kept after compaction.
    pub first_kept_entry_id: String,
    /// Approximate token count before compaction.
    pub tokens_before: usize,
}

/// File operations tracked during compaction.
#[derive(Debug, Clone, Default)]
pub struct FileOperations {
    pub read: Vec<String>,
    pub edited: Vec<String>,
}

/// Estimate token count from a string (rough: ~4 chars per token).
pub fn estimate_tokens(text: &str) -> usize {
    // Simple word-based estimation
    text.split_whitespace().count().max(text.len() / 4)
}

/// Estimate tokens used by a list of messages.
pub fn estimate_context_tokens(messages: &[Message]) -> usize {
    messages
        .iter()
        .map(|m| {
            let json = serde_json::to_string(m).unwrap_or_default();
            estimate_tokens(&json)
        })
        .sum()
}

/// Check whether compaction should be triggered.
///
/// Compaction fires when the estimated context usage crosses
/// `threshold * max_context_tokens`. Both `threshold` and `max_context_tokens`
/// are read from the merged settings; the explicit `max_context_tokens`
/// argument lets callers override the model-window assumption (e.g. when the
/// active model is known to have a smaller window).
pub fn should_compact(
    context_tokens: usize,
    max_context_tokens: usize,
    settings: &CompactionSettings,
) -> bool {
    if !settings.enabled() {
        return false;
    }
    let trigger = (max_context_tokens as f32 * settings.threshold()) as usize;
    context_tokens >= trigger
}

/// Build a compaction summary prompt from messages.
pub fn build_compaction_prompt(messages: &[Message], file_ops: &FileOperations) -> String {
    let mut prompt = String::from(
        "Summarize the following conversation context concisely. \
         Focus on: key decisions made, files read/modified, important context for continuing.\n\n",
    );

    if !file_ops.read.is_empty() {
        prompt.push_str("Files read: ");
        prompt.push_str(&file_ops.read.join(", "));
        prompt.push('\n');
    }
    if !file_ops.edited.is_empty() {
        prompt.push_str("Files edited: ");
        prompt.push_str(&file_ops.edited.join(", "));
        prompt.push('\n');
    }

    prompt.push_str("\nConversation:\n");
    for msg in messages {
        match msg {
            Message::User(u) => {
                let text = match &u.content {
                    model::UserContent::Text(s) => s.clone(),
                    model::UserContent::Blocks(blocks) => blocks
                        .iter()
                        .filter_map(|c| match c {
                            model::UserContentBlock::Text(t) => Some(t.text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join(" "),
                };
                prompt.push_str(&format!("User: {}\n", text));
            }
            Message::Assistant(a) => {
                let text = a
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        model::AssistantContentBlock::Text(t) => Some(t.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                prompt.push_str(&format!("Assistant: {}\n", text));
            }
            Message::ToolResult(tr) => {
                prompt.push_str(&format!("Tool result ({}): ...\n", tr.tool_name));
            }
        }
    }

    prompt
}

/// Extract file operations from tool call messages.
pub fn extract_file_operations(messages: &[Message]) -> FileOperations {
    let mut ops = FileOperations::default();

    for msg in messages {
        if let Message::Assistant(a) = msg {
            for block in &a.content {
                if let model::AssistantContentBlock::ToolCall(tc) = block {
                    if let Some(path) = tc.arguments.get("path").and_then(|v| v.as_str()) {
                        match tc.name.as_str() {
                            "read" | "grep" | "find" | "ls" => {
                                if !ops.read.contains(&path.to_string()) {
                                    ops.read.push(path.to_string());
                                }
                            }
                            "write" | "edit" => {
                                if !ops.edited.contains(&path.to_string()) {
                                    ops.edited.push(path.to_string());
                                }
                            }
                            _ => {}
                        }
                    }
                    // Also check file_path for edit tool
                    if let Some(path) = tc.arguments.get("file_path").and_then(|v| v.as_str()) {
                        match tc.name.as_str() {
                            "edit" | "write" => {
                                if !ops.edited.contains(&path.to_string()) {
                                    ops.edited.push(path.to_string());
                                }
                            }
                            "read" => {
                                if !ops.read.contains(&path.to_string()) {
                                    ops.read.push(path.to_string());
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    ops
}

/// Select which messages to keep (most recent) and which to compact.
/// Returns (messages_to_compact, messages_to_keep, first_kept_index).
pub fn split_for_compaction(
    messages: &[Message],
    keep_recent_tokens: usize,
) -> (Vec<Message>, Vec<Message>, usize) {
    // Walk backwards to find how many recent messages fit in budget
    let mut kept_tokens = 0;
    let mut split_index = messages.len();

    for (i, msg) in messages.iter().enumerate().rev() {
        let json = serde_json::to_string(msg).unwrap_or_default();
        let msg_tokens = estimate_tokens(&json);
        if kept_tokens + msg_tokens > keep_recent_tokens {
            split_index = i + 1;
            break;
        }
        kept_tokens += msg_tokens;
        if i == 0 {
            split_index = 0;
        }
    }

    // Ensure we keep at least some messages
    if split_index >= messages.len() {
        split_index = messages.len().saturating_sub(2);
    }

    let to_compact = messages[..split_index].to_vec();
    let to_keep = messages[split_index..].to_vec();
    (to_compact, to_keep, split_index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::UserMessage;

    #[test]
    fn test_estimate_tokens() {
        assert!(estimate_tokens("hello world foo bar") >= 4);
        assert!(estimate_tokens("") == 0);
    }

    #[test]
    fn test_should_compact() {
        let settings = CompactionSettings::default();
        // With default threshold = 0.8, trigger at 160_000 of 200_000.
        assert!(should_compact(190_000, 200_000, &settings));
        assert!(!should_compact(100_000, 200_000, &settings));
    }

    #[test]
    fn test_should_compact_disabled() {
        let settings = CompactionSettings {
            enabled: Some(false),
            ..Default::default()
        };
        assert!(!should_compact(999_999, 200_000, &settings));
    }

    #[test]
    fn test_split_for_compaction() {
        let messages: Vec<Message> = (0..10)
            .map(|i| Message::User(UserMessage::new_text(format!("message {}", i))))
            .collect();

        let (to_compact, to_keep, _idx) = split_for_compaction(&messages, 999_999);
        // With a huge budget, nothing gets compacted (split at 0)
        assert!(to_compact.is_empty());
        assert_eq!(to_keep.len(), 10);
    }

    #[test]
    fn test_split_for_compaction_small_budget() {
        let messages: Vec<Message> = (0..10)
            .map(|i| Message::User(UserMessage::new_text(format!("message {}", i))))
            .collect();

        let (to_compact, to_keep, _idx) = split_for_compaction(&messages, 1);
        // With tiny budget, most get compacted
        assert!(!to_keep.is_empty());
        assert!(to_compact.len() + to_keep.len() == 10);
    }

    #[test]
    fn test_extract_file_operations() {
        use model::{AssistantContentBlock, AssistantMessage, StopReason, ToolCall};

        let tc = ToolCall {
            content_type: "tool_call".into(),
            id: "tc1".into(),
            name: "read".into(),
            arguments: serde_json::json!({"path": "/tmp/foo.rs"}),
            thought_signature: None,
        };
        let msg = Message::Assistant(AssistantMessage {
            role: "assistant".into(),
            content: vec![AssistantContentBlock::ToolCall(tc)],
            api: model::Api::AnthropicMessages,
            provider: model::types::Provider::Anthropic,
            model: String::new(),
            stop_reason: StopReason::ToolUse,
            usage: Default::default(),
            error_message: None,
            timestamp: 0,
        });

        let ops = extract_file_operations(&[msg]);
        assert_eq!(ops.read, vec!["/tmp/foo.rs"]);
        assert!(ops.edited.is_empty());
    }

    #[test]
    fn test_build_compaction_prompt() {
        let messages = vec![Message::User(UserMessage::new_text("hello"))];
        let ops = FileOperations {
            read: vec!["/tmp/foo.rs".into()],
            edited: vec![],
        };
        let prompt = build_compaction_prompt(&messages, &ops);
        assert!(prompt.contains("Files read: /tmp/foo.rs"));
        assert!(prompt.contains("User: hello"));
    }
}
