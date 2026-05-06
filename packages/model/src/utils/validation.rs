//! Structural validation for `Context` instances before sending them to a
//! provider.
//!
//! Each issue carries a categorical `kind`, a human-readable `message`, and
//! the index of the offending message in the context (when applicable). The
//! validator walks the messages in order and reports problems that would
//! otherwise show up as opaque provider errors:
//!
//! - empty user / assistant content,
//! - tool calls without a matching tool result,
//! - tool results without a preceding tool call,
//! - tool definitions with empty or duplicated names,
//! - image blocks missing `data` or `mime_type`,
//! - tool-result-to-user transitions without an intervening assistant turn
//!   (some APIs reject this shape).
//!
//! The function is non-fatal: callers may decide to drop, repair, or pass
//! through messages despite reported issues.

use crate::types::{
    AssistantContentBlock, Context, Message, ToolResultContent, UserContent, UserContentBlock,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Categorical kind of validation issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ValidationIssueKind {
    /// An assistant tool call has no matching tool result later in the
    /// conversation.
    OrphanToolCall,
    /// A user or assistant message has empty content.
    EmptyContent,
    /// An image block is missing `data` or `mime_type`.
    InvalidImage,
    /// A tool definition has an empty name.
    EmptyToolName,
    /// Two tool definitions share the same name.
    DuplicateToolName,
    /// A tool result references a tool call id that does not exist.
    OrphanToolResult,
    /// A tool result is followed directly by a user message with no
    /// assistant turn in between, which some APIs reject.
    MissingAssistantBetweenToolResultAndUser,
}

/// One validation finding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationIssue {
    /// What went wrong.
    pub kind: ValidationIssueKind,
    /// Human-readable explanation.
    pub message: String,
    /// Zero-based index of the offending entry inside `Context::messages`,
    /// or `None` for issues that target the tool list.
    #[serde(rename = "messageIndex", skip_serializing_if = "Option::is_none")]
    pub message_index: Option<usize>,
}

impl ValidationIssue {
    fn new(
        kind: ValidationIssueKind,
        message: impl Into<String>,
        message_index: Option<usize>,
    ) -> Self {
        Self {
            kind,
            message: message.into(),
            message_index,
        }
    }
}

/// Validate a `Context` and return all issues found.
pub fn validate_context(ctx: &Context) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();

    // ---- Tool definitions ----
    if let Some(tools) = ctx.tools.as_ref() {
        let mut seen: HashSet<&str> = HashSet::new();
        for tool in tools {
            if tool.name.trim().is_empty() {
                issues.push(ValidationIssue::new(
                    ValidationIssueKind::EmptyToolName,
                    "tool definition has empty name",
                    None,
                ));
                continue;
            }
            if !seen.insert(tool.name.as_str()) {
                issues.push(ValidationIssue::new(
                    ValidationIssueKind::DuplicateToolName,
                    format!("duplicate tool name: {}", tool.name),
                    None,
                ));
            }
        }
    }

    // ---- Track tool calls and tool results across messages ----
    // Map id -> index of the message that produced the tool call.
    let mut pending_tool_calls: Vec<(String, usize)> = Vec::new();
    let mut all_tool_call_ids: HashSet<String> = HashSet::new();

    for (idx, message) in ctx.messages.iter().enumerate() {
        match message {
            Message::User(user) => {
                if user_content_is_empty(&user.content) {
                    issues.push(ValidationIssue::new(
                        ValidationIssueKind::EmptyContent,
                        "user message has empty content",
                        Some(idx),
                    ));
                }
                for block in user_blocks(&user.content) {
                    if let UserContentBlock::Image(img) = block
                        && (img.data.is_empty() || img.mime_type.is_empty())
                    {
                        issues.push(ValidationIssue::new(
                            ValidationIssueKind::InvalidImage,
                            "user image block missing data or mime_type",
                            Some(idx),
                        ));
                    }
                }

                // tool-result -> user without an assistant in between.
                if idx > 0 && matches!(ctx.messages.get(idx - 1), Some(Message::ToolResult(_))) {
                    issues.push(ValidationIssue::new(
                        ValidationIssueKind::MissingAssistantBetweenToolResultAndUser,
                        "tool result followed by user message without assistant turn",
                        Some(idx),
                    ));
                }
            }
            Message::Assistant(asst) => {
                if asst.content.is_empty() {
                    issues.push(ValidationIssue::new(
                        ValidationIssueKind::EmptyContent,
                        "assistant message has empty content",
                        Some(idx),
                    ));
                }
                for block in &asst.content {
                    if let AssistantContentBlock::ToolCall(tc) = block {
                        pending_tool_calls.push((tc.id.clone(), idx));
                        all_tool_call_ids.insert(tc.id.clone());
                    }
                }
            }
            Message::ToolResult(tr) => {
                if tr.content.is_empty() {
                    issues.push(ValidationIssue::new(
                        ValidationIssueKind::EmptyContent,
                        "tool result has empty content",
                        Some(idx),
                    ));
                }
                for block in &tr.content {
                    if let ToolResultContent::Image(img) = block
                        && (img.data.is_empty() || img.mime_type.is_empty())
                    {
                        issues.push(ValidationIssue::new(
                            ValidationIssueKind::InvalidImage,
                            "tool result image block missing data or mime_type",
                            Some(idx),
                        ));
                    }
                }

                if !all_tool_call_ids.contains(&tr.tool_call_id) {
                    issues.push(ValidationIssue::new(
                        ValidationIssueKind::OrphanToolResult,
                        format!(
                            "tool result references unknown tool call id: {}",
                            tr.tool_call_id
                        ),
                        Some(idx),
                    ));
                } else if let Some(pos) = pending_tool_calls
                    .iter()
                    .position(|(id, _)| id == &tr.tool_call_id)
                {
                    pending_tool_calls.remove(pos);
                }
            }
        }
    }

    // Anything still pending is an orphan tool call.
    for (id, idx) in pending_tool_calls {
        issues.push(ValidationIssue::new(
            ValidationIssueKind::OrphanToolCall,
            format!("tool call {id} has no matching tool result"),
            Some(idx),
        ));
    }

    issues
}

fn user_content_is_empty(content: &UserContent) -> bool {
    match content {
        UserContent::Text(t) => t.is_empty(),
        UserContent::Blocks(blocks) => blocks.is_empty(),
    }
}

fn user_blocks(content: &UserContent) -> &[UserContentBlock] {
    match content {
        UserContent::Blocks(blocks) => blocks.as_slice(),
        UserContent::Text(_) => &[],
    }
}
