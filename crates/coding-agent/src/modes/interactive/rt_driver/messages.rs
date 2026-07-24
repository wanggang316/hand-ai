//! Message-component rendering: [`ChatUpdate`]-carried messages → scrollback
//! [`Line`]s, on the rt stack.
//!
//! This is the rt-native port of the legacy `components/{user_message,
//! assistant_message}` renderers. Where the legacy components render to
//! `Vec<String>` of ANSI-escaped lines through the old `hand_tui` `Component`
//! model, these render to owned [`Line<'static>`] rich text — spans carrying a
//! ratatui [`Style`] — the model the rt scheduler paints. The message *bodies*
//! (user and assistant text) run through the M2 rt markdown renderer
//! ([`render_markdown`]) with the syntax-highlighting theme, so a code fence in
//! an assistant reply is bordered and colored exactly as elsewhere in the rt
//! stack.
//!
//! # What each renderer preserves (behavioural signatures, pinned from legacy)
//!
//! - **User bubble** ([`user_bubble_lines`]) — the submitted prompt echoed
//!   immediately into a tinted box. Each *input* line renders as its own logical
//!   row (structure fidelity: a multi-line prompt keeps its shape and is never
//!   collapsed by markdown soft-wrapping), padded one column on each side and
//!   background-tinted edge to edge.
//! - **Assistant message** ([`assistant_lines`]) — text blocks as rendered
//!   markdown; thinking blocks dimmed + italic *before* the body (or a static
//!   `Thinking...` label when collapsed); an error footnote (`Error: <msg>` /
//!   `Operation aborted`) below the body when the message stopped with
//!   [`StopReason::Error`]/[`StopReason::Aborted`] **and carries no tool call**
//!   (a tool-call message owns its own error UI, so no footnote there).
//! - **Streaming preview** ([`stream_preview_lines`]) — the mid-stream partial
//!   rendered in the live active area. An *unclosed* code fence mid-stream is
//!   closed defensively before rendering ([`close_unclosed_fence`]) so the
//!   code-block styling stays contained in the preview and never leaks past the
//!   partial; on `MessageEnd` the final snapshot commits through
//!   [`assistant_lines`] and settles to one complete code block.

use hand_tui::rt::components::syntax_highlight::default_markdown_theme;
use hand_tui::rt::components::{MarkdownTheme, render_markdown};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use model::{AssistantContentBlock, AssistantMessage, StopReason};

use crate::modes::interactive::theme::ThemePalette;

/// The static label shown in place of a thinking block when thinking is
/// collapsed (Ctrl+T). Mirrors the legacy `DEFAULT_HIDDEN_THINKING_LABEL`.
pub const HIDDEN_THINKING_LABEL: &str = "Thinking...";

/// The markdown theme used for message bodies: the default rt theme with the
/// syntax highlighter wired into the fenced-code hook.
fn body_theme() -> MarkdownTheme {
    default_markdown_theme()
}

/// Render an immediately-echoed user prompt as a tinted bubble.
///
/// Structure fidelity is the contract: each *input* line (split on `\n`) is
/// rendered as its own logical row, so a multi-line prompt keeps its shape and
/// markdown soft-wrapping never merges two input lines into one. Every row —
/// the one-column left/right padding included — carries the palette's
/// user-message background tint so the bubble reads as one continuous
/// background block, and text takes the palette's contrasting user-message
/// foreground so it stays legible on the tint.
///
/// `width` is the render width; a body line is rendered as plain styled text
/// (not markdown-parsed) so a lone `#` or `-` in a prompt is echoed verbatim
/// rather than reinterpreted as a heading or list — the echo must show what the
/// user typed. `palette` supplies the bubble's tint and text colour from the
/// active theme (the default palette keeps the historical `#343541` look).
#[must_use]
pub fn user_bubble_lines(text: &str, width: u16, palette: &ThemePalette) -> Vec<Line<'static>> {
    let user_bg = palette.user_message_bg;
    let bg = Style::default().bg(user_bg);
    let body = Style::default().bg(user_bg).fg(palette.user_message_text);
    let pad_cols = usize::from(width.max(2)).saturating_sub(2);

    // A blank tinted row pads the top and bottom of the bubble; interior rows
    // are the body text, one per input line, indented one column and tinted.
    let blank = || Line::from(Span::styled(" ".repeat(width.into()), bg));

    let mut lines = vec![blank()];
    for input_line in text.split('\n') {
        // Left pad, body, right-fill to the width so the tint reaches the edge.
        let visible = input_line.chars().count();
        let right_fill = pad_cols.saturating_sub(visible);
        let spans = vec![
            Span::styled(" ".to_string(), bg),
            Span::styled(input_line.to_string(), body),
            Span::styled(" ".repeat(right_fill + 1), bg),
        ];
        lines.push(Line::from(spans));
    }
    lines.push(blank());
    lines
}

/// Render a finalized assistant message into its scrollback lines.
///
/// Walks the message's content blocks in order:
///
/// - **Thinking** blocks render *before* the body: dimmed + italic markdown when
///   expanded, or the static [`HIDDEN_THINKING_LABEL`] (also dimmed + italic)
///   when `hide_thinking` is set. Empty thinking blocks are skipped.
/// - **Text** blocks render as markdown through the body theme (syntax-
///   highlighted code fences included).
/// - **ToolCall** blocks render nothing here — the tool-execution component owns
///   their UI.
///
/// An error footnote is appended below the body when the message stopped with
/// [`StopReason::Error`] (`Error: <msg>`) or [`StopReason::Aborted`]
/// (`Operation aborted`, or the carried abort reason) **and** the message
/// carries no tool call. A message *with* a tool call gets no footnote — the
/// tool frame surfaces the failure itself.
///
/// Returns an empty vector for a message with no visible content (e.g. the empty
/// `MessageStart` snapshot), so no blank block lands in scrollback.
#[must_use]
pub fn assistant_lines(
    message: &AssistantMessage,
    hide_thinking: bool,
    width: u16,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let theme = body_theme();
    let mut lines: Vec<Line<'static>> = Vec::new();

    for block in &message.content {
        match block {
            AssistantContentBlock::Thinking(t) if !t.thinking.trim().is_empty() => {
                lines.extend(thinking_lines(
                    t.thinking.trim(),
                    hide_thinking,
                    width,
                    &theme,
                    palette,
                ));
            }
            AssistantContentBlock::Text(t) if !t.text.trim().is_empty() => {
                lines.extend(render_markdown(t.text.trim(), width, &theme));
            }
            // Empty text/thinking and every tool call render nothing here.
            _ => {}
        }
    }

    if let Some(footnote) = error_footnote(message, palette) {
        lines.push(Line::default());
        lines.push(footnote);
    }

    lines
}

/// Render a thinking block: dimmed + italic markdown, or the collapsed static
/// label. The whole block takes the dim-italic style; when expanded the markdown
/// spans are re-styled so the dim-italic wins edge to edge.
fn thinking_lines(
    thinking: &str,
    hide: bool,
    width: u16,
    theme: &MarkdownTheme,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let dim_italic = Style::default()
        .fg(palette.thinking_text)
        .add_modifier(Modifier::ITALIC);

    if hide {
        return vec![Line::from(Span::styled(
            HIDDEN_THINKING_LABEL.to_string(),
            dim_italic,
        ))];
    }

    render_markdown(thinking, width, theme)
        .into_iter()
        .map(|line| {
            let spans: Vec<Span<'static>> = line
                .spans
                .into_iter()
                .map(|span| {
                    // Patch the dim-italic style *over* the markdown span's own,
                    // so the thinking block reads uniformly dimmed while keeping
                    // any structural spacing the markdown produced.
                    Span::styled(span.content.into_owned(), dim_italic.patch(span.style))
                })
                .collect();
            Line::from(spans)
        })
        .collect()
}

/// The error footnote for a message, or `None` when it stopped normally or
/// carries a tool call (which owns its own error UI).
fn error_footnote(message: &AssistantMessage, palette: &ThemePalette) -> Option<Line<'static>> {
    let has_tool_call = message
        .content
        .iter()
        .any(|b| matches!(b, AssistantContentBlock::ToolCall(_)));
    if has_tool_call {
        return None;
    }

    let error_style = Style::default().fg(palette.error);
    match message.stop_reason {
        StopReason::Error => {
            let msg = message.error_message.as_deref().unwrap_or("Unknown error");
            Some(Line::from(Span::styled(
                format!("Error: {msg}"),
                error_style,
            )))
        }
        StopReason::Aborted => {
            let msg = message
                .error_message
                .as_deref()
                .filter(|m| *m != "Request was aborted")
                .unwrap_or("Operation aborted");
            Some(Line::from(Span::styled(msg.to_string(), error_style)))
        }
        _ => None,
    }
}

/// Render a mid-stream assistant partial for the live active-area preview.
///
/// The partial is passed through [`close_unclosed_fence`] first: while a code
/// fence is open mid-stream, the closing ``` has not yet arrived, so a naive
/// render would treat the *rest of the transcript* (and anything the renderer
/// appends) as code — bleeding code styling past the partial. Closing the fence
/// defensively keeps the code-block styling contained to just the code lines the
/// partial actually contains. On `MessageEnd` the real (closed) snapshot commits
/// through [`assistant_lines`] and settles to exactly one complete code block.
///
/// Thinking blocks are not distinguished here (a preview shows the raw partial
/// as it streams); the finalized commit applies the thinking treatment.
#[must_use]
pub fn stream_preview_lines(partial: &str, width: u16) -> Vec<Line<'static>> {
    if partial.trim().is_empty() {
        return Vec::new();
    }
    let closed = close_unclosed_fence(partial);
    render_markdown(&closed, width, &body_theme())
}

/// If `source` has an odd number of ```` ``` ```` fences (an unclosed code
/// block), append a closing fence so a markdown render treats only the buffered
/// code lines as code and does not bleed the code style onto whatever follows.
///
/// Counts fence *markers* — a line whose first non-whitespace run is three or
/// more backticks — rather than raw backtick occurrences, so an inline `` `x` ``
/// never miscounts as a block fence. Returns `source` unchanged when the fences
/// are balanced.
#[must_use]
pub fn close_unclosed_fence(source: &str) -> String {
    let open = source.lines().filter(|l| is_fence_marker(l)).count();
    if open % 2 == 1 {
        // Ensure the appended fence starts on its own line.
        let mut closed = source.to_string();
        if !closed.ends_with('\n') {
            closed.push('\n');
        }
        closed.push_str("```");
        closed
    } else {
        source.to_string()
    }
}

/// Whether `line` opens or closes a fenced code block: its first non-whitespace
/// run is three or more backticks (the CommonMark fenced-code marker).
fn is_fence_marker(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("```") && trimmed.chars().take_while(|c| *c == '`').count() >= 3
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::types::{
        Api, AssistantContentBlock, AssistantMessage, Provider, StopReason, TextContent,
        ThinkingContent, ToolCall, Usage,
    };
    use ratatui::style::Color;

    /// The default palette — the historical hard-coded look, so the existing
    /// assertions keep pinning the same colours.
    fn pal() -> ThemePalette {
        ThemePalette::default()
    }

    /// The plain concatenated text of a line.
    fn text_of(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// The joined plain text of every line.
    fn joined(lines: &[Line<'_>]) -> String {
        lines.iter().map(text_of).collect::<Vec<_>>().join("\n")
    }

    fn message(content: Vec<AssistantContentBlock>, stop_reason: StopReason) -> AssistantMessage {
        AssistantMessage {
            role: "assistant".to_string(),
            content,
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

    fn text_block(s: &str) -> AssistantContentBlock {
        AssistantContentBlock::Text(TextContent::new(s))
    }

    fn thinking_block(s: &str) -> AssistantContentBlock {
        AssistantContentBlock::Thinking(ThinkingContent::new(s))
    }

    // --- user bubble (VAL-CHAT-001 / VAL-CHAT-032) -----------------------

    #[test]
    fn user_bubble_echoes_body_text() {
        let lines = user_bubble_lines("hello world", 40, &pal());
        assert!(
            joined(&lines).contains("hello world"),
            "bubble must echo the prompt: {:?}",
            joined(&lines)
        );
    }

    #[test]
    fn user_bubble_multiline_keeps_one_row_per_input_line() {
        // Structure fidelity: two input lines stay two distinct body rows, never
        // merged by markdown soft-wrapping.
        let lines = user_bubble_lines("line one\nline two", 40, &pal());
        let bodies: Vec<String> = lines
            .iter()
            .map(text_of)
            .filter(|t| t.contains("line"))
            .collect();
        assert_eq!(
            bodies.len(),
            2,
            "each input line is its own row: {bodies:?}"
        );
        assert!(bodies[0].contains("line one"));
        assert!(bodies[1].contains("line two"));
    }

    #[test]
    fn user_bubble_tints_every_row_background() {
        // Every row — padding included — carries the tint background so the
        // bubble reads as one continuous block.
        let lines = user_bubble_lines("hi", 20, &pal());
        assert!(!lines.is_empty());
        for line in &lines {
            assert!(
                line.spans
                    .iter()
                    .all(|s| s.style.bg == Some(pal().user_message_bg)),
                "every span in a bubble row must be tinted: {line:?}"
            );
        }
    }

    #[test]
    fn user_bubble_body_line_fills_to_width() {
        // The body row spans the full width so the tint reaches the right edge,
        // not just the length of the text.
        let width = 30u16;
        let lines = user_bubble_lines("short", width, &pal());
        let body = lines
            .iter()
            .find(|l| text_of(l).contains("short"))
            .expect("body row");
        let body_width: usize = body.spans.iter().map(|s| s.content.chars().count()).sum();
        assert_eq!(
            body_width,
            usize::from(width),
            "row fills the tint to width"
        );
    }

    #[test]
    fn user_bubble_echoes_markdown_source_verbatim() {
        // A `#` prompt is echoed literally, NOT reinterpreted as a heading (the
        // echo shows what the user typed).
        let lines = user_bubble_lines("# not a heading", 40, &pal());
        assert!(
            joined(&lines).contains("# not a heading"),
            "the leading # must survive verbatim: {:?}",
            joined(&lines)
        );
    }

    // --- theme application (VAL-COMPAT-004) ------------------------------

    #[test]
    fn user_bubble_tints_from_the_palette() {
        // The default palette keeps the historical tint; a custom palette
        // recolours the bubble — the render function consumes the theme.
        let default_lines = user_bubble_lines("hi", 20, &ThemePalette::default());
        assert!(
            default_lines.iter().all(|l| l
                .spans
                .iter()
                .all(|s| s.style.bg == Some(Color::Rgb(52, 53, 65)))),
            "default palette keeps the #343541 tint"
        );

        let neon = ThemePalette {
            user_message_bg: Color::Rgb(0x12, 0x00, 0x1f),
            user_message_text: Color::Rgb(0xf0, 0xe6, 0xff),
            ..ThemePalette::default()
        };
        let themed = user_bubble_lines("hi", 20, &neon);
        assert!(
            themed.iter().all(|l| l
                .spans
                .iter()
                .all(|s| s.style.bg == Some(Color::Rgb(0x12, 0x00, 0x1f)))),
            "custom palette recolours the bubble tint"
        );
        let body = themed
            .iter()
            .find(|l| text_of(l).contains("hi"))
            .expect("body row");
        assert!(
            body.spans
                .iter()
                .any(|s| s.style.fg == Some(Color::Rgb(0xf0, 0xe6, 0xff))),
            "custom palette recolours the bubble text"
        );
    }

    #[test]
    fn thinking_and_error_take_the_palette() {
        // Thinking text and the error footnote colour from the palette, so a
        // custom theme retints them while the default keeps the historical look.
        let neon = ThemePalette {
            thinking_text: Color::Rgb(0x00, 0xff, 0xff),
            error: Color::Rgb(0xff, 0x00, 0x3c),
            ..ThemePalette::default()
        };
        let mut msg = message(
            vec![thinking_block("pondering"), text_block("answer")],
            StopReason::Error,
        );
        msg.error_message = Some("boom".to_string());
        let lines = assistant_lines(&msg, false, 40, &neon);
        assert!(
            lines.iter().any(|l| l
                .spans
                .iter()
                .any(|s| s.style.fg == Some(Color::Rgb(0x00, 0xff, 0xff)))),
            "thinking retints from the palette"
        );
        assert!(
            lines.iter().any(|l| l
                .spans
                .iter()
                .any(|s| s.style.fg == Some(Color::Rgb(0xff, 0x00, 0x3c)))),
            "error footnote retints from the palette"
        );
    }

    // --- assistant markdown body (VAL-CHAT-002) --------------------------

    #[test]
    fn assistant_renders_markdown_body() {
        let msg = message(vec![text_block("**bold** text")], StopReason::Stop);
        let lines = assistant_lines(&msg, false, 40, &pal());
        assert!(joined(&lines).contains("bold"));
        // A bold span is present (markdown parsed, not echoed raw).
        assert!(
            lines.iter().any(|l| l
                .spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::BOLD))),
            "markdown bold must be styled"
        );
    }

    #[test]
    fn assistant_code_fence_is_bordered_and_highlighted() {
        let msg = message(
            vec![text_block("```rust\nfn main() {}\n```")],
            StopReason::Stop,
        );
        let out = joined(&assistant_lines(&msg, false, 40, &pal()));
        assert!(out.contains("# lang: rust"), "lang label: {out:?}");
        assert!(
            out.contains('┌') && out.contains('└'),
            "code borders: {out:?}"
        );
        assert!(out.contains("fn main"), "code body: {out:?}");
    }

    #[test]
    fn empty_assistant_snapshot_renders_nothing() {
        let msg = message(vec![text_block("")], StopReason::Stop);
        assert!(assistant_lines(&msg, false, 40, &pal()).is_empty());
    }

    // --- thinking blocks (VAL-CHAT-008) ----------------------------------

    #[test]
    fn thinking_renders_dim_italic_before_body() {
        let msg = message(
            vec![thinking_block("pondering"), text_block("answer")],
            StopReason::Stop,
        );
        let lines = assistant_lines(&msg, false, 40, &pal());
        let out = joined(&lines);
        assert!(out.contains("pondering"), "thinking body present: {out:?}");
        // Thinking precedes the answer.
        let think_idx = lines.iter().position(|l| text_of(l).contains("pondering"));
        let ans_idx = lines.iter().position(|l| text_of(l).contains("answer"));
        assert!(
            think_idx < ans_idx,
            "thinking must render before the body: {think_idx:?} vs {ans_idx:?}"
        );
        // The thinking line is dimmed + italic.
        let think = &lines[think_idx.unwrap()];
        assert!(
            think
                .spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::ITALIC)
                    && s.style.fg == Some(Color::DarkGray)),
            "thinking must be dim + italic: {think:?}"
        );
    }

    #[test]
    fn collapsed_thinking_shows_static_label_and_hides_body() {
        let msg = message(
            vec![thinking_block("secret reasoning"), text_block("answer")],
            StopReason::Stop,
        );
        let out = joined(&assistant_lines(&msg, true, 40, &pal()));
        assert!(out.contains(HIDDEN_THINKING_LABEL), "static label: {out:?}");
        assert!(
            !out.contains("secret reasoning"),
            "collapsed thinking must not leak the body: {out:?}"
        );
        // The body still renders.
        assert!(out.contains("answer"), "body still shows: {out:?}");
    }

    // --- error footnote (VAL-CHAT-029) -----------------------------------

    #[test]
    fn error_stop_reason_appends_red_error_footnote() {
        let mut msg = message(vec![text_block("partial")], StopReason::Error);
        msg.error_message = Some("rate limit".to_string());
        let lines = assistant_lines(&msg, false, 40, &pal());
        let footnote = lines
            .iter()
            .find(|l| text_of(l).contains("Error: rate limit"))
            .expect("error footnote present");
        assert!(
            footnote
                .spans
                .iter()
                .any(|s| s.style.fg == Some(Color::Red)),
            "footnote must be red: {footnote:?}"
        );
    }

    #[test]
    fn aborted_stop_reason_appends_operation_aborted_footnote() {
        let msg = message(vec![text_block("partial")], StopReason::Aborted);
        let out = joined(&assistant_lines(&msg, false, 40, &pal()));
        assert!(
            out.contains("Operation aborted"),
            "default abort label: {out:?}"
        );
    }

    #[test]
    fn aborted_uses_carried_reason_over_default() {
        let mut msg = message(vec![text_block("partial")], StopReason::Aborted);
        msg.error_message = Some("user cancelled".to_string());
        let out = joined(&assistant_lines(&msg, false, 40, &pal()));
        assert!(out.contains("user cancelled"), "carried reason: {out:?}");
    }

    #[test]
    fn error_message_with_tool_call_gets_no_footnote() {
        // A tool-call message owns its own error UI, so no footnote here.
        let msg = message(
            vec![
                text_block("running a tool"),
                AssistantContentBlock::ToolCall(ToolCall::new(
                    "id-1",
                    "Read",
                    serde_json::json!({"path": "/x"}),
                )),
            ],
            StopReason::Error,
        );
        let out = joined(&assistant_lines(&msg, false, 40, &pal()));
        assert!(
            !out.contains("Error:"),
            "tool-call message must not carry a footnote: {out:?}"
        );
    }

    #[test]
    fn normal_stop_reason_has_no_footnote() {
        let msg = message(vec![text_block("all good")], StopReason::Stop);
        let out = joined(&assistant_lines(&msg, false, 40, &pal()));
        assert!(
            !out.contains("Error"),
            "no footnote on a clean stop: {out:?}"
        );
    }

    // --- unclosed fence containment (VAL-CHAT-033) -----------------------

    #[test]
    fn odd_fence_count_is_closed() {
        // One open fence → a closing fence is appended.
        let closed = close_unclosed_fence("here is code:\n```rust\nfn main() {}");
        assert_eq!(
            closed.lines().filter(|l| is_fence_marker(l)).count(),
            2,
            "an unclosed fence must be closed: {closed:?}"
        );
    }

    #[test]
    fn balanced_fences_are_left_untouched() {
        let src = "```rust\nfn main() {}\n```";
        assert_eq!(close_unclosed_fence(src), src, "balanced fences unchanged");
    }

    #[test]
    fn inline_backticks_are_not_counted_as_a_fence() {
        // An inline `code` span must not be miscounted as a block fence.
        let src = "use `cargo test` to run";
        assert_eq!(close_unclosed_fence(src), src, "inline code is not a fence");
    }

    #[test]
    fn stream_preview_contains_unclosed_code_in_a_bordered_block() {
        // Mid-stream, an open fence renders as a *complete* bordered block in the
        // preview — the styling is contained, not bled onto later content.
        let preview = stream_preview_lines("intro\n```rust\nfn main() {", 40);
        let out = joined(&preview);
        assert!(out.contains("fn main"), "code body present: {out:?}");
        assert!(
            out.contains('┌') && out.contains('└'),
            "the preview closes the block so it is bordered top and bottom: {out:?}"
        );
    }

    #[test]
    fn stream_preview_settles_to_one_block_when_final_arrives() {
        // The preview (unclosed) and the final commit (closed) both yield exactly
        // one bordered code block — settling, not doubling.
        let final_msg = message(
            vec![text_block("intro\n```rust\nfn main() {}\n```")],
            StopReason::Stop,
        );
        let settled = joined(&assistant_lines(&final_msg, false, 40, &pal()));
        assert_eq!(
            settled.matches('┌').count(),
            1,
            "exactly one code block top border after settle: {settled:?}"
        );
        assert_eq!(
            settled.matches('└').count(),
            1,
            "exactly one code block bottom border after settle: {settled:?}"
        );
    }

    #[test]
    fn empty_stream_preview_renders_nothing() {
        assert!(stream_preview_lines("", 40).is_empty());
        assert!(stream_preview_lines("   \n  ", 40).is_empty());
    }
}
