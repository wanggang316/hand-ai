//! Skeleton chat renderer: [`ChatUpdate`] → scrollback [`Line`]s.
//!
//! The rt driver commits finalized chat output into the terminal's native
//! scrollback through the rt [`HistorySink`](hand_tui::rt::history::HistorySink).
//! That sink takes ratatui [`Line`]s, so this module is the thin bridge from the
//! driver-side [`ChatUpdate`] protocol (produced by
//! [`event_dispatch`](crate::modes::interactive::event_dispatch), which is reused
//! unchanged and carries **zero** `hand_tui` dependency) to the styled lines the
//! sink commits.
//!
//! # Scope — skeleton rendering only
//!
//! This feature renders the *shapes the skeleton needs to demonstrate a turn*:
//! a user echo, an assistant text block, tool-result and status lines. It is a
//! flat, one-block-per-update text rendering — **not** the rich per-message
//! components (thinking blocks, bash cards, markdown, syntax highlighting) that
//! land in later M3 features.
//!
//! ## Seam for the message components
//!
//! Every update is funnelled through [`render_update`], which returns the lines
//! for exactly one [`ChatUpdate`]. A later feature mounts real components by
//! replacing the body of the arm it owns (e.g. routing `AppendAssistant` through
//! the rt `MarkdownView`) without touching the driver's commit path or the
//! `event_dispatch` protocol. Streaming updates
//! ([`ChatUpdate::ReplaceLastAssistant`], [`ChatUpdate::ToolUpdate`]) render live
//! in the viewport rather than scrollback and so return `None` here — the
//! skeleton echoes the *final* snapshot on `MessageEnd`/`AppendAssistant`, which
//! is enough to prove a streamed turn lands in history.

use hand_tui::rt::history::wrap_lines;
use ratatui::style::{Color, Style};
use ratatui::text::Line;

use crate::modes::interactive::event_dispatch::ChatUpdate;

/// Style for the user-echo prefix — a dim `> ` before the submitted text, the
/// hand chat convention.
fn user_style() -> Style {
    Style::default().fg(Color::Cyan)
}

/// Style for status / notice lines (compaction, `/help`, watchdog banner).
fn status_style() -> Style {
    Style::default().fg(Color::Yellow)
}

/// Style for error banners.
fn error_style() -> Style {
    Style::default().fg(Color::Red)
}

/// Render one [`ChatUpdate`] into the scrollback lines it commits, or `None` when
/// the update has no scrollback representation in the skeleton (streaming deltas
/// that render live in the viewport instead).
///
/// This is the single seam a later feature extends to mount real message
/// components: each arm owns the rendering of its update kind, so swapping a flat
/// text block for a component touches only that arm.
#[must_use]
pub fn render_update(update: &ChatUpdate) -> Option<Vec<Line<'static>>> {
    match update {
        ChatUpdate::AppendUser { text } => Some(user_lines(text)),
        ChatUpdate::AppendAssistant { message } => Some(assistant_lines(message)),
        // Streaming deltas: the skeleton renders the final snapshot on
        // AppendAssistant / (the trailing) ReplaceLastAssistant-as-end. A live
        // in-viewport preview is a later feature, so a mid-stream replace has no
        // scrollback line of its own.
        ChatUpdate::ReplaceLastAssistant { .. } => None,
        ChatUpdate::AppendToolResult { text } => Some(plain_lines(text)),
        ChatUpdate::ToolEnd {
            result_text,
            is_error,
            exit_code,
            ..
        } => Some(tool_end_lines(result_text, *is_error, *exit_code)),
        // Tool start / update stream into a live component in a later feature; the
        // skeleton shows only the finalized result via ToolEnd.
        ChatUpdate::ToolStart { .. } | ChatUpdate::ToolUpdate { .. } => None,
        ChatUpdate::AppendStatus { text } => Some(status_lines(text)),
        ChatUpdate::ThemeChanged { theme } => Some(status_lines(&format!("[theme: {theme}]"))),
    }
}

/// The dim-prefixed echo of a submitted user message, one logical line per input
/// line so a multi-line prompt keeps its shape in scrollback.
fn user_lines(text: &str) -> Vec<Line<'static>> {
    text.split('\n')
        .map(|line| Line::from(format!("> {line}")).style(user_style()))
        .collect()
}

/// The concatenated text blocks of an assistant message, split into logical
/// lines. Thinking and tool-call blocks are skipped — they render through
/// dedicated components in a later feature.
fn assistant_lines(message: &model::AssistantMessage) -> Vec<Line<'static>> {
    let text = assistant_text(message);
    if text.is_empty() {
        return Vec::new();
    }
    plain_lines(&text)
}

/// Extract the plain-text content of an assistant message, joining text blocks
/// with newlines. Mirrors the extraction the legacy driver uses for copy/echo.
fn assistant_text(message: &model::AssistantMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            model::AssistantContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A finalized tool result: a dim body plus an error/exit-code tag so the
/// skeleton shows a tool ran and how it ended.
fn tool_end_lines(result_text: &str, is_error: bool, exit_code: Option<i32>) -> Vec<Line<'static>> {
    let tag = match (is_error, exit_code) {
        (true, Some(code)) => format!("[tool error, exit {code}]"),
        (true, None) => "[tool error]".to_string(),
        (false, Some(code)) => format!("[tool ok, exit {code}]"),
        (false, None) => "[tool ok]".to_string(),
    };
    let style = if is_error {
        error_style()
    } else {
        status_style()
    };
    let mut lines = vec![Line::from(tag).style(style)];
    lines.extend(plain_lines(result_text));
    lines
}

/// Plain, unstyled logical lines from a text block.
fn plain_lines(text: &str) -> Vec<Line<'static>> {
    text.split('\n')
        .map(|line| Line::from(line.to_string()))
        .collect()
}

/// Status / notice lines, styled yellow.
fn status_lines(text: &str) -> Vec<Line<'static>> {
    text.split('\n')
        .map(|line| Line::from(line.to_string()).style(status_style()))
        .collect()
}

/// An error banner, styled red, for the driver's error path (the red-banner
/// route the legacy `push_error` took, kept distinct from dim status lines).
#[must_use]
pub fn error_lines(text: &str) -> Vec<Line<'static>> {
    text.split('\n')
        .map(|line| Line::from(line.to_string()).style(error_style()))
        .collect()
}

/// Pre-wrap a batch of already-rendered logical lines to `width`, matching the
/// wrap the [`HistorySink`](hand_tui::rt::history::HistorySink) applies on
/// commit. Exposed so a caller that wants the *visual* row count (e.g. a test)
/// can measure it without a live terminal.
#[must_use]
pub fn wrap_for_width(lines: &[Line<'static>], width: u16) -> Vec<Line<'static>> {
    wrap_lines(lines, width)
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::types::{
        Api, AssistantContentBlock, AssistantMessage, Provider, StopReason, TextContent, Usage,
    };

    fn assistant(text: &str) -> Box<AssistantMessage> {
        Box::new(AssistantMessage {
            role: "assistant".to_string(),
            content: vec![AssistantContentBlock::Text(TextContent::new(text))],
            api: Api::AnthropicMessages,
            provider: Provider::Anthropic,
            model: "claude".to_string(),
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        })
    }

    fn text_of(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn user_update_echoes_with_prompt_prefix() {
        let lines = render_update(&ChatUpdate::AppendUser {
            text: "hello world".to_string(),
        })
        .expect("user update renders");
        assert_eq!(lines.len(), 1);
        assert_eq!(text_of(&lines[0]), "> hello world");
    }

    #[test]
    fn multiline_user_update_keeps_one_line_each() {
        let lines = render_update(&ChatUpdate::AppendUser {
            text: "line one\nline two".to_string(),
        })
        .expect("user update renders");
        assert_eq!(lines.len(), 2);
        assert_eq!(text_of(&lines[0]), "> line one");
        assert_eq!(text_of(&lines[1]), "> line two");
    }

    #[test]
    fn assistant_update_renders_text_blocks() {
        let lines = render_update(&ChatUpdate::AppendAssistant {
            message: assistant("hi back"),
        })
        .expect("assistant update renders");
        assert_eq!(lines.len(), 1);
        assert_eq!(text_of(&lines[0]), "hi back");
    }

    #[test]
    fn empty_assistant_snapshot_renders_no_lines() {
        // MessageStart carries an empty snapshot; it must not commit a blank
        // block to scrollback.
        let lines = render_update(&ChatUpdate::AppendAssistant {
            message: assistant(""),
        })
        .expect("assistant update renders");
        assert!(lines.is_empty());
    }

    #[test]
    fn streaming_replace_has_no_scrollback_line() {
        let update = ChatUpdate::ReplaceLastAssistant {
            message: assistant("partial"),
        };
        assert!(render_update(&update).is_none());
    }

    #[test]
    fn tool_start_and_update_have_no_scrollback_line() {
        assert!(
            render_update(&ChatUpdate::ToolStart {
                tool_call_id: "c1".into(),
                tool_name: "read".into(),
                args: serde_json::json!({}),
            })
            .is_none()
        );
        assert!(
            render_update(&ChatUpdate::ToolUpdate {
                tool_call_id: "c1".into(),
                partial_text: "…".into(),
            })
            .is_none()
        );
    }

    #[test]
    fn tool_end_tags_success_with_exit_code() {
        let lines = render_update(&ChatUpdate::ToolEnd {
            tool_call_id: "c1".into(),
            result_text: "done".into(),
            is_error: false,
            exit_code: Some(0),
        })
        .expect("tool end renders");
        assert_eq!(text_of(&lines[0]), "[tool ok, exit 0]");
        assert_eq!(text_of(&lines[1]), "done");
    }

    #[test]
    fn tool_end_tags_error() {
        let lines = render_update(&ChatUpdate::ToolEnd {
            tool_call_id: "c1".into(),
            result_text: "nope".into(),
            is_error: true,
            exit_code: None,
        })
        .expect("tool end renders");
        assert_eq!(text_of(&lines[0]), "[tool error]");
    }

    #[test]
    fn status_update_renders_yellow_line() {
        let lines = render_update(&ChatUpdate::AppendStatus {
            text: "[Compacting context...]".to_string(),
        })
        .expect("status renders");
        assert_eq!(text_of(&lines[0]), "[Compacting context...]");
    }

    #[test]
    fn error_lines_split_on_newline() {
        let lines = error_lines("boom\ndetail");
        assert_eq!(lines.len(), 2);
        assert_eq!(text_of(&lines[0]), "boom");
        assert_eq!(text_of(&lines[1]), "detail");
    }
}
