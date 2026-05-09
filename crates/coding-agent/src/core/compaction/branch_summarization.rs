//! Branch summarization — message-list path.
//!
//! Rust port of pi-mono `core/compaction/branch-summarization.ts`. When the
//! user navigates to a different point in the session tree, this module
//! generates a summary of the branch being left so context isn't lost.
//!
//! The TS reference walks `SessionEntry`s through helpers like
//! `collectEntriesForBranchSummary` and `prepareBranchEntries`. The current
//! Rust [`crate::core::session_manager::SessionEntry`] is strictly less
//! expressive (no `branch_summary` / `custom_message` / `bash_execution` /
//! `thinking_level_change` variants and no parent-id tree), so the
//! entry-tree-walking helpers are deliberately left as TODOs and only the
//! message-list-oriented path is ported here. See the controller's brief
//! and the master parity plan (§A4) for the follow-up work.
//!
//! [`generate_branch_summary`] takes a flat `&[Message]` slice plus already
//! computed [`super::utils::FileOperations`] and goes straight to the LLM
//! through a [`SummarizationClient`], which lets tests stub the network.

use crate::core::compaction::utils::{
    FileOperations, SUMMARIZATION_SYSTEM_PROMPT, compute_file_lists, format_file_operations,
    serialize_conversation,
};
use async_trait::async_trait;
use model::{
    AssistantContentBlock, AssistantMessage, Context, Message, Model, SimpleStreamOptions,
    StopReason, UserMessage,
};
use std::sync::Arc;

// ============================================================================
// Constants
// ============================================================================

/// Default token budget reserved for the summarization prompt + LLM
/// response. Matches the pi-mono default (`reserveTokens = 16384`).
pub const DEFAULT_BRANCH_RESERVE_TOKENS: u64 = 16_384;

/// Fallback context window when [`Model::context_window`] is missing/zero.
/// Mirrors pi-mono's `model.contextWindow || 128000` guard.
pub const FALLBACK_CONTEXT_WINDOW: u64 = 128_000;

const BRANCH_SUMMARY_PREAMBLE: &str = "The user explored a different conversation branch before returning here.\nSummary of that exploration:\n\n";

const BRANCH_SUMMARY_PROMPT: &str = r#"Create a structured summary of this conversation branch for context when returning later.

Use this EXACT format:

## Goal
[What was the user trying to accomplish in this branch?]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned]
- [Or "(none)" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Work that was started but not finished]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [What should happen next to continue this work]

Keep each section concise. Preserve exact file paths, function names, and error messages."#;

// ============================================================================
// Types
// ============================================================================

/// Outcome of a branch-summary generation. Matches pi-mono's
/// `BranchSummaryResult`: at most one of `summary` / `aborted` / `error` is
/// populated, and `read_files` / `modified_files` are populated alongside a
/// successful `summary`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BranchSummaryResult {
    pub summary: Option<String>,
    pub read_files: Vec<String>,
    pub modified_files: Vec<String>,
    pub aborted: bool,
    pub error: Option<String>,
}

/// Persistable payload mirroring pi-mono's `BranchSummaryDetails`. Stored
/// alongside a `branch_summary` entry once that variant is added to
/// [`crate::core::session_manager::SessionEntry`].
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BranchSummaryDetails {
    #[serde(rename = "readFiles", default)]
    pub read_files: Vec<String>,
    #[serde(rename = "modifiedFiles", default)]
    pub modified_files: Vec<String>,
}

/// Options passed to [`generate_branch_summary`]. The shape mirrors pi-mono's
/// `GenerateBranchSummaryOptions` minus the `signal` (cancellation flows
/// through [`SimpleStreamOptions`] when callers wire it).
#[derive(Debug, Clone, Default)]
pub struct GenerateBranchSummaryOptions {
    /// Optional custom focus appended to (or replacing) the default prompt.
    pub custom_instructions: Option<String>,
    /// When `true`, [`Self::custom_instructions`] replaces the default prompt
    /// instead of being appended.
    pub replace_instructions: bool,
    /// Tokens reserved for prompt + LLM response. `None` defaults to
    /// [`DEFAULT_BRANCH_RESERVE_TOKENS`].
    pub reserve_tokens: Option<u64>,
}

// ============================================================================
// SummarizationClient trait
// ============================================================================

/// Network-touching surface used by the summarizer.
///
/// Wrapping [`model::complete_simple`] behind a trait keeps the test path
/// fully synchronous and deterministic — see [`super::compactor`] for the
/// shared production adapter and the in-module tests for the in-memory
/// stub.
#[async_trait]
pub trait SummarizationClient: Send + Sync {
    /// Run a non-streaming completion and return the assistant message.
    /// Implementations should mirror pi-mono's `completeSimple` semantics:
    /// errors and aborts are returned as an [`AssistantMessage`] with the
    /// matching [`StopReason`] rather than as `Err`.
    async fn complete(
        &self,
        model: &Model,
        context: Context,
        options: SimpleStreamOptions,
    ) -> Result<AssistantMessage, String>;
}

// ============================================================================
// Public API — message-list path
// ============================================================================

/// Generate a branch summary from an already-prepared message slice.
///
/// This is the message-list-oriented half of pi-mono's
/// `generateBranchSummary`. Callers are expected to have already walked
/// the session tree, computed [`FileOperations`], and produced the flat
/// `messages` slice in chronological order. The entry-tree-walking
/// helpers (`collectEntriesForBranchSummary`, `prepareBranchEntries`,
/// `getMessageFromEntry`) cannot be ported 1:1 until
/// [`crate::core::session_manager::SessionEntry`] grows the missing
/// variants — see the module-level docs and the §A4 follow-up.
///
/// Returns a [`BranchSummaryResult`]:
/// - `summary` populated on success (with [`BRANCH_SUMMARY_PREAMBLE`] and
///   the file-operations XML appended).
/// - `aborted = true` when the underlying completion was aborted.
/// - `error = Some(_)` when the completion or transport returned an error.
pub async fn generate_branch_summary(
    messages: &[Message],
    file_ops: &FileOperations,
    model: &Model,
    client: Arc<dyn SummarizationClient>,
    options: GenerateBranchSummaryOptions,
) -> BranchSummaryResult {
    if messages.is_empty() {
        return BranchSummaryResult {
            summary: Some("No content to summarize".to_string()),
            ..Default::default()
        };
    }

    // Build the user-side prompt: <conversation> ... </conversation> + instructions.
    let conversation_text = serialize_conversation(messages);
    let instructions = if options.replace_instructions {
        options
            .custom_instructions
            .clone()
            .unwrap_or_else(|| BRANCH_SUMMARY_PROMPT.to_string())
    } else if let Some(extra) = options.custom_instructions.as_ref() {
        format!("{BRANCH_SUMMARY_PROMPT}\n\nAdditional focus: {extra}")
    } else {
        BRANCH_SUMMARY_PROMPT.to_string()
    };
    let prompt_text =
        format!("<conversation>\n{conversation_text}\n</conversation>\n\n{instructions}");

    // Compose the request. SUMMARIZATION_SYSTEM_PROMPT installs the "do not
    // continue the conversation" guard rail; the user message carries the
    // wrapped transcript and the structured-summary instructions.
    let context = Context {
        system_prompt: Some(SUMMARIZATION_SYSTEM_PROMPT.to_string()),
        messages: vec![Message::User(UserMessage::new_text(prompt_text))],
        tools: None,
    };

    // pi-mono uses `maxTokens: 2048` for branch summaries.
    let mut stream_options = SimpleStreamOptions::default();
    stream_options.base.max_tokens = Some(2048);

    let response = match client.complete(model, context, stream_options).await {
        Ok(msg) => msg,
        Err(err) => {
            return BranchSummaryResult {
                error: Some(err),
                ..Default::default()
            };
        }
    };

    match response.stop_reason {
        StopReason::Aborted => {
            return BranchSummaryResult {
                aborted: true,
                ..Default::default()
            };
        }
        StopReason::Error => {
            return BranchSummaryResult {
                error: Some(
                    response
                        .error_message
                        .unwrap_or_else(|| "Summarization failed".to_string()),
                ),
                ..Default::default()
            };
        }
        _ => {}
    }

    let mut summary = extract_text(&response);
    if summary.is_empty() {
        summary = "No summary generated".to_string();
    }
    summary = format!("{BRANCH_SUMMARY_PREAMBLE}{summary}");

    let (read_files, modified_files) = compute_file_lists(file_ops);
    summary.push_str(&format_file_operations(&read_files, &modified_files));

    BranchSummaryResult {
        summary: Some(summary),
        read_files,
        modified_files,
        aborted: false,
        error: None,
    }
}

/// Pull the concatenated text content out of an [`AssistantMessage`].
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
// Entry-tree path — placeholder.
// ============================================================================

// TODO(parity): the pi-mono entry-tree path
// (`collectEntriesForBranchSummary`, `prepareBranchEntries`,
// `getMessageFromEntry`) requires extending
// `crate::core::session_manager::SessionEntry` with `branch_summary`,
// `custom_message`, `thinking_level_change`, and `bash_execution`
// variants plus a `parent_id` tree. Tracked in
// `docs/exec-plans/parity-completion.md` §A4.

#[cfg(test)]
mod tests {
    use super::*;
    use model::types::Provider;
    use model::{
        Api, AssistantContentBlock, AssistantMessage, StopReason, TextContent, Usage, UserMessage,
    };
    use std::sync::Mutex;

    /// In-memory stub that returns a pre-canned response (or error) and
    /// records the last `Context` it was handed.
    struct StubClient {
        response: Mutex<Result<AssistantMessage, String>>,
        last_context: Mutex<Option<Context>>,
    }

    impl StubClient {
        fn new(response: AssistantMessage) -> Arc<Self> {
            Arc::new(Self {
                response: Mutex::new(Ok(response)),
                last_context: Mutex::new(None),
            })
        }

        fn new_err(err: impl Into<String>) -> Arc<Self> {
            Arc::new(Self {
                response: Mutex::new(Err(err.into())),
                last_context: Mutex::new(None),
            })
        }
    }

    #[async_trait]
    impl SummarizationClient for StubClient {
        async fn complete(
            &self,
            _model: &Model,
            context: Context,
            _options: SimpleStreamOptions,
        ) -> Result<AssistantMessage, String> {
            *self.last_context.lock().unwrap() = Some(context);
            // Clone the canned result so the stub can be reused.
            self.response
                .lock()
                .unwrap()
                .as_ref()
                .map(|m| m.clone())
                .map_err(|e| e.clone())
        }
    }

    fn dummy_model() -> Model {
        Model {
            id: "test".into(),
            name: "Test".into(),
            api: Api::AnthropicMessages,
            provider: Provider::Anthropic,
            base_url: String::new(),
            reasoning: false,
            input: vec![],
            cost: model::Cost {
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

    fn assistant_text(text: &str, stop_reason: StopReason) -> AssistantMessage {
        AssistantMessage {
            role: "assistant".into(),
            content: vec![AssistantContentBlock::Text(TextContent::new(text))],
            api: Api::AnthropicMessages,
            provider: Provider::Anthropic,
            model: "test".into(),
            usage: Usage::default(),
            stop_reason,
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        }
    }

    fn assistant_error(error: &str) -> AssistantMessage {
        let mut m = assistant_text("", StopReason::Error);
        m.error_message = Some(error.into());
        m
    }

    #[tokio::test]
    async fn empty_messages_return_canned_summary() {
        let client = StubClient::new(assistant_text("ignored", StopReason::Stop));
        let model = dummy_model();
        let res = generate_branch_summary(
            &[],
            &FileOperations::default(),
            &model,
            client.clone(),
            GenerateBranchSummaryOptions::default(),
        )
        .await;
        assert_eq!(res.summary.as_deref(), Some("No content to summarize"));
        assert!(res.error.is_none());
        assert!(!res.aborted);
        // Stub should NOT have been called.
        assert!(client.last_context.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn success_prepends_preamble_and_appends_files() {
        let client = StubClient::new(assistant_text("## Goal\nDo a thing", StopReason::Stop));
        let model = dummy_model();
        let mut file_ops = FileOperations::default();
        file_ops.read.insert("docs/x.md".into());
        file_ops.edited.insert("src/lib.rs".into());
        let messages = vec![Message::User(UserMessage::new_text("hello"))];

        let res = generate_branch_summary(
            &messages,
            &file_ops,
            &model,
            client.clone(),
            GenerateBranchSummaryOptions::default(),
        )
        .await;

        let summary = res.summary.expect("summary present");
        assert!(summary.starts_with("The user explored a different conversation branch"));
        assert!(summary.contains("## Goal"));
        assert!(summary.contains("<read-files>\ndocs/x.md\n</read-files>"));
        assert!(summary.contains("<modified-files>\nsrc/lib.rs\n</modified-files>"));
        assert_eq!(res.read_files, vec!["docs/x.md".to_string()]);
        assert_eq!(res.modified_files, vec!["src/lib.rs".to_string()]);
    }

    #[tokio::test]
    async fn aborted_response_sets_aborted_flag() {
        let client = StubClient::new(assistant_text("partial", StopReason::Aborted));
        let model = dummy_model();
        let messages = vec![Message::User(UserMessage::new_text("hi"))];
        let res = generate_branch_summary(
            &messages,
            &FileOperations::default(),
            &model,
            client,
            GenerateBranchSummaryOptions::default(),
        )
        .await;
        assert!(res.aborted);
        assert!(res.summary.is_none());
        assert!(res.error.is_none());
    }

    #[tokio::test]
    async fn error_response_carries_message() {
        let client = StubClient::new(assistant_error("boom"));
        let model = dummy_model();
        let messages = vec![Message::User(UserMessage::new_text("hi"))];
        let res = generate_branch_summary(
            &messages,
            &FileOperations::default(),
            &model,
            client,
            GenerateBranchSummaryOptions::default(),
        )
        .await;
        assert_eq!(res.error.as_deref(), Some("boom"));
        assert!(res.summary.is_none());
    }

    #[tokio::test]
    async fn transport_error_propagates_to_error_field() {
        let client = StubClient::new_err("network down");
        let model = dummy_model();
        let messages = vec![Message::User(UserMessage::new_text("hi"))];
        let res = generate_branch_summary(
            &messages,
            &FileOperations::default(),
            &model,
            client,
            GenerateBranchSummaryOptions::default(),
        )
        .await;
        assert_eq!(res.error.as_deref(), Some("network down"));
    }

    #[tokio::test]
    async fn custom_instructions_appended_by_default() {
        let client = StubClient::new(assistant_text("ok", StopReason::Stop));
        let model = dummy_model();
        let messages = vec![Message::User(UserMessage::new_text("hi"))];
        let opts = GenerateBranchSummaryOptions {
            custom_instructions: Some("focus on file paths".into()),
            replace_instructions: false,
            reserve_tokens: None,
        };
        let _ = generate_branch_summary(
            &messages,
            &FileOperations::default(),
            &model,
            client.clone(),
            opts,
        )
        .await;

        let ctx = client
            .last_context
            .lock()
            .unwrap()
            .clone()
            .expect("context captured");
        let user_text = match &ctx.messages[0] {
            Message::User(u) => match &u.content {
                model::UserContent::Text(s) => s.clone(),
                _ => String::new(),
            },
            _ => String::new(),
        };
        // Default prompt is preserved AND custom focus is appended.
        assert!(user_text.contains("Create a structured summary"));
        assert!(user_text.contains("Additional focus: focus on file paths"));
    }

    #[tokio::test]
    async fn replace_instructions_overrides_default_prompt() {
        let client = StubClient::new(assistant_text("ok", StopReason::Stop));
        let model = dummy_model();
        let messages = vec![Message::User(UserMessage::new_text("hi"))];
        let opts = GenerateBranchSummaryOptions {
            custom_instructions: Some("only list files".into()),
            replace_instructions: true,
            reserve_tokens: None,
        };
        let _ = generate_branch_summary(
            &messages,
            &FileOperations::default(),
            &model,
            client.clone(),
            opts,
        )
        .await;

        let ctx = client
            .last_context
            .lock()
            .unwrap()
            .clone()
            .expect("context captured");
        let user_text = match &ctx.messages[0] {
            Message::User(u) => match &u.content {
                model::UserContent::Text(s) => s.clone(),
                _ => String::new(),
            },
            _ => String::new(),
        };
        // Default structured prompt is GONE.
        assert!(!user_text.contains("Create a structured summary"));
        assert!(user_text.contains("only list files"));
    }

    #[tokio::test]
    async fn empty_response_text_falls_back_to_placeholder() {
        let client = StubClient::new(assistant_text("", StopReason::Stop));
        let model = dummy_model();
        let messages = vec![Message::User(UserMessage::new_text("hi"))];
        let res = generate_branch_summary(
            &messages,
            &FileOperations::default(),
            &model,
            client,
            GenerateBranchSummaryOptions::default(),
        )
        .await;
        assert!(res.summary.unwrap().contains("No summary generated"));
    }

    #[tokio::test]
    async fn system_prompt_is_summarization_guard() {
        let client = StubClient::new(assistant_text("ok", StopReason::Stop));
        let model = dummy_model();
        let messages = vec![Message::User(UserMessage::new_text("hi"))];
        let _ = generate_branch_summary(
            &messages,
            &FileOperations::default(),
            &model,
            client.clone(),
            GenerateBranchSummaryOptions::default(),
        )
        .await;
        let ctx = client.last_context.lock().unwrap().clone().unwrap();
        assert_eq!(
            ctx.system_prompt.as_deref(),
            Some(SUMMARIZATION_SYSTEM_PROMPT)
        );
    }

    #[test]
    fn branch_summary_details_round_trips_camel_case() {
        let d = BranchSummaryDetails {
            read_files: vec!["a.md".into()],
            modified_files: vec!["b.rs".into()],
        };
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains("\"readFiles\""));
        assert!(json.contains("\"modifiedFiles\""));
        let back: BranchSummaryDetails = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d);
    }
}
