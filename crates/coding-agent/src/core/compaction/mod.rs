//! Context compaction — summarizes older session messages so the active
//! conversation stays under the model's context window.
//!
//! Split into:
//!
//! - [`utils`]: pure helpers — token estimation, file-operation tracking,
//!   conversation serialization, and the legacy helpers
//!   (`should_compact`, `split_for_compaction`, `build_compaction_prompt`,
//!   `extract_file_operations`) consumed by [`crate::core::agent_session`].
//! - [`branch_summarization`]: summarizes an abandoned conversation
//!   branch when the user navigates away — async, model-driven.
//! - [`compactor`]: the main compaction pipeline — finds a cut point,
//!   builds a summary prompt, calls the LLM, and produces a
//!   [`CompactionResult`].
//!
//! The legacy surface (`CompactionResult`, `FileOperations`,
//! `estimate_tokens`, `estimate_context_tokens`, `should_compact`,
//! `build_compaction_prompt`, `extract_file_operations`,
//! `split_for_compaction`) continues to be re-exported here so existing
//! callers — notably [`crate::core::agent_session`] — keep compiling
//! verbatim while the richer ported pipeline lands alongside.

pub mod branch_summarization;
pub mod compactor;
pub mod utils;

// Re-export the pre-existing public surface so external callers
// (`crate::core::agent_session`, integration tests, slash commands) do
// not need to know that the module became a directory.
pub use utils::{
    CompactionResult, FileOperations, SUMMARIZATION_SYSTEM_PROMPT, build_compaction_prompt,
    compute_file_lists, estimate_context_tokens, estimate_tokens, extract_file_operations,
    extract_file_ops_from_message, format_file_operations, serialize_conversation, should_compact,
    split_for_compaction,
};

pub use branch_summarization::{
    BranchSummaryDetails, BranchSummaryResult, DEFAULT_BRANCH_RESERVE_TOKENS,
    FALLBACK_CONTEXT_WINDOW, GenerateBranchSummaryOptions, SummarizationClient,
    generate_branch_summary,
};

pub use compactor::{
    CompactionDetails, CompactionInput, CompactionOutput, CompactionRuntimeSettings,
    ContextUsageEstimate, calculate_context_tokens, compact, estimate_context_tokens_with_usage,
    estimate_tokens_for_message, generate_summary, generate_turn_prefix_summary,
    get_last_assistant_usage, should_compact_with_reserve,
};
