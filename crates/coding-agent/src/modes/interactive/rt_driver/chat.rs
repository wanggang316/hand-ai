//! Chat renderer: [`ChatUpdate`] → scrollback [`Line`]s.
//!
//! The rt driver commits finalized chat output into the terminal's native
//! scrollback through the rt [`HistorySink`](hand_tui::rt::history::HistorySink).
//! That sink takes ratatui [`Line`]s, so this module is the thin bridge from the
//! driver-side [`ChatUpdate`] protocol (produced by
//! [`event_dispatch`](crate::modes::interactive::event_dispatch), which is reused
//! unchanged and carries **zero** `hand_tui` dependency) to the styled lines the
//! sink commits.
//!
//! # The message-component seam
//!
//! Every update is funnelled through [`render_update`], which returns the lines
//! for exactly one [`ChatUpdate`]. The rich per-message rendering — the tinted
//! user bubble, markdown assistant bodies, dim-italic thinking blocks, and the
//! per-message error footnote — lives in [`messages`](super::messages) and is
//! mounted here: the `AppendUser` and `AppendAssistant` arms delegate to it,
//! carrying a [`RenderContext`] (render width + the global thinking-collapse
//! flag) so the body renderers can wrap markdown to the pane and honour the
//! Ctrl+T toggle. Tool-result, status, and error lines stay flat text — they
//! have no rich component.
//!
//! Streaming deltas ([`ChatUpdate::ReplaceLastAssistant`],
//! [`ChatUpdate::ToolUpdate`]) render live in the active-area *preview* (see
//! [`messages::stream_preview_lines`]) rather than scrollback, so they return
//! `None` here — the driver commits the *final* snapshot on `MessageEnd`, which
//! is the M1 live-block commit semantics (one commit, no duplication, no jump).

use ratatui::style::Style;
use ratatui::text::Line;

use super::messages;
use crate::modes::interactive::event_dispatch::ChatUpdate;
use crate::modes::interactive::theme::ThemePalette;

/// Context the rich message renderers need: the pane width markdown bodies wrap
/// to, the global thinking-collapse flag (Ctrl+T) applied to every assistant
/// message, and the active theme palette that colours the message surfaces.
#[derive(Debug, Clone, Copy)]
pub struct RenderContext {
    /// The render width in columns — markdown bodies and the user bubble wrap /
    /// tint to this.
    pub width: u16,
    /// Whether thinking blocks render collapsed (the static label) rather than
    /// their full dim-italic body. Flipped globally by Ctrl+T.
    pub hide_thinking: bool,
    /// The active theme palette: the user-bubble tint, thinking-text and
    /// error/status colours resolve from it so a custom theme colours the chat
    /// (VAL-COMPAT-004). Defaults to the historical palette.
    pub palette: ThemePalette,
}

impl RenderContext {
    /// A context for `width` columns with thinking expanded and the default
    /// (historical) palette.
    #[must_use]
    pub fn new(width: u16) -> Self {
        Self {
            width,
            hide_thinking: false,
            palette: ThemePalette::default(),
        }
    }
}

/// Style for status / notice lines (compaction, `/help`, watchdog banner).
fn status_style(palette: &ThemePalette) -> Style {
    Style::default().fg(palette.warning)
}

/// Style for error banners.
fn error_style(palette: &ThemePalette) -> Style {
    Style::default().fg(palette.error)
}

/// Render one [`ChatUpdate`] into the scrollback lines it commits, or `None` when
/// the update has no scrollback representation (streaming deltas that render in
/// the active-area preview instead).
///
/// The `AppendUser` / `AppendAssistant` arms delegate to the rich
/// [`messages`](super::messages) renderers using `ctx`; the remaining arms are
/// flat text.
#[must_use]
pub fn render_update(update: &ChatUpdate, ctx: RenderContext) -> Option<Vec<Line<'static>>> {
    let palette = &ctx.palette;
    match update {
        ChatUpdate::AppendUser { text } => {
            Some(messages::user_bubble_lines(text, ctx.width, palette))
        }
        ChatUpdate::AppendAssistant { message } => Some(messages::assistant_lines(
            message,
            ctx.hide_thinking,
            ctx.width,
            palette,
        )),
        // Streaming deltas render in the active-area preview, not scrollback; the
        // final snapshot commits on MessageEnd through AppendAssistant.
        ChatUpdate::ReplaceLastAssistant { .. } => None,
        ChatUpdate::AppendToolResult { text } => Some(plain_lines(text)),
        ChatUpdate::ToolEnd {
            result_text,
            is_error,
            exit_code,
            ..
        } => Some(tool_end_lines(result_text, *is_error, *exit_code, palette)),
        // Tool start / update stream into a live component in a later feature; the
        // driver shows only the finalized result via ToolEnd.
        ChatUpdate::ToolStart { .. } | ChatUpdate::ToolUpdate { .. } => None,
        ChatUpdate::AppendStatus { text } => Some(status_lines(text, palette)),
        ChatUpdate::ThemeChanged { theme } => {
            Some(status_lines(&format!("[theme: {theme}]"), palette))
        }
    }
}

/// A finalized tool result: a dim body plus an error/exit-code tag so the driver
/// shows a tool ran and how it ended.
fn tool_end_lines(
    result_text: &str,
    is_error: bool,
    exit_code: Option<i32>,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let tag = match (is_error, exit_code) {
        (true, Some(code)) => format!("[tool error, exit {code}]"),
        (true, None) => "[tool error]".to_string(),
        (false, Some(code)) => format!("[tool ok, exit {code}]"),
        (false, None) => "[tool ok]".to_string(),
    };
    let style = if is_error {
        error_style(palette)
    } else {
        status_style(palette)
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

/// Status / notice lines, styled with the palette's warning colour.
fn status_lines(text: &str, palette: &ThemePalette) -> Vec<Line<'static>> {
    text.split('\n')
        .map(|line| Line::from(line.to_string()).style(status_style(palette)))
        .collect()
}

/// Public entry to the status-line rendering, for a caller (the Ctrl+T toggle,
/// startup notices) that commits a status line directly without going through a
/// [`ChatUpdate`]. Uses the default (historical yellow) palette — these
/// secondary notices are not on the themed chat path.
#[must_use]
pub fn status_lines_for(text: &str) -> Vec<Line<'static>> {
    status_lines(text, &ThemePalette::default())
}

/// An error banner, styled with the palette's error colour, for the driver's
/// error path (the red-banner route the legacy `push_error` took, kept distinct
/// from dim status lines). Uses the default (historical red) palette.
#[must_use]
pub fn error_lines(text: &str) -> Vec<Line<'static>> {
    text.split('\n')
        .map(|line| Line::from(line.to_string()).style(error_style(&ThemePalette::default())))
        .collect()
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
            raw_stop_reason: None,
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

    fn ctx() -> RenderContext {
        RenderContext::new(80)
    }

    #[test]
    fn user_update_renders_a_tinted_bubble_with_the_text() {
        let lines = render_update(
            &ChatUpdate::AppendUser {
                text: "hello world".to_string(),
            },
            ctx(),
        )
        .expect("user update renders");
        let joined: String = lines.iter().map(text_of).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("hello world"));
    }

    #[test]
    fn multiline_user_update_keeps_a_row_per_input_line() {
        let lines = render_update(
            &ChatUpdate::AppendUser {
                text: "line one\nline two".to_string(),
            },
            ctx(),
        )
        .expect("user update renders");
        let bodies: Vec<String> = lines
            .iter()
            .map(text_of)
            .filter(|t| t.contains("line"))
            .collect();
        assert_eq!(bodies.len(), 2);
        assert!(bodies[0].contains("line one"));
        assert!(bodies[1].contains("line two"));
    }

    #[test]
    fn assistant_update_renders_text_blocks() {
        let lines = render_update(
            &ChatUpdate::AppendAssistant {
                message: assistant("hi back"),
            },
            ctx(),
        )
        .expect("assistant update renders");
        let joined: String = lines.iter().map(text_of).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("hi back"));
    }

    #[test]
    fn empty_assistant_snapshot_renders_no_lines() {
        // MessageStart carries an empty snapshot; it must not commit a blank
        // block to scrollback.
        let lines = render_update(
            &ChatUpdate::AppendAssistant {
                message: assistant(""),
            },
            ctx(),
        )
        .expect("assistant update renders");
        assert!(lines.is_empty());
    }

    #[test]
    fn streaming_replace_has_no_scrollback_line() {
        let update = ChatUpdate::ReplaceLastAssistant {
            message: assistant("partial"),
        };
        assert!(render_update(&update, ctx()).is_none());
    }

    #[test]
    fn tool_start_and_update_have_no_scrollback_line() {
        assert!(
            render_update(
                &ChatUpdate::ToolStart {
                    tool_call_id: "c1".into(),
                    tool_name: "read".into(),
                    args: serde_json::json!({}),
                },
                ctx()
            )
            .is_none()
        );
        assert!(
            render_update(
                &ChatUpdate::ToolUpdate {
                    tool_call_id: "c1".into(),
                    partial_text: "…".into(),
                },
                ctx()
            )
            .is_none()
        );
    }

    #[test]
    fn tool_end_tags_success_with_exit_code() {
        let lines = render_update(
            &ChatUpdate::ToolEnd {
                tool_call_id: "c1".into(),
                result_text: "done".into(),
                is_error: false,
                exit_code: Some(0),
            },
            ctx(),
        )
        .expect("tool end renders");
        assert_eq!(text_of(&lines[0]), "[tool ok, exit 0]");
        assert_eq!(text_of(&lines[1]), "done");
    }

    #[test]
    fn tool_end_tags_error() {
        let lines = render_update(
            &ChatUpdate::ToolEnd {
                tool_call_id: "c1".into(),
                result_text: "nope".into(),
                is_error: true,
                exit_code: None,
            },
            ctx(),
        )
        .expect("tool end renders");
        assert_eq!(text_of(&lines[0]), "[tool error]");
    }

    #[test]
    fn status_update_renders_yellow_line() {
        let lines = render_update(
            &ChatUpdate::AppendStatus {
                text: "[Compacting context...]".to_string(),
            },
            ctx(),
        )
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
