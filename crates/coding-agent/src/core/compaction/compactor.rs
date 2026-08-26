//! Compaction pipeline — message-list path.
//!
//! Produces the summary that lets a long session keep running without
//! overflowing the model's context window.
//!
//! The current [`crate::core::session_manager::SessionEntry`] does not
//! yet model `branch_summary` / `custom_message` / `bash_execution` /
//! `thinking_level_change` variants or a parent-id tree, so the
//! entry-tree-walking helpers (cut-point selection over previous
//! compaction entries, turn-start projection, etc.) are deferred. This
//! module ships the message-list half:
//!
//! - Token math: [`calculate_context_tokens`], [`estimate_tokens_for_message`],
//!   [`estimate_context_tokens_with_usage`], [`get_last_assistant_usage`].
//! - Threshold gate: [`should_compact_with_reserve`] (a `reserve_tokens`
//!   policy distinct from the legacy threshold gate in
//!   [`super::utils::should_compact`]).
//! - LLM-driven summarization: [`generate_summary`],
//!   [`generate_turn_prefix_summary`], [`compact`] — all routed through
//!   the [`super::branch_summarization::SummarizationClient`] trait so
//!   tests can mock the network.

use crate::core::compaction::branch_summarization::SummarizationClient;
use crate::core::compaction::utils::{
    FileOperations, SUMMARIZATION_SYSTEM_PROMPT, compute_file_lists, format_file_operations,
    serialize_conversation, summarization_stream_options,
};
use model::{
    AssistantContent as AssistantContentBlock, AssistantMessage, Context, Message, Model,
    StopReason, ThinkingLevel, Usage, UserMessage,
};
use std::sync::Arc;

// ============================================================================
// Settings
// ============================================================================

/// Runtime settings used by the message-list compactor.
///
/// Distinct from the project-level
/// [`crate::core::settings::CompactionSettings`], whose `threshold`
/// semantics are kept for backwards compatibility with the legacy
/// [`super::utils::should_compact`] gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionRuntimeSettings {
    pub enabled: bool,
    pub reserve_tokens: u64,
    pub keep_recent_tokens: u64,
}

impl Default for CompactionRuntimeSettings {
    /// Ship the default policy: enabled, 16 384-token reserve,
    /// 20 000-token keep-recent budget.
    fn default() -> Self {
        Self {
            enabled: true,
            reserve_tokens: 16_384,
            keep_recent_tokens: 20_000,
        }
    }
}

// ============================================================================
// CompactionDetails (persisted file lists)
// ============================================================================

/// Persistable file-tracking payload. Consumed by the entry-tree
/// port when it lands.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CompactionDetails {
    #[serde(rename = "readFiles", default)]
    pub read_files: Vec<String>,
    #[serde(rename = "modifiedFiles", default)]
    pub modified_files: Vec<String>,
}

// ============================================================================
// Token math
// ============================================================================

/// Calculate total context tokens from a [`Usage`] record.
///
/// Uses the native `total_tokens` field when available, otherwise falls
/// back to summing the components.
pub fn calculate_context_tokens(usage: &Usage) -> u64 {
    if usage.total_tokens > 0 {
        usage.total_tokens
    } else {
        usage.input + usage.output + usage.cache_read + usage.cache_write
    }
}

/// Pull the [`Usage`] record off an assistant message, but only when
/// the message is "good" — aborted and error messages carry unreliable
/// usage data and are skipped.
fn assistant_usage(msg: &Message) -> Option<&Usage> {
    let Message::Assistant(a) = msg else {
        return None;
    };
    if matches!(a.stop_reason, StopReason::Aborted | StopReason::Error) {
        return None;
    }
    Some(&a.usage)
}

/// Find the last non-aborted assistant message's [`Usage`] in a slice.
pub fn get_last_assistant_usage(messages: &[Message]) -> Option<&Usage> {
    for msg in messages.iter().rev() {
        if let Some(u) = assistant_usage(msg) {
            return Some(u);
        }
    }
    None
}

fn last_assistant_usage_info(messages: &[Message]) -> Option<(usize, &Usage)> {
    for (i, msg) in messages.iter().enumerate().rev() {
        if let Some(u) = assistant_usage(msg) {
            return Some((i, u));
        }
    }
    None
}

/// Estimate token count for a single message using a chars/4 heuristic.
///
/// Conservative — overestimates tokens — so the reserve gate fires a
/// little early rather than overshooting the context window. Tool
/// results and images are billed at fixed-size approximations.
pub fn estimate_tokens_for_message(message: &Message) -> u64 {
    let mut chars: u64 = 0;
    match message {
        Message::User(u) => match &u.content {
            model::UserContent::Text(s) => chars += s.len() as u64,
            model::UserContent::Blocks(blocks) => {
                for b in blocks {
                    match b {
                        model::UserContentBlock::Text(t) => chars += t.text.len() as u64,
                        model::UserContentBlock::Image(_) => chars += 4_800,
                    }
                }
            }
        },
        Message::Assistant(a) => {
            for block in &a.content {
                match block {
                    AssistantContentBlock::Text(t) => chars += t.text.len() as u64,
                    AssistantContentBlock::Thinking(t) => chars += t.thinking.len() as u64,
                    AssistantContentBlock::ToolCall(tc) => {
                        chars += tc.name.len() as u64;
                        chars += tc.arguments.to_string().len() as u64;
                    }
                }
            }
        }
        Message::ToolResult(tr) => {
            for block in &tr.content {
                match block {
                    model::ToolResultContent::Text(t) => chars += t.text.len() as u64,
                    model::ToolResultContent::Image(_) => chars += 4_800,
                }
            }
        }
    }
    chars.div_ceil(4)
}

/// Combined context-tokens estimate.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextUsageEstimate {
    /// Best-effort total: `usage_tokens + trailing_tokens` when usage is
    /// available, otherwise pure heuristic.
    pub tokens: u64,
    /// Tokens reported by the last good assistant usage (0 when none).
    pub usage_tokens: u64,
    /// Heuristic tokens for everything *after* the last usage point.
    pub trailing_tokens: u64,
    /// Index of the last usage-bearing message, if any.
    pub last_usage_index: Option<usize>,
}

/// Estimate context tokens using the last assistant usage as an anchor.
///
/// When a recent assistant message has good usage data, only the
/// messages after it are scored heuristically. Otherwise every message
/// is scored.
pub fn estimate_context_tokens_with_usage(messages: &[Message]) -> ContextUsageEstimate {
    match last_assistant_usage_info(messages) {
        Some((idx, usage)) => {
            let usage_tokens = calculate_context_tokens(usage);
            let trailing_tokens: u64 = messages
                .iter()
                .skip(idx + 1)
                .map(estimate_tokens_for_message)
                .sum();
            ContextUsageEstimate {
                tokens: usage_tokens + trailing_tokens,
                usage_tokens,
                trailing_tokens,
                last_usage_index: Some(idx),
            }
        }
        None => {
            let total: u64 = messages.iter().map(estimate_tokens_for_message).sum();
            ContextUsageEstimate {
                tokens: total,
                usage_tokens: 0,
                trailing_tokens: total,
                last_usage_index: None,
            }
        }
    }
}

/// Decide whether compaction should fire under the `reserve_tokens`
/// policy: trigger when `context_tokens > context_window - reserve`.
///
/// Distinct from the legacy [`super::utils::should_compact`] gate, which
/// uses a `threshold` ratio and is still consumed by `agent_session`
/// pending the cut-over.
pub fn should_compact_with_reserve(
    context_tokens: u64,
    context_window: u64,
    settings: &CompactionRuntimeSettings,
) -> bool {
    if !settings.enabled {
        return false;
    }
    let limit = context_window.saturating_sub(settings.reserve_tokens);
    context_tokens > limit
}

// ============================================================================
// Summarization prompts
// ============================================================================

const SUMMARIZATION_PROMPT: &str = r#"The messages above are a conversation to summarize. Create a structured context checkpoint summary that another LLM will use to continue the work.

Use this EXACT format:

## Goal
[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned by user]
- [Or "(none)" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Current work]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [Ordered list of what should happen next]

## Critical Context
- [Any data, examples, or references needed to continue]
- [Or "(none)" if not applicable]

Keep each section concise. Preserve exact file paths, function names, and error messages."#;

const UPDATE_SUMMARIZATION_PROMPT: &str = r#"The messages above are NEW conversation messages to incorporate into the existing summary provided in <previous-summary> tags.

Update the existing structured summary with new information. RULES:
- PRESERVE all existing information from the previous summary
- ADD new progress, decisions, and context from the new messages
- UPDATE the Progress section: move items from "In Progress" to "Done" when completed
- UPDATE "Next Steps" based on what was accomplished
- PRESERVE exact file paths, function names, and error messages
- If something is no longer relevant, you may remove it

Use this EXACT format:

## Goal
[Preserve existing goals, add new ones if the task expanded]

## Constraints & Preferences
- [Preserve existing, add new ones discovered]

## Progress
### Done
- [x] [Include previously done items AND newly completed items]

### In Progress
- [ ] [Current work - update based on progress]

### Blocked
- [Current blockers - remove if resolved]

## Key Decisions
- **[Decision]**: [Brief rationale] (preserve all previous, add new)

## Next Steps
1. [Update based on current state]

## Critical Context
- [Preserve important context, add new if needed]

Keep each section concise. Preserve exact file paths, function names, and error messages."#;

const TURN_PREFIX_SUMMARIZATION_PROMPT: &str = r#"This is the PREFIX of a turn that was too large to keep. The SUFFIX (recent work) is retained.

Summarize the prefix to provide context for the retained suffix:

## Original Request
[What did the user ask for in this turn?]

## Early Progress
- [Key decisions and work done in the prefix]

## Context for Suffix
- [Information needed to understand the retained recent work]

Be concise. Focus on what's needed to understand the kept suffix."#;

// ============================================================================
// LLM-driven summarization
// ============================================================================

/// Generate a summary of the given conversation slice, optionally merging
/// with a previous summary (uses the update-summarization prompt when
/// `previous_summary` is `Some`).
///
/// Routes through the [`SummarizationClient`] trait so tests can stub the
/// network.
pub async fn generate_summary(
    messages: &[Message],
    model: &Model,
    reserve_tokens: u64,
    client: Arc<dyn SummarizationClient>,
    custom_instructions: Option<&str>,
    previous_summary: Option<&str>,
    thinking_level: Option<ThinkingLevel>,
) -> Result<String, String> {
    // 80% of the reserve budget goes to the response; the remaining
    // 20% is left as headroom for the prompt itself.
    let max_tokens = ((reserve_tokens as f64) * 0.8).floor() as u32;

    let mut base_prompt = if previous_summary.is_some() {
        UPDATE_SUMMARIZATION_PROMPT.to_string()
    } else {
        SUMMARIZATION_PROMPT.to_string()
    };
    if let Some(extra) = custom_instructions {
        base_prompt = format!("{base_prompt}\n\nAdditional focus: {extra}");
    }

    let conversation_text = serialize_conversation(messages);
    let mut prompt_text = format!("<conversation>\n{conversation_text}\n</conversation>\n\n");
    if let Some(prev) = previous_summary {
        prompt_text.push_str(&format!(
            "<previous-summary>\n{prev}\n</previous-summary>\n\n"
        ));
    }
    prompt_text.push_str(&base_prompt);

    let context = Context {
        system_prompt: Some(SUMMARIZATION_SYSTEM_PROMPT.to_string()),
        messages: vec![Message::User(UserMessage::new_text(prompt_text))],
        tools: None,
    };

    let mut options = summarization_stream_options(max_tokens);
    // Only forward `reasoning` when the model supports it AND the
    // caller passed a non-"off" level. `ThinkingLevel` has no `Off`
    // variant — absence is encoded as `None` — so a `Some` value
    // here is always meaningful.
    if let Some(level) = thinking_level.filter(|_| model.reasoning) {
        options.reasoning = Some(level);
    }

    let response = client.complete(model, context, options).await?;
    if matches!(response.stop_reason, StopReason::Error) {
        return Err(format!(
            "Summarization failed: {}",
            response
                .error_message
                .unwrap_or_else(|| "Unknown error".to_string())
        ));
    }

    Ok(extract_text(&response))
}

/// Generate a summary for a turn prefix when the cut point splits a turn.
/// Uses half the reserve budget — the turn prefix is a smaller chunk
/// than the full conversation.
pub async fn generate_turn_prefix_summary(
    messages: &[Message],
    model: &Model,
    reserve_tokens: u64,
    client: Arc<dyn SummarizationClient>,
    thinking_level: Option<ThinkingLevel>,
) -> Result<String, String> {
    let max_tokens = ((reserve_tokens as f64) * 0.5).floor() as u32;

    let conversation_text = serialize_conversation(messages);
    let prompt_text = format!(
        "<conversation>\n{conversation_text}\n</conversation>\n\n{TURN_PREFIX_SUMMARIZATION_PROMPT}",
    );

    let context = Context {
        system_prompt: Some(SUMMARIZATION_SYSTEM_PROMPT.to_string()),
        messages: vec![Message::User(UserMessage::new_text(prompt_text))],
        tools: None,
    };

    let mut options = summarization_stream_options(max_tokens);
    // Only forward `reasoning` when the model supports it AND the
    // caller passed a non-"off" level. `ThinkingLevel` has no `Off`
    // variant — absence is encoded as `None` — so a `Some` value
    // here is always meaningful.
    if let Some(level) = thinking_level.filter(|_| model.reasoning) {
        options.reasoning = Some(level);
    }

    let response = client.complete(model, context, options).await?;
    if matches!(response.stop_reason, StopReason::Error) {
        return Err(format!(
            "Turn prefix summarization failed: {}",
            response
                .error_message
                .unwrap_or_else(|| "Unknown error".to_string())
        ));
    }

    Ok(extract_text(&response))
}

/// Already-prepared inputs for [`compact`]. The entry-tree fields
/// (`first_kept_entry_id`, `tokens_before`) are produced by the
/// entry-tree port and re-attached when [`compact`] returns.
#[derive(Debug, Clone)]
pub struct CompactionInput {
    /// Messages that will be summarized and discarded.
    pub messages_to_summarize: Vec<Message>,
    /// Messages that will be summarized as a turn prefix (if splitting a
    /// turn). Empty when [`Self::is_split_turn`] is false.
    pub turn_prefix_messages: Vec<Message>,
    /// Whether this compaction splits a turn mid-flight.
    pub is_split_turn: bool,
    /// Summary from a previous compaction, for iterative update.
    pub previous_summary: Option<String>,
    /// File operations extracted from `messages_to_summarize`
    /// (and `turn_prefix_messages` when splitting).
    pub file_ops: FileOperations,
    pub settings: CompactionRuntimeSettings,
}

/// Output of [`compact`]: the merged summary plus the `read_files` /
/// `modified_files` lists derived from the input file operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionOutput {
    pub summary: String,
    pub read_files: Vec<String>,
    pub modified_files: Vec<String>,
}

/// Run the compaction pipeline against an already-prepared input.
///
/// When [`CompactionInput::is_split_turn`] is `true` and the turn prefix
/// is non-empty, the history and turn-prefix summaries are produced
/// concurrently with [`tokio::join`] and stitched together with the
/// standard separator. The file-operations XML is appended to the final
/// summary.
pub async fn compact(
    input: CompactionInput,
    model: &Model,
    client: Arc<dyn SummarizationClient>,
    custom_instructions: Option<&str>,
    thinking_level: Option<ThinkingLevel>,
) -> Result<CompactionOutput, String> {
    let CompactionInput {
        messages_to_summarize,
        turn_prefix_messages,
        is_split_turn,
        previous_summary,
        file_ops,
        settings,
    } = input;

    let mut summary = if is_split_turn && !turn_prefix_messages.is_empty() {
        let history_fut = async {
            if messages_to_summarize.is_empty() {
                Ok::<String, String>("No prior history.".to_string())
            } else {
                generate_summary(
                    &messages_to_summarize,
                    model,
                    settings.reserve_tokens,
                    client.clone(),
                    custom_instructions,
                    previous_summary.as_deref(),
                    thinking_level,
                )
                .await
            }
        };
        let prefix_fut = generate_turn_prefix_summary(
            &turn_prefix_messages,
            model,
            settings.reserve_tokens,
            client.clone(),
            thinking_level,
        );
        let (history_res, prefix_res) = tokio::join!(history_fut, prefix_fut);
        let history = history_res?;
        let prefix = prefix_res?;
        format!("{history}\n\n---\n\n**Turn Context (split turn):**\n\n{prefix}")
    } else {
        generate_summary(
            &messages_to_summarize,
            model,
            settings.reserve_tokens,
            client.clone(),
            custom_instructions,
            previous_summary.as_deref(),
            thinking_level,
        )
        .await?
    };

    let (read_files, modified_files) = compute_file_lists(&file_ops);
    summary.push_str(&format_file_operations(&read_files, &modified_files));

    Ok(CompactionOutput {
        summary,
        read_files,
        modified_files,
    })
}

/// Concatenate the text content of an [`AssistantMessage`].
fn extract_text(msg: &AssistantMessage) -> String {
    msg.content
        .iter()
        .filter_map(|b| match b {
            AssistantContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ============================================================================
// Entry-tree path — placeholders.
// ============================================================================

// TODO: the entry-tree path (preparing compaction, finding cut points,
// projecting entries to messages, and tracking file operations across
// previous compaction entries) requires extending
// `crate::core::session_manager::SessionEntry` with `branch_summary`,
// `custom_message`, `bash_execution`, and `thinking_level_change`
// variants plus a `parent_id` tree.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::compaction::branch_summarization::SummarizationClient;
    use async_trait::async_trait;
    use model::types::Provider;
    use model::{
        Api, AssistantContentBlock, AssistantMessage, Cost, SimpleStreamOptions, StopReason,
        TextContent, ThinkingLevel, ToolCall, ToolResultContent, ToolResultMessage, Usage,
        UserMessage,
    };
    use std::sync::Mutex;

    // ---- shared test helpers ----

    fn dummy_model(reasoning: bool) -> Model {
        Model {
            id: "test".into(),
            name: "Test".into(),
            api: Api::AnthropicMessages,
            provider: Provider::Anthropic,
            base_url: String::new(),
            reasoning,
            input: vec![],
            cost: Cost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_window: 200_000,
            max_tokens: 8192,
            headers: None,
            compat: None,
            thinking_level_map: None,
        }
    }

    fn assistant_msg(text: &str, usage: Usage, stop_reason: StopReason) -> Message {
        Message::Assistant(AssistantMessage {
            role: "assistant".into(),
            content: vec![AssistantContentBlock::Text(TextContent::new(text))],
            api: Api::AnthropicMessages,
            provider: Provider::Anthropic,
            model: "test".into(),
            usage,
            stop_reason,
            raw_stop_reason: None,
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        })
    }

    fn assistant_with_tool_call(name: &str, args: serde_json::Value) -> Message {
        Message::Assistant(AssistantMessage {
            role: "assistant".into(),
            content: vec![AssistantContentBlock::ToolCall(ToolCall {
                content_type: "tool_call".into(),
                id: "tc".into(),
                name: name.into(),
                arguments: args,
                thought_signature: None,
            })],
            api: Api::AnthropicMessages,
            provider: Provider::Anthropic,
            model: "test".into(),
            usage: Usage::default(),
            stop_reason: StopReason::ToolUse,
            raw_stop_reason: None,
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        })
    }

    /// Recording stub: returns a queue of pre-canned responses in order
    /// and records every call.
    struct ScriptedClient {
        responses: Mutex<Vec<Result<AssistantMessage, String>>>,
        calls: Mutex<Vec<SimpleStreamOptions>>,
    }

    impl ScriptedClient {
        fn new(responses: Vec<Result<AssistantMessage, String>>) -> Arc<Self> {
            Arc::new(Self {
                responses: Mutex::new(responses),
                calls: Mutex::new(Vec::new()),
            })
        }

        fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    #[async_trait]
    impl SummarizationClient for ScriptedClient {
        async fn complete(
            &self,
            _model: &Model,
            _context: Context,
            options: SimpleStreamOptions,
        ) -> Result<AssistantMessage, String> {
            self.calls.lock().unwrap().push(options);
            let mut q = self.responses.lock().unwrap();
            assert!(!q.is_empty(), "no scripted response left");
            q.remove(0)
        }
    }

    fn ok_assistant(text: &str) -> Result<AssistantMessage, String> {
        Ok(AssistantMessage {
            role: "assistant".into(),
            content: vec![AssistantContentBlock::Text(TextContent::new(text))],
            api: Api::AnthropicMessages,
            provider: Provider::Anthropic,
            model: "test".into(),
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            raw_stop_reason: None,
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        })
    }

    fn err_assistant(text: &str) -> Result<AssistantMessage, String> {
        Ok(AssistantMessage {
            role: "assistant".into(),
            content: vec![AssistantContentBlock::Text(TextContent::new(""))],
            api: Api::AnthropicMessages,
            provider: Provider::Anthropic,
            model: "test".into(),
            usage: Usage::default(),
            stop_reason: StopReason::Error,
            raw_stop_reason: None,
            error_message: Some(text.into()),
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        })
    }

    // ---- token math ----

    #[test]
    fn calculate_context_tokens_prefers_total() {
        let usage = Usage {
            input: 100,
            output: 200,
            cache_read: 50,
            cache_write: 0,
            total_tokens: 1234,
            ..Default::default()
        };
        assert_eq!(calculate_context_tokens(&usage), 1234);
    }

    #[test]
    fn calculate_context_tokens_falls_back_to_components() {
        let usage = Usage {
            input: 100,
            output: 200,
            cache_read: 50,
            cache_write: 25,
            total_tokens: 0,
            ..Default::default()
        };
        assert_eq!(calculate_context_tokens(&usage), 375);
    }

    #[test]
    fn assistant_usage_skips_aborted_and_error() {
        let aborted = assistant_msg(
            "x",
            Usage {
                total_tokens: 10,
                ..Default::default()
            },
            StopReason::Aborted,
        );
        let errored = assistant_msg(
            "x",
            Usage {
                total_tokens: 20,
                ..Default::default()
            },
            StopReason::Error,
        );
        let good = assistant_msg(
            "x",
            Usage {
                total_tokens: 30,
                ..Default::default()
            },
            StopReason::Stop,
        );
        let messages = [aborted, errored, good];
        let usage = get_last_assistant_usage(&messages).unwrap();
        assert_eq!(usage.total_tokens, 30);
    }

    #[test]
    fn estimate_tokens_for_message_user_text() {
        let msg = Message::User(UserMessage::new_text("a".repeat(40)));
        // 40 chars / 4 = 10 tokens.
        assert_eq!(estimate_tokens_for_message(&msg), 10);
    }

    #[test]
    fn estimate_tokens_for_message_assistant_tool_call_includes_args() {
        let msg = assistant_with_tool_call("read", serde_json::json!({"path": "/tmp/x.rs"}));
        let tokens = estimate_tokens_for_message(&msg);
        // Conservative bound: at least name length / 4 = 1.
        assert!(tokens >= 1);
        // And the JSON body of arguments contributes too.
        let bigger =
            assistant_with_tool_call("read", serde_json::json!({"path": "x".repeat(1000)}));
        assert!(estimate_tokens_for_message(&bigger) > tokens + 200);
    }

    #[test]
    fn estimate_tokens_for_message_tool_result_image_is_4800_chars() {
        let img = model::ImageContent {
            content_type: "image".into(),
            data: String::new(),
            mime_type: "image/png".into(),
        };
        let msg = Message::ToolResult(ToolResultMessage::new(
            "tc",
            "screenshot",
            vec![ToolResultContent::Image(img)],
        ));
        // 4800 chars / 4 = 1200 tokens.
        assert_eq!(estimate_tokens_for_message(&msg), 1200);
    }

    #[test]
    fn estimate_context_tokens_with_usage_uses_anchor_when_present() {
        let user = Message::User(UserMessage::new_text("a".repeat(40))); // 10 tokens
        let assistant = assistant_msg(
            "ignored",
            Usage {
                total_tokens: 500,
                ..Default::default()
            },
            StopReason::Stop,
        );
        let trailing = Message::User(UserMessage::new_text("b".repeat(80))); // 20 tokens

        let est = estimate_context_tokens_with_usage(&[user, assistant, trailing]);
        assert_eq!(est.usage_tokens, 500);
        assert_eq!(est.trailing_tokens, 20);
        assert_eq!(est.tokens, 520);
        assert_eq!(est.last_usage_index, Some(1));
    }

    #[test]
    fn estimate_context_tokens_with_usage_falls_back_to_heuristic() {
        let only_user = Message::User(UserMessage::new_text("a".repeat(40))); // 10 tokens
        let est = estimate_context_tokens_with_usage(&[only_user]);
        assert_eq!(est.usage_tokens, 0);
        assert_eq!(est.tokens, 10);
        assert_eq!(est.trailing_tokens, 10);
        assert!(est.last_usage_index.is_none());
    }

    // ---- should_compact_with_reserve ----

    #[test]
    fn should_compact_with_reserve_fires_above_threshold() {
        let s = CompactionRuntimeSettings::default();
        // window=200_000, reserve=16_384 → trigger at >183_616.
        assert!(should_compact_with_reserve(190_000, 200_000, &s));
        assert!(!should_compact_with_reserve(180_000, 200_000, &s));
    }

    #[test]
    fn should_compact_with_reserve_short_circuits_when_disabled() {
        let s = CompactionRuntimeSettings {
            enabled: false,
            ..Default::default()
        };
        assert!(!should_compact_with_reserve(999_999, 200_000, &s));
    }

    #[test]
    fn should_compact_with_reserve_handles_underflow() {
        // reserve > window — saturating_sub keeps us well-defined.
        let s = CompactionRuntimeSettings {
            enabled: true,
            reserve_tokens: 999_999,
            keep_recent_tokens: 0,
        };
        assert!(should_compact_with_reserve(1, 100, &s));
    }

    // ---- generate_summary ----

    #[tokio::test]
    async fn generate_summary_uses_initial_prompt_when_no_previous() {
        let client = ScriptedClient::new(vec![ok_assistant("summary text")]);
        let model = dummy_model(false);
        let messages = vec![Message::User(UserMessage::new_text("hi"))];
        let out = generate_summary(&messages, &model, 16_384, client.clone(), None, None, None)
            .await
            .unwrap();
        assert_eq!(out, "summary text");
        assert_eq!(client.call_count(), 1);
    }

    #[tokio::test]
    async fn generate_summary_appends_custom_focus() {
        let client = ScriptedClient::new(vec![ok_assistant("ok")]);
        let model = dummy_model(false);
        let messages = vec![Message::User(UserMessage::new_text("hi"))];
        let _ = generate_summary(
            &messages,
            &model,
            16_384,
            client.clone(),
            Some("watch the budget"),
            None,
            None,
        )
        .await
        .unwrap();
        // Inspect the captured options' max_tokens (0.8 × 16384 = 13107).
        let opts = client.calls.lock().unwrap()[0].clone();
        assert_eq!(opts.base.max_tokens, Some(13_107));
    }

    /// A summary wraps a transcript that is never sent again, so the
    /// prompt it would cache can never be hit. Left at the default,
    /// retention resolves to `Short` and the provider bills the request
    /// at its cache-write premium for an entry nobody reads.
    #[tokio::test]
    async fn generate_summary_opts_out_of_prompt_cache_writes() {
        let client = ScriptedClient::new(vec![ok_assistant("summary text")]);
        let model = dummy_model(false);
        let messages = vec![Message::User(UserMessage::new_text("hi"))];
        let _ = generate_summary(&messages, &model, 16_384, client.clone(), None, None, None)
            .await
            .unwrap();
        let opts = client.calls.lock().unwrap()[0].clone();
        assert_eq!(opts.base.cache_retention, Some(model::CacheRetention::None));
    }

    /// The turn-prefix summary is the same kind of one-shot request and
    /// must opt out on the same grounds.
    #[tokio::test]
    async fn generate_turn_prefix_summary_opts_out_of_prompt_cache_writes() {
        let client = ScriptedClient::new(vec![ok_assistant("prefix summary")]);
        let model = dummy_model(false);
        let messages = vec![Message::User(UserMessage::new_text("hi"))];
        let _ = generate_turn_prefix_summary(&messages, &model, 16_384, client.clone(), None)
            .await
            .unwrap();
        let opts = client.calls.lock().unwrap()[0].clone();
        assert_eq!(opts.base.cache_retention, Some(model::CacheRetention::None));
    }

    /// Opting out of the cache must not disturb the output cap the
    /// summary paths already computed.
    #[tokio::test]
    async fn summary_options_keep_their_output_cap() {
        let options = crate::core::compaction::summarization_stream_options(2048);
        assert_eq!(options.base.max_tokens, Some(2048));
        assert_eq!(
            options.base.cache_retention,
            Some(model::CacheRetention::None)
        );
    }

    #[tokio::test]
    async fn generate_summary_returns_error_on_error_stop_reason() {
        let client = ScriptedClient::new(vec![err_assistant("rate limited")]);
        let model = dummy_model(false);
        let messages = vec![Message::User(UserMessage::new_text("hi"))];
        let res = generate_summary(&messages, &model, 16_384, client, None, None, None).await;
        assert!(matches!(res, Err(ref s) if s.contains("rate limited")));
    }

    #[tokio::test]
    async fn generate_summary_propagates_transport_error() {
        let client = ScriptedClient::new(vec![Err("network down".into())]);
        let model = dummy_model(false);
        let messages = vec![Message::User(UserMessage::new_text("hi"))];
        let res = generate_summary(&messages, &model, 16_384, client, None, None, None).await;
        assert_eq!(res.unwrap_err(), "network down");
    }

    #[tokio::test]
    async fn generate_summary_passes_reasoning_only_when_model_supports_it() {
        // Reasoning-capable model + ThinkingLevel::High → option set.
        let client = ScriptedClient::new(vec![ok_assistant("ok")]);
        let model = dummy_model(true);
        let messages = vec![Message::User(UserMessage::new_text("hi"))];
        let _ = generate_summary(
            &messages,
            &model,
            16_384,
            client.clone(),
            None,
            None,
            Some(ThinkingLevel::High),
        )
        .await
        .unwrap();
        let opts = client.calls.lock().unwrap()[0].clone();
        assert!(matches!(opts.reasoning, Some(ThinkingLevel::High)));

        // Same level on a non-reasoning model → option NOT set.
        let client = ScriptedClient::new(vec![ok_assistant("ok")]);
        let model = dummy_model(false);
        let _ = generate_summary(
            &messages,
            &model,
            16_384,
            client.clone(),
            None,
            None,
            Some(ThinkingLevel::High),
        )
        .await
        .unwrap();
        let opts = client.calls.lock().unwrap()[0].clone();
        assert!(opts.reasoning.is_none());
    }

    #[tokio::test]
    async fn generate_summary_skips_reasoning_when_level_is_none() {
        // None encodes "no reasoning" — `ThinkingLevel` has no `Off`
        // variant, so absence carries the same meaning as an explicit
        // off sentinel would.
        let client = ScriptedClient::new(vec![ok_assistant("ok")]);
        let model = dummy_model(true);
        let messages = vec![Message::User(UserMessage::new_text("hi"))];
        let _ = generate_summary(&messages, &model, 16_384, client.clone(), None, None, None)
            .await
            .unwrap();
        let opts = client.calls.lock().unwrap()[0].clone();
        assert!(opts.reasoning.is_none());
    }

    // ---- generate_turn_prefix_summary ----

    #[tokio::test]
    async fn generate_turn_prefix_summary_uses_half_budget() {
        let client = ScriptedClient::new(vec![ok_assistant("prefix")]);
        let model = dummy_model(false);
        let messages = vec![Message::User(UserMessage::new_text("hi"))];
        let out = generate_turn_prefix_summary(&messages, &model, 16_384, client.clone(), None)
            .await
            .unwrap();
        assert_eq!(out, "prefix");
        let opts = client.calls.lock().unwrap()[0].clone();
        // 0.5 × 16_384 = 8192.
        assert_eq!(opts.base.max_tokens, Some(8_192));
    }

    // ---- compact ----

    #[tokio::test]
    async fn compact_non_split_returns_history_only() {
        let client = ScriptedClient::new(vec![ok_assistant("HISTORY")]);
        let model = dummy_model(false);
        let mut file_ops = FileOperations::default();
        file_ops.read.insert("a.rs".into());

        let input = CompactionInput {
            messages_to_summarize: vec![Message::User(UserMessage::new_text("x"))],
            turn_prefix_messages: vec![],
            is_split_turn: false,
            previous_summary: None,
            file_ops,
            settings: CompactionRuntimeSettings::default(),
        };
        let out = compact(input, &model, client.clone(), None, None)
            .await
            .unwrap();
        assert!(out.summary.starts_with("HISTORY"));
        // File-ops XML appended.
        assert!(out.summary.contains("<read-files>\na.rs\n</read-files>"));
        assert_eq!(out.read_files, vec!["a.rs".to_string()]);
        assert_eq!(client.call_count(), 1);
    }

    #[tokio::test]
    async fn compact_split_turn_merges_history_and_prefix() {
        let client = ScriptedClient::new(vec![ok_assistant("HIST"), ok_assistant("PREFIX")]);
        let model = dummy_model(false);
        let input = CompactionInput {
            messages_to_summarize: vec![Message::User(UserMessage::new_text("x"))],
            turn_prefix_messages: vec![Message::User(UserMessage::new_text("y"))],
            is_split_turn: true,
            previous_summary: None,
            file_ops: FileOperations::default(),
            settings: CompactionRuntimeSettings::default(),
        };
        let out = compact(input, &model, client.clone(), None, None)
            .await
            .unwrap();
        assert!(out.summary.contains("HIST"));
        assert!(out.summary.contains("PREFIX"));
        assert!(out.summary.contains("**Turn Context (split turn):**"));
        assert_eq!(client.call_count(), 2);
    }

    #[tokio::test]
    async fn compact_split_turn_with_no_history_uses_canned_text() {
        // history future short-circuits with "No prior history."; only the
        // prefix path actually calls the LLM.
        let client = ScriptedClient::new(vec![ok_assistant("PREFIX")]);
        let model = dummy_model(false);
        let input = CompactionInput {
            messages_to_summarize: vec![],
            turn_prefix_messages: vec![Message::User(UserMessage::new_text("y"))],
            is_split_turn: true,
            previous_summary: None,
            file_ops: FileOperations::default(),
            settings: CompactionRuntimeSettings::default(),
        };
        let out = compact(input, &model, client.clone(), None, None)
            .await
            .unwrap();
        assert!(out.summary.starts_with("No prior history."));
        assert!(out.summary.contains("PREFIX"));
        assert_eq!(client.call_count(), 1);
    }

    #[tokio::test]
    async fn compact_propagates_history_error() {
        let client = ScriptedClient::new(vec![err_assistant("history failed")]);
        let model = dummy_model(false);
        let input = CompactionInput {
            messages_to_summarize: vec![Message::User(UserMessage::new_text("x"))],
            turn_prefix_messages: vec![],
            is_split_turn: false,
            previous_summary: None,
            file_ops: FileOperations::default(),
            settings: CompactionRuntimeSettings::default(),
        };
        let res = compact(input, &model, client, None, None).await;
        assert!(res.unwrap_err().contains("history failed"));
    }

    // ---- CompactionDetails serde ----

    #[test]
    fn compaction_details_round_trips_camel_case() {
        let d = CompactionDetails {
            read_files: vec!["a.md".into()],
            modified_files: vec!["b.rs".into()],
        };
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains("\"readFiles\""));
        assert!(json.contains("\"modifiedFiles\""));
        let back: CompactionDetails = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d);
    }

    // ---- Settings defaults ----

    #[test]
    fn runtime_settings_defaults_are_stable() {
        let s = CompactionRuntimeSettings::default();
        assert!(s.enabled);
        assert_eq!(s.reserve_tokens, 16_384);
        assert_eq!(s.keep_recent_tokens, 20_000);
    }
}
