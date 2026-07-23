//! Session **replay**: a resumed session's transcript → ordered scrollback
//! blocks, on the rt stack.
//!
//! When the driver resumes a session (`--continue`, `--resume`, `/resume`, or
//! `--fork`), the already-stored `user` / `assistant` / `tool-result` messages
//! must be rendered into native scrollback *in order*, exactly as they would have
//! landed live, so the resumed conversation reads as one continuous transcript —
//! then a `[resumed: <label>]` marker closes the replay so the boundary between
//! prior history and the fresh turn is visible.
//!
//! This is the rt-native port of the legacy `replay_messages_into`: where the
//! legacy path pushed `UserMessageComponent` / `AssistantMessageComponent` /
//! dimmed tool-result rows into an `Arc<Mutex<ChatList>>`, this produces owned
//! [`Line<'static>`] blocks (one `Vec<Line>` per message, the same shape the
//! [`HistorySink`](hand_tui::rt::history::HistorySink) commits) so the driver
//! queues each as a single `insert_before`.
//!
//! # What each message renders (reusing the M2 message components)
//!
//! - **User** → the tinted [`user_bubble_lines`](super::messages::user_bubble_lines)
//!   bubble, so a resumed prompt looks identical to a freshly-typed one.
//! - **Assistant** → [`assistant_lines`](super::messages::assistant_lines): markdown
//!   body, dim-italic thinking (honouring the global collapse flag), and — crucially
//!   for [`VAL-CHAT-029`] — the present-side **error footnote** when the stored
//!   message stopped with `stop_reason = Error/Aborted` and carries no tool call.
//!   Replaying an `error-ended` session is the only TUI path that surfaces that
//!   footnote live (a mock provider maps `finish_reason: error` to a normal stop).
//! - **Tool result** → a single **dimmed** `[tool_name] <body>` line, matching the
//!   legacy `coloured_text(… , Some(DIM_FG))` treatment: a resumed tool result is a
//!   compact one-liner, not the full state-tinted box a *live* tool call renders
//!   (the box needs the paired `ToolExecutionStart`/`End` events, which a stored
//!   result does not carry).

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use model::{Message, ToolResultContent, UserContent, UserContentBlock};

use super::messages::{assistant_lines, user_bubble_lines};

/// Dim foreground for a replayed tool-result line — the rt-native equivalent of
/// the legacy `DIM_FG`, so a resumed `[tool_name]` row recedes below the
/// user/assistant messages the way it did in the legacy driver.
const TOOL_DIM: Color = Color::DarkGray;

/// Render a resumed session's `messages` into ordered scrollback blocks, closed
/// by a `[resumed: <label>]` marker.
///
/// Each returned `Vec<Line>` is one scrollback block the caller queues as a
/// single `insert_before`, so the replayed transcript lands in message order with
/// the marker last. `width` wraps the message bodies; `hide_thinking` honours the
/// global Ctrl+T collapse so a resume respects the persisted (or default) thinking
/// visibility. An empty transcript still emits the marker block, so a resumed but
/// empty session shows the boundary line rather than nothing.
#[must_use]
pub fn replay_blocks(
    messages: &[Message],
    label: &str,
    hide_thinking: bool,
    width: u16,
) -> Vec<Vec<Line<'static>>> {
    let mut blocks: Vec<Vec<Line<'static>>> = Vec::new();

    for message in messages {
        match message {
            Message::User(user) => {
                let text = user_message_text(&user.content);
                blocks.push(user_bubble_lines(&text, width));
            }
            Message::Assistant(assistant) => {
                let lines = assistant_lines(assistant, hide_thinking, width);
                // An empty assistant snapshot (e.g. a bare `MessageStart` that was
                // persisted) renders no lines; skip it so no blank block scrolls.
                if !lines.is_empty() {
                    blocks.push(lines);
                }
            }
            Message::ToolResult(result) => {
                blocks.push(vec![tool_result_line(&result.tool_name, &result.content)]);
            }
        }
    }

    blocks.push(vec![resumed_marker(label)]);
    blocks
}

/// The plain text of a stored user message, joining a block message's text
/// blocks with newlines (image blocks are dropped — the bubble echoes text) and
/// passing a text message through verbatim. Mirrors the legacy replay's
/// user-content extraction.
fn user_message_text(content: &UserContent) -> String {
    match content {
        UserContent::Text(s) => s.clone(),
        UserContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|b| match b {
                UserContentBlock::Text(t) => Some(t.text.as_str()),
                UserContentBlock::Image(_) => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// A single dimmed `[tool_name] <body>` line for a replayed tool result. The body
/// joins the result's text blocks with newlines collapsed to spaces so the row
/// stays a compact one-liner (image blocks are dropped — a resumed result shows
/// its text, not graphics bytes).
fn tool_result_line(tool_name: &str, content: &[ToolResultContent]) -> Line<'static> {
    let body: String = content
        .iter()
        .filter_map(|c| match c {
            ToolResultContent::Text(t) => Some(t.text.as_str()),
            ToolResultContent::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
        .replace('\n', " ");
    let text = if body.is_empty() {
        format!("[{tool_name}]")
    } else {
        format!("[{tool_name}] {body}")
    };
    Line::from(Span::styled(text, Style::default().fg(TOOL_DIM)))
}

/// The `[resumed: <label>]` marker line closing a replay. Dim so it reads as a
/// boundary annotation rather than transcript content.
fn resumed_marker(label: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!("[resumed: {label}]"),
        Style::default().fg(TOOL_DIM).add_modifier(Modifier::ITALIC),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    use model::types::{
        Api, AssistantContentBlock, AssistantMessage, Provider, StopReason, TextContent,
        ToolResultMessage, Usage, UserMessage,
    };

    /// The plain concatenated text of a line.
    fn text_of(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// The joined plain text of every line of every block, one block per newline
    /// group — a simple ordering / presence check.
    fn joined(blocks: &[Vec<Line<'_>>]) -> String {
        blocks
            .iter()
            .flatten()
            .map(text_of)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn user(text: &str) -> Message {
        Message::User(UserMessage::new_text(text))
    }

    fn assistant(text: &str, stop_reason: StopReason) -> AssistantMessage {
        AssistantMessage {
            role: "assistant".to_string(),
            content: vec![AssistantContentBlock::Text(TextContent::new(text))],
            api: Api::AnthropicMessages,
            provider: Provider::Anthropic,
            model: "claude".to_string(),
            usage: Usage::default(),
            stop_reason,
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        }
    }

    fn tool_result(tool_name: &str, body: &str) -> Message {
        Message::ToolResult(ToolResultMessage {
            role: "toolResult".to_string(),
            tool_call_id: "call-1".to_string(),
            tool_name: tool_name.to_string(),
            content: vec![ToolResultContent::Text(TextContent::new(body))],
            details: None,
            is_error: false,
            timestamp: 0,
        })
    }

    // --- ordering + marker (VAL-CHAT-012) --------------------------------

    #[test]
    fn replays_messages_in_order_then_the_resumed_marker() {
        let messages = vec![
            user("first prompt"),
            Message::Assistant(assistant("first answer", StopReason::Stop)),
            user("second prompt"),
            Message::Assistant(assistant("second answer", StopReason::Stop)),
        ];
        let blocks = replay_blocks(&messages, "my-session", false, 60);
        let out = joined(&blocks);

        // Each fragment appears, and in transcript order.
        let idx = |needle: &str| {
            out.find(needle)
                .unwrap_or_else(|| panic!("missing {needle}: {out}"))
        };
        assert!(idx("first prompt") < idx("first answer"));
        assert!(idx("first answer") < idx("second prompt"));
        assert!(idx("second prompt") < idx("second answer"));
        // The resumed marker is last.
        assert!(idx("second answer") < idx("[resumed: my-session]"));
    }

    #[test]
    fn empty_transcript_still_emits_the_resumed_marker() {
        let blocks = replay_blocks(&[], "empty-session", false, 60);
        assert_eq!(blocks.len(), 1, "only the marker block");
        assert!(joined(&blocks).contains("[resumed: empty-session]"));
    }

    // --- tool-result dimmed one-liner (VAL-CHAT-012) ---------------------

    #[test]
    fn tool_result_renders_a_single_dimmed_bracketed_line() {
        let messages = vec![tool_result("read", "file contents here")];
        let blocks = replay_blocks(&messages, "s", false, 60);
        // The tool result is one block, one line.
        let tool_block = &blocks[0];
        assert_eq!(tool_block.len(), 1, "tool result is a single line");
        let line = &tool_block[0];
        assert_eq!(text_of(line), "[read] file contents here");
        // It is dimmed (DarkGray fg).
        assert!(
            line.spans.iter().all(|s| s.style.fg == Some(TOOL_DIM)),
            "a replayed tool result must be dimmed: {line:?}"
        );
    }

    #[test]
    fn tool_result_collapses_newlines_to_stay_a_one_liner() {
        let messages = vec![tool_result("bash", "line one\nline two")];
        let blocks = replay_blocks(&messages, "s", false, 60);
        assert_eq!(
            text_of(&blocks[0][0]),
            "[bash] line one line two",
            "newlines collapse to spaces so the row stays one line"
        );
    }

    #[test]
    fn empty_tool_result_renders_just_the_bracketed_name() {
        let messages = vec![Message::ToolResult(ToolResultMessage {
            role: "toolResult".to_string(),
            tool_call_id: "c".to_string(),
            tool_name: "noop".to_string(),
            content: vec![],
            details: None,
            is_error: false,
            timestamp: 0,
        })];
        let blocks = replay_blocks(&messages, "s", false, 60);
        assert_eq!(text_of(&blocks[0][0]), "[noop]");
    }

    // --- present-side error footnote on replay (VAL-CHAT-029) ------------

    #[test]
    fn replaying_an_error_ended_assistant_shows_the_red_error_footnote() {
        // The live TUB path that surfaces VAL-CHAT-029's present-side: a stored
        // assistant message with stop_reason = Error carries the red footnote when
        // replayed, so a resumed error-ended session shows it.
        let mut errored = assistant("partial before failure", StopReason::Error);
        errored.error_message = Some("rate limit exceeded".to_string());
        let blocks = replay_blocks(&[Message::Assistant(errored)], "err", false, 60);
        let out = joined(&blocks);
        assert!(
            out.contains("Error: rate limit exceeded"),
            "replay must surface the error footnote: {out}"
        );
        // The footnote is red.
        let footnote = blocks
            .iter()
            .flatten()
            .find(|l| text_of(l).contains("Error: rate limit exceeded"))
            .expect("error footnote line");
        assert!(
            footnote
                .spans
                .iter()
                .any(|s| s.style.fg == Some(Color::Red)),
            "the replayed error footnote must be red: {footnote:?}"
        );
    }

    #[test]
    fn replaying_an_aborted_assistant_shows_the_operation_aborted_footnote() {
        let aborted = assistant("cut short", StopReason::Aborted);
        let blocks = replay_blocks(&[Message::Assistant(aborted)], "s", false, 60);
        assert!(joined(&blocks).contains("Operation aborted"));
    }

    // --- user bubble + thinking honour the render context ----------------

    #[test]
    fn user_message_replays_as_a_tinted_bubble() {
        let blocks = replay_blocks(&[user("hi there")], "s", false, 60);
        assert!(joined(&blocks).contains("hi there"));
        // The bubble tints its rows (the user-bubble contract).
        assert!(
            blocks[0]
                .iter()
                .any(|l| l.spans.iter().any(|s| s.style.bg.is_some())),
            "a replayed user message must render the tinted bubble"
        );
    }

    #[test]
    fn user_block_content_joins_text_and_drops_images() {
        use model::types::{ImageContent, TextContent};
        let msg = Message::User(UserMessage::new_blocks(vec![
            UserContentBlock::Text(TextContent::new("line a")),
            UserContentBlock::Image(ImageContent {
                content_type: "image".to_string(),
                data: "AAAA".to_string(),
                mime_type: "image/png".to_string(),
            }),
            UserContentBlock::Text(TextContent::new("line b")),
        ]));
        let blocks = replay_blocks(&[msg], "s", false, 60);
        let out = joined(&blocks);
        assert!(out.contains("line a"), "{out}");
        assert!(out.contains("line b"), "{out}");
    }

    #[test]
    fn collapsed_thinking_hides_the_body_on_replay() {
        let mut msg = assistant("the answer", StopReason::Stop);
        msg.content.insert(
            0,
            AssistantContentBlock::Thinking(model::types::ThinkingContent::new("secret reasoning")),
        );
        // hide_thinking = true → the body is replaced by the static label.
        let blocks = replay_blocks(&[Message::Assistant(msg)], "s", true, 60);
        let out = joined(&blocks);
        assert!(out.contains("Thinking..."), "collapsed label: {out}");
        assert!(
            !out.contains("secret reasoning"),
            "collapsed thinking must not leak on replay: {out}"
        );
        assert!(out.contains("the answer"), "body still shows: {out}");
    }
}
