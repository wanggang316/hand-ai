//! Custom message types and LLM-context transformer.
//!
//! Defines four coding-agent–specific message variants
//! (`bashExecution`, `custom`, `branchSummary`, `compactionSummary`)
//! and a [`convert_to_llm`] transformer that flattens an agent-message
//! stream down to the LLM's `Message` shape.
//!
//! ## Current shape
//!
//! Until the full agent-message type ships through the RPC layer
//! (`rpc::types::MessagesData` carries opaque JSON for now), this
//! module owns the four custom variants directly as a Rust enum. The
//! on-the-wire shape is stable so a future generic agent-message
//! surface can adopt these variants verbatim.
//!
//! [`convert_to_llm`] flattens an agent-message stream into
//! `Vec<model::Message>`:
//!
//! - `bashExecution` → `User { text: bashExecutionToText }`, dropped
//!   when `exclude_from_context == true`.
//! - `custom` → `User { content }` (text or rich blocks).
//! - `branchSummary` → `User { wrapped in BRANCH_SUMMARY_PREFIX/SUFFIX }`.
//! - `compactionSummary` → `User { wrapped in COMPACTION_SUMMARY_*}`.
//! - Plain `User` / `Assistant` / `ToolResult` pass through unchanged.

use model::{
    AssistantMessage, ImageContent, Message, TextContent, ToolResultMessage, UserContent,
    UserContentBlock, UserMessage,
};
use serde::{Deserialize, Serialize};

/// Text glued before a compaction summary when injected into LLM
/// context. Verbatim from TS `COMPACTION_SUMMARY_PREFIX`.
pub const COMPACTION_SUMMARY_PREFIX: &str = "The conversation history before this point was compacted into the following summary:\n\n<summary>\n";

/// Text glued after a compaction summary. Verbatim from TS
/// `COMPACTION_SUMMARY_SUFFIX`.
pub const COMPACTION_SUMMARY_SUFFIX: &str = "\n</summary>";

/// Text glued before a branch summary. Verbatim from TS
/// `BRANCH_SUMMARY_PREFIX`.
pub const BRANCH_SUMMARY_PREFIX: &str =
    "The following is a summary of a branch that this conversation came back from:\n\n<summary>\n";

/// Text glued after a branch summary. Verbatim from TS
/// `BRANCH_SUMMARY_SUFFIX`.
pub const BRANCH_SUMMARY_SUFFIX: &str = "</summary>";

/// Bash execution recorded via the `!` command in interactive mode.
///
/// Mirrors the TS `BashExecutionMessage` interface. Stored on disk in
/// the session log; folded into LLM context as a user message via
/// [`bash_execution_to_text`] unless [`Self::exclude_from_context`] is
/// set (the `!!` prefix in TS).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BashExecutionMessage {
    /// Discriminator — fixed to `"bashExecution"` to match TS on the wire.
    pub role: String,
    pub command: String,
    pub output: String,
    pub exit_code: Option<i32>,
    pub cancelled: bool,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
    pub timestamp: u64,
    /// If `true`, this message is excluded from LLM context (`!!` prefix).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude_from_context: Option<bool>,
}

/// Content of a [`CustomMessage`] — either plain text (matching the
/// TS `string` branch) or a list of rich content blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CustomMessageContent {
    Text(String),
    Blocks(Vec<CustomMessageBlock>),
}

/// A content block inside a [`CustomMessage`]. The TS reference allows
/// `TextContent | ImageContent`; we mirror that with a tagged enum so
/// the wire shape (`{type: "text", ...}` / `{type: "image", ...}`) is
/// preserved.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum CustomMessageBlock {
    Text(TextContent),
    Image(ImageContent),
}

/// Extension-injected message via `sendMessage()`.
///
/// Mirrors the TS `CustomMessage<T>` interface. `details` is left as
/// opaque JSON because the TS generic is erased at runtime; concrete
/// extensions deserialize it on demand.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomMessage {
    pub role: String,
    pub custom_type: String,
    pub content: CustomMessageContent,
    pub display: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
    pub timestamp: u64,
}

/// Branch-replay summary, injected when a forked branch returns to its
/// parent. Mirrors TS `BranchSummaryMessage`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryMessage {
    pub role: String,
    pub summary: String,
    pub from_id: String,
    pub timestamp: u64,
}

/// Compaction summary recorded after the agent compacts older
/// conversation history. Mirrors TS `CompactionSummaryMessage`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CompactionSummaryMessage {
    pub role: String,
    pub summary: String,
    pub tokens_before: u64,
    pub timestamp: u64,
}

/// All agent-level messages — the LLM-shape variants (`User`,
/// `Assistant`, `ToolResult`) plus the four coding-agent custom
/// variants.
///
/// This is the local Rust analogue of TS `AgentMessage` for the
/// purposes of [`convert_to_llm`]. When the upstream `AgentMessage`
/// port lands in `upstream-agent-core` (TODO from `rpc::types`), this enum
/// can be replaced or re-exported from there.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "camelCase")]
pub enum AgentMessage {
    User(UserMessage),
    Assistant(AssistantMessage),
    ToolResult(ToolResultMessage),
    BashExecution(BashExecutionMessage),
    Custom(CustomMessage),
    BranchSummary(BranchSummaryMessage),
    CompactionSummary(CompactionSummaryMessage),
}

/// Format a [`BashExecutionMessage`] as user-message text for LLM
/// context. Mirrors TS `bashExecutionToText` line for line.
pub fn bash_execution_to_text(msg: &BashExecutionMessage) -> String {
    let mut text = format!("Ran `{}`\n", msg.command);
    if !msg.output.is_empty() {
        text.push_str("```\n");
        text.push_str(&msg.output);
        text.push_str("\n```");
    } else {
        text.push_str("(no output)");
    }
    if msg.cancelled {
        text.push_str("\n\n(command cancelled)");
    } else if let Some(code) = msg.exit_code
        && code != 0
    {
        text.push_str(&format!("\n\nCommand exited with code {code}"));
    }
    if msg.truncated
        && let Some(path) = &msg.full_output_path
    {
        text.push_str(&format!("\n\n[Output truncated. Full output: {path}]"));
    }
    text
}

/// Construct a [`BranchSummaryMessage`] from a textual `timestamp`.
///
/// `timestamp` mirrors the TS contract: an ISO-8601-or-RFC-3339 string
/// the caller already has on hand. Parse failures fall back to 0 to
/// match the TS `new Date(invalid).getTime()` → `NaN` → 0 path the
/// callers already tolerate.
pub fn create_branch_summary_message(
    summary: impl Into<String>,
    from_id: impl Into<String>,
    timestamp: &str,
) -> BranchSummaryMessage {
    BranchSummaryMessage {
        role: "branchSummary".into(),
        summary: summary.into(),
        from_id: from_id.into(),
        timestamp: parse_timestamp_ms(timestamp),
    }
}

/// Construct a [`CompactionSummaryMessage`].
pub fn create_compaction_summary_message(
    summary: impl Into<String>,
    tokens_before: u64,
    timestamp: &str,
) -> CompactionSummaryMessage {
    CompactionSummaryMessage {
        role: "compactionSummary".into(),
        summary: summary.into(),
        tokens_before,
        timestamp: parse_timestamp_ms(timestamp),
    }
}

/// Construct a [`CustomMessage`] from raw content + display flag.
pub fn create_custom_message(
    custom_type: impl Into<String>,
    content: CustomMessageContent,
    display: bool,
    details: Option<serde_json::Value>,
    timestamp: &str,
) -> CustomMessage {
    CustomMessage {
        role: "custom".into(),
        custom_type: custom_type.into(),
        content,
        display,
        details,
        timestamp: parse_timestamp_ms(timestamp),
    }
}

/// Parse an RFC-3339 timestamp string into milliseconds-since-epoch.
/// Returns 0 for unparseable input — matching the TS
/// `new Date("garbage").getTime()` (which yields `NaN` and is then
/// coerced to 0 by callers).
fn parse_timestamp_ms(timestamp: &str) -> u64 {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .and_then(|dt| u64::try_from(dt.timestamp_millis()).ok())
        .unwrap_or(0)
}

/// Transform agent messages (including custom variants) to
/// LLM-compatible [`Message`]s. Mirrors TS `convertToLlm`.
///
/// - `bashExecution` with `exclude_from_context = Some(true)` is
///   filtered out entirely.
/// - All custom variants flatten into [`Message::User`] with the
///   appropriate text/blocks.
/// - Plain LLM-shape variants pass through unchanged.
pub fn convert_to_llm(messages: &[AgentMessage]) -> Vec<Message> {
    messages.iter().filter_map(convert_one).collect()
}

fn convert_one(message: &AgentMessage) -> Option<Message> {
    match message {
        AgentMessage::BashExecution(m) => {
            if m.exclude_from_context.unwrap_or(false) {
                return None;
            }
            Some(Message::User(UserMessage {
                role: "user".into(),
                content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent::new(
                    bash_execution_to_text(m),
                ))]),
                timestamp: m.timestamp,
            }))
        }
        AgentMessage::Custom(m) => Some(custom_message_to_llm(&m.content, m.timestamp)),
        AgentMessage::BranchSummary(m) => {
            let text = format!(
                "{}{}{}",
                BRANCH_SUMMARY_PREFIX, m.summary, BRANCH_SUMMARY_SUFFIX
            );
            Some(Message::User(UserMessage {
                role: "user".into(),
                content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent::new(text))]),
                timestamp: m.timestamp,
            }))
        }
        AgentMessage::CompactionSummary(m) => {
            let text = format!(
                "{}{}{}",
                COMPACTION_SUMMARY_PREFIX, m.summary, COMPACTION_SUMMARY_SUFFIX
            );
            Some(Message::User(UserMessage {
                role: "user".into(),
                content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent::new(text))]),
                timestamp: m.timestamp,
            }))
        }
        AgentMessage::User(m) => Some(Message::User(m.clone())),
        AgentMessage::Assistant(m) => Some(Message::Assistant(m.clone())),
        AgentMessage::ToolResult(m) => Some(Message::ToolResult(m.clone())),
    }
}

/// Flatten custom-message content into an LLM user message. Shared
/// between [`convert_to_llm`] and the session-entry path below.
fn custom_message_to_llm(content: &CustomMessageContent, timestamp: u64) -> Message {
    let blocks = match content {
        CustomMessageContent::Text(s) => {
            vec![UserContentBlock::Text(TextContent::new(s.clone()))]
        }
        CustomMessageContent::Blocks(blocks) => blocks
            .iter()
            .map(|b| match b {
                CustomMessageBlock::Text(t) => UserContentBlock::Text(t.clone()),
                CustomMessageBlock::Image(i) => UserContentBlock::Image(i.clone()),
            })
            .collect(),
    };
    Message::User(UserMessage {
        role: "user".into(),
        content: UserContent::Blocks(blocks),
        timestamp,
    })
}

/// Convert a persisted `CustomMessage` session entry's raw JSON
/// `content` into the LLM user message it contributes to context.
///
/// Returns `None` — with a warning — when the content matches neither
/// the string nor the blocks shape of
/// [`CustomMessageContent`]: a malformed entry costs its own context
/// contribution, never the rest of the transcript.
pub(crate) fn custom_message_entry_to_llm(
    custom_type: &str,
    content: &serde_json::Value,
    timestamp_ms: i64,
) -> Option<Message> {
    let parsed: CustomMessageContent = match serde_json::from_value(content.clone()) {
        Ok(parsed) => parsed,
        Err(err) => {
            tracing::warn!(
                custom_type,
                error = %err,
                "custom message entry has unparseable content; \
                 excluding it from LLM context"
            );
            return None;
        }
    };
    Some(custom_message_to_llm(
        &parsed,
        u64::try_from(timestamp_ms).unwrap_or(0),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bash_msg() -> BashExecutionMessage {
        BashExecutionMessage {
            role: "bashExecution".into(),
            command: "ls".into(),
            output: "a\nb".into(),
            exit_code: Some(0),
            cancelled: false,
            truncated: false,
            full_output_path: None,
            timestamp: 100,
            exclude_from_context: None,
        }
    }

    #[test]
    fn bash_execution_to_text_includes_command_and_fenced_output() {
        let s = bash_execution_to_text(&bash_msg());
        assert!(s.starts_with("Ran `ls`\n"));
        assert!(s.contains("```\na\nb\n```"));
    }

    #[test]
    fn bash_execution_to_text_handles_empty_output() {
        let mut m = bash_msg();
        m.output = String::new();
        let s = bash_execution_to_text(&m);
        assert!(s.contains("(no output)"));
        assert!(!s.contains("```"));
    }

    #[test]
    fn bash_execution_to_text_marks_cancellation() {
        let mut m = bash_msg();
        m.cancelled = true;
        let s = bash_execution_to_text(&m);
        assert!(s.contains("(command cancelled)"));
        // Cancellation takes precedence over a non-zero exit code.
        let mut m2 = bash_msg();
        m2.cancelled = true;
        m2.exit_code = Some(1);
        let s2 = bash_execution_to_text(&m2);
        assert!(s2.contains("(command cancelled)"));
        assert!(!s2.contains("Command exited with code"));
    }

    #[test]
    fn bash_execution_to_text_reports_nonzero_exit() {
        let mut m = bash_msg();
        m.exit_code = Some(2);
        let s = bash_execution_to_text(&m);
        assert!(s.contains("Command exited with code 2"));
    }

    #[test]
    fn bash_execution_to_text_includes_truncation_pointer() {
        let mut m = bash_msg();
        m.truncated = true;
        m.full_output_path = Some("/tmp/full.log".into());
        let s = bash_execution_to_text(&m);
        assert!(s.contains("[Output truncated. Full output: /tmp/full.log]"));
    }

    #[test]
    fn convert_to_llm_skips_excluded_bash_messages() {
        let mut excluded = bash_msg();
        excluded.exclude_from_context = Some(true);
        let messages = vec![
            AgentMessage::BashExecution(bash_msg()),
            AgentMessage::BashExecution(excluded),
        ];
        let llm = convert_to_llm(&messages);
        assert_eq!(llm.len(), 1, "excluded message must be filtered out");
    }

    #[test]
    fn convert_to_llm_wraps_branch_and_compaction_summaries() {
        let messages = vec![
            AgentMessage::BranchSummary(BranchSummaryMessage {
                role: "branchSummary".into(),
                summary: "branch-body".into(),
                from_id: "abc".into(),
                timestamp: 1,
            }),
            AgentMessage::CompactionSummary(CompactionSummaryMessage {
                role: "compactionSummary".into(),
                summary: "compaction-body".into(),
                tokens_before: 5_000,
                timestamp: 2,
            }),
        ];
        let llm = convert_to_llm(&messages);
        assert_eq!(llm.len(), 2);
        // Each summary must be wrapped in its prefix/suffix.
        match &llm[0] {
            Message::User(u) => match &u.content {
                UserContent::Blocks(blocks) => match &blocks[0] {
                    UserContentBlock::Text(t) => {
                        assert!(t.text.contains("<summary>\nbranch-body</summary>"));
                        assert!(t.text.starts_with(BRANCH_SUMMARY_PREFIX));
                    }
                    _ => panic!("expected text block"),
                },
                _ => panic!("expected blocks content"),
            },
            _ => panic!("expected user message"),
        }
        match &llm[1] {
            Message::User(u) => match &u.content {
                UserContent::Blocks(blocks) => match &blocks[0] {
                    UserContentBlock::Text(t) => {
                        assert!(t.text.starts_with(COMPACTION_SUMMARY_PREFIX));
                        assert!(t.text.ends_with(COMPACTION_SUMMARY_SUFFIX));
                        assert!(t.text.contains("compaction-body"));
                    }
                    _ => panic!("expected text block"),
                },
                _ => panic!("expected blocks content"),
            },
            _ => panic!("expected user message"),
        }
    }

    #[test]
    fn convert_to_llm_flattens_custom_text_and_blocks() {
        let text_msg = AgentMessage::Custom(CustomMessage {
            role: "custom".into(),
            custom_type: "extension/foo".into(),
            content: CustomMessageContent::Text("hello".into()),
            display: true,
            details: None,
            timestamp: 10,
        });
        let blocks_msg = AgentMessage::Custom(CustomMessage {
            role: "custom".into(),
            custom_type: "extension/foo".into(),
            content: CustomMessageContent::Blocks(vec![
                CustomMessageBlock::Text(TextContent::new("a")),
                CustomMessageBlock::Image(ImageContent::new("BASE64", "image/png")),
            ]),
            display: false,
            details: Some(serde_json::json!({"k": 1})),
            timestamp: 20,
        });
        let llm = convert_to_llm(&[text_msg, blocks_msg]);
        assert_eq!(llm.len(), 2);
        // Text variant — single text block.
        match &llm[0] {
            Message::User(u) => match &u.content {
                UserContent::Blocks(blocks) => {
                    assert_eq!(blocks.len(), 1);
                    match &blocks[0] {
                        UserContentBlock::Text(t) => assert_eq!(t.text, "hello"),
                        _ => panic!("expected text block"),
                    }
                }
                _ => panic!("expected blocks"),
            },
            _ => panic!("expected user"),
        }
        // Blocks variant — text + image preserved in order.
        match &llm[1] {
            Message::User(u) => match &u.content {
                UserContent::Blocks(blocks) => {
                    assert_eq!(blocks.len(), 2);
                    matches!(blocks[0], UserContentBlock::Text(_));
                    matches!(blocks[1], UserContentBlock::Image(_));
                }
                _ => panic!("expected blocks"),
            },
            _ => panic!("expected user"),
        }
    }

    #[test]
    fn factory_helpers_parse_rfc3339_timestamps() {
        // 1970-01-01T00:00:01Z = 1000 ms
        let m = create_branch_summary_message("s", "id-x", "1970-01-01T00:00:01Z");
        assert_eq!(m.timestamp, 1000);
        assert_eq!(m.role, "branchSummary");
        assert_eq!(m.summary, "s");
        assert_eq!(m.from_id, "id-x");

        let m2 = create_compaction_summary_message("s2", 42, "1970-01-01T00:00:02Z");
        assert_eq!(m2.timestamp, 2000);
        assert_eq!(m2.tokens_before, 42);

        let m3 = create_custom_message(
            "ext/foo",
            CustomMessageContent::Text("c".into()),
            true,
            None,
            "1970-01-01T00:00:03Z",
        );
        assert_eq!(m3.timestamp, 3000);
        assert_eq!(m3.custom_type, "ext/foo");
    }

    #[test]
    fn factory_helpers_fall_back_to_zero_on_invalid_timestamp() {
        let m = create_branch_summary_message("s", "id", "not-a-date");
        assert_eq!(m.timestamp, 0);
    }
}
