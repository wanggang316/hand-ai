//! Shared utilities for compaction and branch summarization.
//!
//! Provides `read` / `written` / `edited` `BTreeSet`-backed
//! file-operation tracking, deterministic XML formatting, and the
//! message serializer used by the summarizer prompts.
//!
//! The legacy public surface (`CompactionResult`, `should_compact`,
//! `split_for_compaction`, `build_compaction_prompt`, `extract_file_operations`,
//! and the simple `estimate_tokens` / `estimate_context_tokens` heuristics
//! consumed by [`crate::core::agent_session`]) lives here too so existing
//! callers continue to compile while the richer pipeline lands in
//! [`super::compactor`] and [`super::branch_summarization`].

use crate::core::settings::CompactionSettings;
use model::{
    AssistantContentBlock, CacheRetention, Message, SimpleStreamOptions, UserContent,
    UserContentBlock,
};
use std::collections::BTreeSet;

/// Stream options shared by every summarization request.
///
/// Summaries are one-shot: each one wraps a transcript that is never sent
/// again, so the prompt it caches can never be hit. Left to the default,
/// retention resolves to `Short` and the request is billed at the
/// provider's cache-write premium — Anthropic charges 25% over base
/// input — for a cache entry nobody reads. `CacheRetention::None`
/// suppresses the breakpoints so a summary is billed as plain input.
pub fn summarization_stream_options(max_tokens: u32) -> SimpleStreamOptions {
    let mut options = SimpleStreamOptions::default();
    options.base.max_tokens = Some(max_tokens);
    options.base.cache_retention = Some(CacheRetention::None);
    options
}

// ============================================================================
// CompactionResult (legacy) — kept verbatim until the entry-tree port lands.
// ============================================================================

/// Result of a compaction operation.
///
/// Legacy `agent_session`-facing shape; a richer details payload will
/// land alongside the entry-tree port.
#[derive(Debug, Clone)]
pub struct CompactionResult {
    /// Summary of compacted messages.
    pub summary: String,
    /// ID of the first message kept after compaction.
    pub first_kept_entry_id: String,
    /// Approximate token count before compaction.
    pub tokens_before: usize,
}

// ============================================================================
// File Operation Tracking
// ============================================================================

/// File operations tracked during compaction.
///
/// - `read`: files opened by `read` / `grep` / `find` / `ls`-style tools.
/// - `written`: files created/overwritten by a `write`-style tool.
/// - `edited`: files mutated by an `edit`-style tool.
///
/// `BTreeSet` is used (not `HashSet`) so the serialized summary is
/// deterministic across runs — the summarizer prompt is content-addressed
/// and stable ordering matters for caching.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileOperations {
    pub read: BTreeSet<String>,
    pub written: BTreeSet<String>,
    pub edited: BTreeSet<String>,
}

impl FileOperations {
    /// Create a new empty [`FileOperations`].
    pub fn new() -> Self {
        Self::default()
    }
}

/// Extract file operations from tool calls in a single assistant message.
///
/// Only inspects assistant messages, only tool-call blocks, and only
/// the `path` argument.
pub fn extract_file_ops_from_message(message: &Message, file_ops: &mut FileOperations) {
    let Message::Assistant(assistant) = message else {
        return;
    };

    for block in &assistant.content {
        let AssistantContentBlock::ToolCall(tc) = block else {
            continue;
        };

        let Some(path) = tc.arguments.get("path").and_then(|v| v.as_str()) else {
            continue;
        };

        match tc.name.as_str() {
            "read" => {
                file_ops.read.insert(path.to_string());
            }
            "write" => {
                file_ops.written.insert(path.to_string());
            }
            "edit" => {
                file_ops.edited.insert(path.to_string());
            }
            _ => {}
        }
    }
}

/// Compute the final read-only and modified file lists for the summary
/// preamble.
///
/// A file that was ever written or edited is considered modified; a file
/// only ever read is reported as read-only. Both lists are sorted (the
/// `BTreeSet` already guarantees this) and deduplicated.
pub fn compute_file_lists(file_ops: &FileOperations) -> (Vec<String>, Vec<String>) {
    let mut modified: BTreeSet<String> = BTreeSet::new();
    modified.extend(file_ops.edited.iter().cloned());
    modified.extend(file_ops.written.iter().cloned());

    let read_only: Vec<String> = file_ops
        .read
        .iter()
        .filter(|f| !modified.contains(f.as_str()))
        .cloned()
        .collect();

    let modified_files: Vec<String> = modified.into_iter().collect();
    (read_only, modified_files)
}

/// Format file operations as XML tags, suitable for appending to a
/// summary. Returns an empty string when both lists are empty so the
/// caller can blindly concatenate.
pub fn format_file_operations(read_files: &[String], modified_files: &[String]) -> String {
    let mut sections: Vec<String> = Vec::new();
    if !read_files.is_empty() {
        sections.push(format!(
            "<read-files>\n{}\n</read-files>",
            read_files.join("\n")
        ));
    }
    if !modified_files.is_empty() {
        sections.push(format!(
            "<modified-files>\n{}\n</modified-files>",
            modified_files.join("\n")
        ));
    }
    if sections.is_empty() {
        String::new()
    } else {
        format!("\n\n{}", sections.join("\n\n"))
    }
}

// ============================================================================
// Message Serialization
// ============================================================================

/// Maximum characters for a tool result in a serialized summary.
const TOOL_RESULT_MAX_CHARS: usize = 2000;

/// Truncate text to `max_chars`, appending a marker if anything was cut.
fn truncate_for_summary(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }
    // Slice on a UTF-8 char boundary at-or-before `max_chars` so we never
    // panic on multi-byte input. `floor_char_boundary` is unstable, so we
    // walk down from `max_chars` until we land on a boundary.
    let mut cut = max_chars.min(text.len());
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let kept = &text[..cut];
    let truncated_chars = text.len() - cut;
    format!("{kept}\n\n[... {truncated_chars} more characters truncated]")
}

/// Serialize a slice of `Message`s into a plain-text transcript suitable
/// for embedding inside a summarization prompt.
///
/// The TS reference operates on `LlmMessage`, which is its post-conversion
/// transport. In the Rust port the LLM-facing `model::Message` is already
/// the right shape, so we serialize directly.
pub fn serialize_conversation(messages: &[Message]) -> String {
    let mut parts: Vec<String> = Vec::new();

    for msg in messages {
        match msg {
            Message::User(u) => {
                let content = match &u.content {
                    UserContent::Text(s) => s.clone(),
                    UserContent::Blocks(blocks) => blocks
                        .iter()
                        .filter_map(|b| match b {
                            UserContentBlock::Text(t) => Some(t.text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join(""),
                };
                if !content.is_empty() {
                    parts.push(format!("[User]: {content}"));
                }
            }
            Message::Assistant(a) => {
                let mut text_parts: Vec<String> = Vec::new();
                let mut thinking_parts: Vec<String> = Vec::new();
                let mut tool_calls: Vec<String> = Vec::new();

                for block in &a.content {
                    match block {
                        AssistantContentBlock::Text(t) => text_parts.push(t.text.clone()),
                        AssistantContentBlock::Thinking(t) => {
                            thinking_parts.push(t.thinking.clone())
                        }
                        AssistantContentBlock::ToolCall(tc) => {
                            // TS does Object.entries(args).map(([k,v]) => `${k}=${JSON.stringify(v)}`).
                            // serde_json::Value's `as_object` gives us the same shape; for non-object
                            // arguments (rare but valid) we render the whole value.
                            let args_str = if let Some(map) = tc.arguments.as_object() {
                                map.iter()
                                    .map(|(k, v)| format!("{k}={v}"))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            } else {
                                tc.arguments.to_string()
                            };
                            tool_calls.push(format!("{}({})", tc.name, args_str));
                        }
                    }
                }

                if !thinking_parts.is_empty() {
                    parts.push(format!(
                        "[Assistant thinking]: {}",
                        thinking_parts.join("\n")
                    ));
                }
                if !text_parts.is_empty() {
                    parts.push(format!("[Assistant]: {}", text_parts.join("\n")));
                }
                if !tool_calls.is_empty() {
                    parts.push(format!("[Assistant tool calls]: {}", tool_calls.join("; ")));
                }
            }
            Message::ToolResult(tr) => {
                let content: String = tr
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        model::ToolResultContent::Text(t) => Some(t.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                if !content.is_empty() {
                    parts.push(format!(
                        "[Tool result]: {}",
                        truncate_for_summary(&content, TOOL_RESULT_MAX_CHARS)
                    ));
                }
            }
        }
    }

    parts.join("\n\n")
}

// ============================================================================
// Summarization System Prompt
// ============================================================================

/// System prompt installed for summarization calls.
pub const SUMMARIZATION_SYSTEM_PROMPT: &str = "You are a context summarization assistant. Your task is to read a conversation between a user and an AI coding assistant, then produce a structured summary following the exact format specified.

Do NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.";

// ============================================================================
// Legacy heuristics — kept until the message-aware estimator (compactor.rs)
// is adopted by `agent_session`. These match the previous Rust shape.
// ============================================================================

/// Estimate token count from a string (rough: ~4 chars per token).
pub fn estimate_tokens(text: &str) -> usize {
    text.split_whitespace().count().max(text.len() / 4)
}

/// Estimate tokens used by a list of messages.
///
/// Cheap heuristic: serialize each message to JSON and feed it to
/// [`estimate_tokens`]. Replaced for production paths by the message-aware
/// estimator in [`super::compactor`], but kept here so existing callers
/// (`agent_session::do_compact`) keep compiling.
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
/// `threshold * max_context_tokens`. This is the legacy gate; the
/// message-aware `should_compact` in [`super::compactor`] uses
/// `reserve_tokens` semantics (`context > window - reserve`).
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

/// Build a compaction summary prompt from messages (legacy path).
///
/// Used by `agent_session::do_compact` while the message-list-oriented
/// `generate_summary` in [`super::compactor`] is being rolled in.
pub fn build_compaction_prompt(messages: &[Message], file_ops: &FileOperations) -> String {
    build_compaction_prompt_with(messages, file_ops, None)
}

/// Variant of [`build_compaction_prompt`] that prepends caller-supplied
/// steering text — used by `/compact <custom instructions>` so the
/// summarizer keeps the user's focus (e.g. "preserve the database
/// schema decisions"). Empty / whitespace-only `custom_instructions`
/// behaves identically to the legacy form.
pub fn build_compaction_prompt_with(
    messages: &[Message],
    file_ops: &FileOperations,
    custom_instructions: Option<&str>,
) -> String {
    let mut prompt = String::from(
        "Summarize the following conversation context concisely. \
         Focus on: key decisions made, files read/modified, important context for continuing.\n\n",
    );

    if let Some(extra) = custom_instructions.map(str::trim).filter(|s| !s.is_empty()) {
        prompt.push_str("Additional user instructions for this summary: ");
        prompt.push_str(extra);
        prompt.push_str("\n\n");
    }

    if !file_ops.read.is_empty() {
        prompt.push_str("Files read: ");
        prompt.push_str(&file_ops.read.iter().cloned().collect::<Vec<_>>().join(", "));
        prompt.push('\n');
    }
    let modified: Vec<&str> = file_ops
        .edited
        .iter()
        .chain(file_ops.written.iter())
        .map(String::as_str)
        .collect();
    if !modified.is_empty() {
        prompt.push_str("Files edited: ");
        prompt.push_str(&modified.join(", "));
        prompt.push('\n');
    }

    prompt.push_str("\nConversation:\n");
    for msg in messages {
        match msg {
            Message::User(u) => {
                let text = match &u.content {
                    UserContent::Text(s) => s.clone(),
                    UserContent::Blocks(blocks) => blocks
                        .iter()
                        .filter_map(|c| match c {
                            UserContentBlock::Text(t) => Some(t.text.as_str()),
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
                        AssistantContentBlock::Text(t) => Some(t.text.as_str()),
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

/// Extract file operations from a slice of messages.
///
/// Built on top of the per-message helper. Only the canonical
/// `read` / `write` / `edit` tool names are recognised here; the
/// higher-level tool routing is responsible for normalising any
/// aliases (`grep`, `find`, `ls`) before they reach this layer.
pub fn extract_file_operations(messages: &[Message]) -> FileOperations {
    let mut ops = FileOperations::default();
    for msg in messages {
        extract_file_ops_from_message(msg, &mut ops);
    }
    ops
}

/// Select which messages to keep (most recent) and which to compact.
/// Returns `(messages_to_compact, messages_to_keep, first_kept_index)`.
pub fn split_for_compaction(
    messages: &[Message],
    keep_recent_tokens: usize,
) -> (Vec<Message>, Vec<Message>, usize) {
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
    use model::types::Provider;
    use model::{
        AssistantContentBlock, AssistantMessage, StopReason, TextContent, ToolCall, UserMessage,
    };

    fn assistant_with(blocks: Vec<AssistantContentBlock>) -> Message {
        Message::Assistant(AssistantMessage {
            role: "assistant".into(),
            content: blocks,
            api: model::Api::AnthropicMessages,
            provider: Provider::Anthropic,
            model: String::new(),
            response_model: None,
            response_id: None,
            diagnostics: None,
            stop_reason: StopReason::ToolUse,
            raw_stop_reason: None,
            usage: Default::default(),
            error_message: None,
            timestamp: 0,
        })
    }

    fn tool_call(name: &str, args: serde_json::Value) -> AssistantContentBlock {
        AssistantContentBlock::ToolCall(ToolCall {
            content_type: "tool_call".into(),
            id: format!("tc-{name}"),
            name: name.into(),
            arguments: args,
            thought_signature: None,
        })
    }

    // ---- estimate_tokens / should_compact (legacy heuristics) ----

    #[test]
    fn estimate_tokens_basic() {
        assert!(estimate_tokens("hello world foo bar") >= 4);
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn should_compact_above_threshold() {
        let settings = CompactionSettings::default();
        assert!(should_compact(190_000, 200_000, &settings));
        assert!(!should_compact(100_000, 200_000, &settings));
    }

    #[test]
    fn should_compact_disabled_short_circuits() {
        let settings = CompactionSettings {
            enabled: Some(false),
            ..Default::default()
        };
        assert!(!should_compact(999_999, 200_000, &settings));
    }

    // ---- FileOperations / extract_file_ops_from_message ----

    #[test]
    fn extract_file_ops_classifies_read_write_edit() {
        let msg = assistant_with(vec![
            tool_call("read", serde_json::json!({"path": "a.rs"})),
            tool_call("write", serde_json::json!({"path": "b.rs"})),
            tool_call("edit", serde_json::json!({"path": "c.rs"})),
        ]);

        let mut ops = FileOperations::new();
        extract_file_ops_from_message(&msg, &mut ops);

        assert_eq!(ops.read, BTreeSet::from(["a.rs".to_string()]));
        assert_eq!(ops.written, BTreeSet::from(["b.rs".to_string()]));
        assert_eq!(ops.edited, BTreeSet::from(["c.rs".to_string()]));
    }

    #[test]
    fn extract_file_ops_ignores_unknown_tools_and_missing_path() {
        let msg = assistant_with(vec![
            tool_call("grep", serde_json::json!({"path": "ignored.rs"})),
            tool_call("read", serde_json::json!({"file_path": "no-path-key.rs"})),
        ]);
        let mut ops = FileOperations::new();
        extract_file_ops_from_message(&msg, &mut ops);
        assert!(ops.read.is_empty());
        assert!(ops.written.is_empty());
        assert!(ops.edited.is_empty());
    }

    #[test]
    fn extract_file_ops_skips_non_assistant_messages() {
        let msg = Message::User(UserMessage::new_text("hi"));
        let mut ops = FileOperations::new();
        extract_file_ops_from_message(&msg, &mut ops);
        assert!(ops.read.is_empty());
    }

    #[test]
    fn extract_file_ops_dedupes_repeats() {
        let msg = assistant_with(vec![
            tool_call("read", serde_json::json!({"path": "a.rs"})),
            tool_call("read", serde_json::json!({"path": "a.rs"})),
        ]);
        let mut ops = FileOperations::new();
        extract_file_ops_from_message(&msg, &mut ops);
        assert_eq!(ops.read.len(), 1);
    }

    // ---- compute_file_lists ----

    #[test]
    fn compute_file_lists_excludes_modified_from_read_only() {
        let mut ops = FileOperations::new();
        ops.read.insert("readme.md".into());
        ops.read.insert("shared.rs".into());
        ops.edited.insert("shared.rs".into());
        ops.written.insert("new.rs".into());

        let (read_only, modified) = compute_file_lists(&ops);
        assert_eq!(read_only, vec!["readme.md".to_string()]);
        assert_eq!(
            modified,
            vec!["new.rs".to_string(), "shared.rs".to_string()]
        );
    }

    #[test]
    fn compute_file_lists_handles_empty() {
        let (read_only, modified) = compute_file_lists(&FileOperations::default());
        assert!(read_only.is_empty());
        assert!(modified.is_empty());
    }

    // ---- format_file_operations ----

    #[test]
    fn format_file_operations_returns_empty_when_no_files() {
        assert_eq!(format_file_operations(&[], &[]), "");
    }

    #[test]
    fn format_file_operations_emits_xml_tags() {
        let out = format_file_operations(&["a.rs".into(), "b.rs".into()], &["c.rs".into()]);
        assert!(out.starts_with("\n\n"));
        assert!(out.contains("<read-files>\na.rs\nb.rs\n</read-files>"));
        assert!(out.contains("<modified-files>\nc.rs\n</modified-files>"));
    }

    #[test]
    fn format_file_operations_separates_sections_with_blank_line() {
        let out = format_file_operations(&["a".into()], &["b".into()]);
        assert!(out.contains("</read-files>\n\n<modified-files>"));
    }

    // ---- serialize_conversation ----

    #[test]
    fn serialize_conversation_preserves_roles() {
        let messages = vec![
            Message::User(UserMessage::new_text("hello")),
            assistant_with(vec![AssistantContentBlock::Text(TextContent::new(
                "hi back",
            ))]),
        ];
        let out = serialize_conversation(&messages);
        assert!(out.contains("[User]: hello"));
        assert!(out.contains("[Assistant]: hi back"));
        // Sections separated by blank line.
        assert!(out.contains("\n\n"));
    }

    #[test]
    fn serialize_conversation_renders_tool_calls() {
        let msg = assistant_with(vec![tool_call(
            "read",
            serde_json::json!({"path": "/tmp/x"}),
        )]);
        let out = serialize_conversation(&[msg]);
        assert!(out.contains("[Assistant tool calls]: read("));
        assert!(out.contains("path=\"/tmp/x\""));
    }

    #[test]
    fn serialize_conversation_skips_empty_user_content() {
        let msg = Message::User(UserMessage::new_text(""));
        assert_eq!(serialize_conversation(&[msg]), "");
    }

    #[test]
    fn serialize_conversation_truncates_long_tool_results() {
        use model::ToolResultContent;
        let huge = "x".repeat(TOOL_RESULT_MAX_CHARS + 50);
        let msg = Message::ToolResult(model::ToolResultMessage::new(
            "id-1",
            "read",
            vec![ToolResultContent::Text(TextContent::new(huge))],
        ));
        let out = serialize_conversation(&[msg]);
        assert!(out.contains("[... 50 more characters truncated]"));
        assert!(out.len() < TOOL_RESULT_MAX_CHARS + 200);
    }

    #[test]
    fn truncate_for_summary_handles_utf8_boundary() {
        // Each '中' is 3 bytes in UTF-8. With max_chars=4 we land mid-char and must back up.
        let s = "中中"; // 6 bytes
        let out = truncate_for_summary(s, 4);
        assert!(out.starts_with("中"));
        assert!(out.contains("more characters truncated"));
    }

    // ---- legacy compatibility ----

    #[test]
    fn extract_file_operations_aggregates_across_messages() {
        let msg_a = assistant_with(vec![tool_call("read", serde_json::json!({"path": "a"}))]);
        let msg_b = assistant_with(vec![tool_call("edit", serde_json::json!({"path": "b"}))]);
        let ops = extract_file_operations(&[msg_a, msg_b]);
        assert_eq!(ops.read, BTreeSet::from(["a".to_string()]));
        assert_eq!(ops.edited, BTreeSet::from(["b".to_string()]));
    }

    #[test]
    fn split_for_compaction_with_huge_budget_keeps_all() {
        let messages: Vec<Message> = (0..10)
            .map(|i| Message::User(UserMessage::new_text(format!("message {}", i))))
            .collect();
        let (to_compact, to_keep, _idx) = split_for_compaction(&messages, 999_999);
        assert!(to_compact.is_empty());
        assert_eq!(to_keep.len(), 10);
    }

    #[test]
    fn split_for_compaction_with_tiny_budget_compacts_most() {
        let messages: Vec<Message> = (0..10)
            .map(|i| Message::User(UserMessage::new_text(format!("message {}", i))))
            .collect();
        let (to_compact, to_keep, _idx) = split_for_compaction(&messages, 1);
        assert!(!to_keep.is_empty());
        assert_eq!(to_compact.len() + to_keep.len(), 10);
    }

    /// A context-visible custom message reaches the model context as a
    /// plain user message via `convert_to_llm` — the same projection the
    /// context assembly uses. Once projected, its tokens must count
    /// toward the keep-recent budget exactly like any other context
    /// message, shifting the cut point instead of being skipped as
    /// zero-cost metadata.
    #[test]
    fn split_for_compaction_counts_context_visible_custom_messages() {
        use crate::core::messages::{
            AgentMessage, CustomMessageContent, convert_to_llm, create_custom_message,
        };

        let plain: Vec<Message> = (0..4)
            .map(|i| Message::User(UserMessage::new_text(format!("plain message {i}"))))
            .collect();

        // Baseline: four tiny messages fit the budget — nothing compacts.
        let (to_compact, to_keep, split_idx) = split_for_compaction(&plain, 1_000);
        assert!(to_compact.is_empty());
        assert_eq!(to_keep.len(), 4);
        assert_eq!(split_idx, 0);

        // Inject a large context-visible custom message (~2000 tokens),
        // projected through the real entry→context conversion.
        let custom = AgentMessage::Custom(create_custom_message(
            "extension/status",
            CustomMessageContent::Text("x".repeat(8_000)),
            true,
            None,
            "1970-01-01T00:00:01Z",
        ));
        let projected = convert_to_llm(std::slice::from_ref(&custom));
        assert_eq!(projected.len(), 1, "custom message must project to context");

        let mut messages = plain;
        messages.insert(3, projected[0].clone());

        // Same budget: the projected custom message blows the keep-recent
        // budget, so the cut point moves past it — only the final plain
        // message stays, and the custom message lands in the compacted
        // (summarized) prefix.
        let (to_compact, to_keep, split_idx) = split_for_compaction(&messages, 1_000);
        assert_eq!(split_idx, 4);
        assert_eq!(to_keep.len(), 1);
        assert_eq!(to_compact.len(), 4);
    }

    #[test]
    fn build_compaction_prompt_lists_files_and_messages() {
        let messages = vec![Message::User(UserMessage::new_text("hello"))];
        let mut ops = FileOperations::default();
        ops.read.insert("/tmp/foo.rs".into());
        let prompt = build_compaction_prompt(&messages, &ops);
        assert!(prompt.contains("Files read: /tmp/foo.rs"));
        assert!(prompt.contains("User: hello"));
    }

    /// Regression for #46: `/compact <custom>` must forward the user's
    /// steering text into the summary prompt rather than silently
    /// dropping it. Whitespace-only steering still behaves as the
    /// no-custom legacy form so callers don't accidentally inject a
    /// blank "Additional user instructions" block.
    #[test]
    fn build_compaction_prompt_with_includes_custom_instructions() {
        let messages = vec![Message::User(UserMessage::new_text("hello"))];
        let ops = FileOperations::default();
        let prompt = build_compaction_prompt_with(
            &messages,
            &ops,
            Some("focus on the database schema changes"),
        );
        assert!(prompt.contains("Additional user instructions"));
        assert!(prompt.contains("focus on the database schema changes"));

        // Whitespace-only steering must collapse to the legacy prompt.
        let prompt_blank = build_compaction_prompt_with(&messages, &ops, Some("   \t  "));
        let prompt_none = build_compaction_prompt(&messages, &ops);
        assert_eq!(prompt_blank, prompt_none);
    }

    #[test]
    fn build_compaction_prompt_merges_edited_and_written() {
        let mut ops = FileOperations::default();
        ops.edited.insert("e.rs".into());
        ops.written.insert("w.rs".into());
        let prompt = build_compaction_prompt(&[], &ops);
        assert!(prompt.contains("Files edited:"));
        assert!(prompt.contains("e.rs"));
        assert!(prompt.contains("w.rs"));
    }
}
