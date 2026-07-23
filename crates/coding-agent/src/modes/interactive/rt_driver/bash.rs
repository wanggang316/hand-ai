//! Inline `!cmd` bash rendering on the rt stack.
//!
//! The rt-native port of the legacy `components/bash_execution` renderer. Where
//! the legacy component painted a stateful [`hand_tui::Component`] to
//! `Vec<String>` of ANSI-escaped lines, this renders an owned
//! [`Line<'static>`] block — spans carrying a ratatui [`Style`] — the model the
//! rt scheduler commits into native scrollback. The rt driver has no live
//! per-cell tick loop for a mounted bash cell; instead a completed command
//! renders once as a finalized block (header + output + exit-code footer), while
//! the *running* loader rides the driver's shared streaming flag (the bordered
//! working-loader in the active area), so nothing needs to be re-committed as
//! the process streams.
//!
//! # Behavioural signatures (pinned from legacy, VAL-CHAT-009 / VAL-CHAT-010)
//!
//! - **Header** — a bold `$ <command>` row in the bash accent (cyan), or the
//!   muted/dim accent when the command is `!!`-prefixed (excluded from LLM
//!   context). The whole frame — top/bottom rule and header — takes the dim
//!   accent for `!!`, so a glance distinguishes a context-excluded run.
//! - **Output body** — the process output, ANSI-stripped and `\r`-normalised,
//!   in a muted foreground. When collapsed (the default finalized view) only the
//!   last [`PREVIEW_ROWS`] *visual* rows are shown, with a `… N more lines` hint;
//!   expanded shows the whole (context-truncated) buffer.
//! - **Exit-code / status footer** — `(exit N)` in red for a non-zero exit,
//!   `(cancelled)` in yellow for an aborted run, nothing for a clean `exit 0`.
//! - **Truncation footnote** — when context truncation dropped output, a yellow
//!   `Output truncated. Full output: <path>` row naming the on-disk file the
//!   full output was spilled to (the path exists on disk).
//!
//! The empty-command case (`!` / `!!` with no command) is not rendered here — it
//! is a yellow `[bash] empty command` status line the driver commits directly,
//! since there is no frame to draw.

use hand_tui::rt::history::wrap_lines;
use hand_tui::utils::strip_ansi;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Visual-row budget for the collapsed output preview — the last N rendered
/// rows are kept so the freshest output is always visible. Mirrors the legacy
/// `PREVIEW_LINES`.
pub const PREVIEW_ROWS: usize = 20;

/// Bash accent — cyan, for the frame rule and command header of a normal `!cmd`.
const BASH_ACCENT: Color = Color::Cyan;
/// Dim accent — dark gray, for a `!!cmd` frame (excluded from the LLM context)
/// and for muted output text.
const DIM_ACCENT: Color = Color::DarkGray;
/// Yellow — cancellation and truncation notices.
const WARNING: Color = Color::Yellow;
/// Red — a non-zero exit code.
const ERROR: Color = Color::Red;

/// How a finished inline bash command ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BashOutcome {
    /// The process exited with the carried code (`None` when unknown).
    Exited(Option<i32>),
    /// The run was cancelled (Esc / Ctrl+C) before it finished.
    Cancelled,
}

/// A parsed inline bash submission: the command to run and whether it is
/// excluded from the LLM context (the `!!` prefix).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedBash {
    /// The command text, trimmed of the `!` / `!!` prefix and surrounding
    /// whitespace.
    pub command: String,
    /// Whether the command is `!!`-prefixed (excluded from the LLM context),
    /// which switches the frame to the dim accent.
    pub exclude_from_context: bool,
}

/// Parse an editor submission that begins with `!` into an inline bash command.
///
/// `!!cmd` sets [`ParsedBash::exclude_from_context`]; a single `!cmd` does not.
/// The command is trimmed. A bare `!` or `!!` (empty command) yields a
/// `ParsedBash` with an empty `command` — the caller surfaces the yellow
/// `[bash] empty command` notice rather than running an empty shell.
///
/// Returns `None` when `raw` does not start with `!` (not a bash submission).
#[must_use]
pub fn parse_inline_bash(raw: &str) -> Option<ParsedBash> {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("!!") {
        Some(ParsedBash {
            command: rest.trim().to_string(),
            exclude_from_context: true,
        })
    } else if let Some(rest) = trimmed.strip_prefix('!') {
        Some(ParsedBash {
            command: rest.trim().to_string(),
            exclude_from_context: false,
        })
    } else {
        None
    }
}

/// The yellow `[bash] empty command` notice for a bare `!` / `!!` submission.
#[must_use]
pub fn empty_command_notice() -> Line<'static> {
    Line::from(Span::styled(
        "[bash] empty command".to_string(),
        Style::default().fg(WARNING),
    ))
}

/// Render a finished inline bash command into its finalized scrollback block.
///
/// `output` is the raw process output (it is ANSI-stripped and `\r`-normalised
/// here). `full_output_path` is `Some` only when context truncation spilled the
/// full output to disk; it drives the yellow truncation footnote. `expanded`
/// shows the whole buffer; collapsed (the default) shows only the last
/// [`PREVIEW_ROWS`] visual rows.
///
/// The whole frame takes the dim accent when `parsed.exclude_from_context`
/// (a `!!cmd`), so a context-excluded run reads differently at a glance.
#[must_use]
pub fn bash_block_lines(
    parsed: &ParsedBash,
    output: &str,
    outcome: BashOutcome,
    full_output_path: Option<&str>,
    expanded: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let accent = if parsed.exclude_from_context {
        DIM_ACCENT
    } else {
        BASH_ACCENT
    };
    let rule = rule_line(accent, width);

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(rule.clone());

    // Header: bold `$ <command>` in the accent color.
    lines.push(Line::from(Span::styled(
        format!("$ {}", parsed.command),
        Style::default().fg(accent).add_modifier(Modifier::BOLD),
    )));

    // Output body: strip ANSI, normalise CR, muted foreground, one row per
    // logical output line — then collapse to the last PREVIEW_ROWS *visual*
    // rows unless expanded.
    let body_style = Style::default().fg(DIM_ACCENT);
    let clean = strip_ansi(output).replace("\r\n", "\n").replace('\r', "\n");
    let body: Vec<Line<'static>> = clean
        .split('\n')
        .map(|l| Line::from(Span::styled(l.to_string(), body_style)))
        .collect();

    let (shown, hidden_rows) = collapse_tail(&body, expanded, width);
    if hidden_rows > 0 {
        lines.push(Line::from(Span::styled(
            format!("… {hidden_rows} more lines (ctrl+o to expand)"),
            body_style,
        )));
    }
    lines.extend(shown);

    // Status footer: exit code / cancellation, then any truncation footnote.
    if let Some(footer) = status_line(outcome) {
        lines.push(footer);
    }
    if let Some(path) = full_output_path {
        lines.push(Line::from(Span::styled(
            format!("Output truncated. Full output: {path}"),
            Style::default().fg(WARNING),
        )));
    }

    lines.push(rule);
    lines
}

/// A full-width horizontal rule in `color`, for the top and bottom frame edges.
fn rule_line(color: Color, width: u16) -> Line<'static> {
    let cols = usize::from(width.max(1));
    Line::from(Span::styled("─".repeat(cols), Style::default().fg(color)))
}

/// Collapse `body` to the last [`PREVIEW_ROWS`] *visual* rows (unless
/// `expanded`), returning the kept rows and the count of hidden **visual** rows.
///
/// Collapse is measured on wrapped (visual) rows so a single very long output
/// line that wraps to many rows is counted as it renders, matching the legacy
/// visual-truncation contract. When expanded, or when the body already fits, the
/// body is returned whole with zero hidden rows.
fn collapse_tail(
    body: &[Line<'static>],
    expanded: bool,
    width: u16,
) -> (Vec<Line<'static>>, usize) {
    if expanded {
        return (body.to_vec(), 0);
    }
    let wrapped = wrap_lines(body, width.max(1));
    if wrapped.len() <= PREVIEW_ROWS {
        return (wrapped, 0);
    }
    let hidden = wrapped.len() - PREVIEW_ROWS;
    let tail = wrapped[hidden..].to_vec();
    (tail, hidden)
}

/// The status footer row for a finished run, or `None` for a clean `exit 0`
/// (which needs no annotation).
fn status_line(outcome: BashOutcome) -> Option<Line<'static>> {
    match outcome {
        BashOutcome::Cancelled => Some(Line::from(Span::styled(
            "(cancelled)".to_string(),
            Style::default().fg(WARNING),
        ))),
        BashOutcome::Exited(Some(0)) => None,
        BashOutcome::Exited(code) => {
            let code = code.map_or_else(|| "?".to_string(), |c| c.to_string());
            Some(Line::from(Span::styled(
                format!("(exit {code})"),
                Style::default().fg(ERROR),
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn joined(lines: &[Line<'_>]) -> String {
        lines.iter().map(text_of).collect::<Vec<_>>().join("\n")
    }

    fn normal(cmd: &str) -> ParsedBash {
        ParsedBash {
            command: cmd.to_string(),
            exclude_from_context: false,
        }
    }

    // --- parsing (VAL-CHAT-009) ------------------------------------------

    #[test]
    fn parse_single_bang_is_included_in_context() {
        let p = parse_inline_bash("!echo hi").expect("bash submission");
        assert_eq!(p.command, "echo hi");
        assert!(!p.exclude_from_context);
    }

    #[test]
    fn parse_double_bang_is_excluded_from_context() {
        let p = parse_inline_bash("!!secret cmd").expect("bash submission");
        assert_eq!(p.command, "secret cmd");
        assert!(p.exclude_from_context, "!! excludes from context");
    }

    #[test]
    fn parse_trims_whitespace_around_command() {
        let p = parse_inline_bash("!   ls -la   ").expect("bash submission");
        assert_eq!(p.command, "ls -la");
    }

    #[test]
    fn parse_bare_bang_yields_empty_command() {
        assert_eq!(parse_inline_bash("!").unwrap().command, "");
        assert_eq!(parse_inline_bash("!!").unwrap().command, "");
        assert_eq!(parse_inline_bash("!   ").unwrap().command, "");
    }

    #[test]
    fn parse_non_bang_is_not_a_bash_submission() {
        assert!(parse_inline_bash("echo hi").is_none());
        assert!(parse_inline_bash("/quit").is_none());
        assert!(parse_inline_bash("").is_none());
    }

    #[test]
    fn empty_command_notice_is_yellow() {
        let line = empty_command_notice();
        assert_eq!(text_of(&line), "[bash] empty command");
        assert!(line.spans.iter().any(|s| s.style.fg == Some(WARNING)));
    }

    // --- header / body / exit (VAL-CHAT-009) -----------------------------

    #[test]
    fn renders_dollar_header_and_output() {
        let lines = bash_block_lines(
            &normal("echo hi"),
            "hi\nthere",
            BashOutcome::Exited(Some(0)),
            None,
            false,
            40,
        );
        let out = joined(&lines);
        assert!(out.contains("$ echo hi"), "header: {out:?}");
        assert!(out.contains("hi"), "output line: {out:?}");
        assert!(out.contains("there"), "output line: {out:?}");
    }

    #[test]
    fn header_is_bold_and_cyan_for_single_bang() {
        let lines = bash_block_lines(&normal("ls"), "", BashOutcome::Exited(Some(0)), None, false, 40);
        let header = lines
            .iter()
            .find(|l| text_of(l).contains("$ ls"))
            .expect("header row");
        assert!(header.spans.iter().any(|s| {
            s.style.fg == Some(BASH_ACCENT) && s.style.add_modifier.contains(Modifier::BOLD)
        }));
    }

    #[test]
    fn nonzero_exit_shows_red_exit_code() {
        let lines = bash_block_lines(
            &normal("false"),
            "",
            BashOutcome::Exited(Some(2)),
            None,
            false,
            40,
        );
        let footer = lines
            .iter()
            .find(|l| text_of(l).contains("exit 2"))
            .expect("exit-code footer");
        assert!(
            footer.spans.iter().any(|s| s.style.fg == Some(ERROR)),
            "exit code must be red: {footer:?}"
        );
    }

    #[test]
    fn zero_exit_shows_no_status_footer() {
        let lines = bash_block_lines(
            &normal("true"),
            "ok",
            BashOutcome::Exited(Some(0)),
            None,
            false,
            40,
        );
        assert!(
            !joined(&lines).contains("exit"),
            "a clean exit needs no code annotation: {:?}",
            joined(&lines)
        );
    }

    #[test]
    fn cancelled_run_shows_yellow_cancelled_status() {
        let lines = bash_block_lines(
            &normal("sleep 100"),
            "",
            BashOutcome::Cancelled,
            None,
            false,
            40,
        );
        let footer = lines
            .iter()
            .find(|l| text_of(l).contains("cancelled"))
            .expect("cancelled footer");
        assert!(footer.spans.iter().any(|s| s.style.fg == Some(WARNING)));
    }

    // --- !! dim border (VAL-CHAT-009) ------------------------------------

    #[test]
    fn double_bang_frame_uses_dim_accent_not_cyan() {
        let parsed = ParsedBash {
            command: "ls".to_string(),
            exclude_from_context: true,
        };
        let lines = bash_block_lines(&parsed, "", BashOutcome::Exited(Some(0)), None, false, 40);
        // The top rule and header take the dim accent; none carry the cyan
        // bash accent.
        let rule = &lines[0];
        assert!(
            rule.spans.iter().all(|s| s.style.fg == Some(DIM_ACCENT)),
            "!! top rule must be dim: {rule:?}"
        );
        assert!(
            !lines.iter().any(|l| l
                .spans
                .iter()
                .any(|s| s.style.fg == Some(BASH_ACCENT))),
            "!! frame must not use the cyan bash accent"
        );
    }

    #[test]
    fn single_bang_frame_uses_cyan_accent() {
        let lines = bash_block_lines(&normal("ls"), "", BashOutcome::Exited(Some(0)), None, false, 40);
        let rule = &lines[0];
        assert!(rule.spans.iter().all(|s| s.style.fg == Some(BASH_ACCENT)));
    }

    // --- collapse to last ~20 visual rows (VAL-CHAT-010) -----------------

    #[test]
    fn collapsed_output_keeps_only_the_last_preview_rows() {
        let output: String = (0..100)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let lines = bash_block_lines(
            &normal("seq"),
            &output,
            BashOutcome::Exited(Some(0)),
            None,
            false,
            80,
        );
        let out = joined(&lines);
        // The tail is present, the head is dropped.
        assert!(out.contains("line 99"), "tail visible: {out:?}");
        assert!(!out.contains("line 0\n"), "head dropped: {out:?}");
        // A hidden-rows hint names how many were collapsed.
        assert!(out.contains("more lines"), "hidden-rows hint: {out:?}");
    }

    #[test]
    fn expanded_output_shows_the_whole_buffer() {
        let output: String = (0..40)
            .map(|i| format!("row {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let lines = bash_block_lines(
            &normal("seq"),
            &output,
            BashOutcome::Exited(Some(0)),
            None,
            true,
            80,
        );
        let out = joined(&lines);
        assert!(out.contains("row 0"), "head present when expanded: {out:?}");
        assert!(out.contains("row 39"), "tail present when expanded: {out:?}");
        assert!(!out.contains("more lines"), "no collapse hint when expanded");
    }

    // --- truncation footnote path (VAL-CHAT-010) -------------------------

    #[test]
    fn truncation_footnote_names_the_full_output_path() {
        let lines = bash_block_lines(
            &normal("seq 1 5000"),
            "…tail of output…",
            BashOutcome::Exited(Some(0)),
            Some("/tmp/hand-bash-out.log"),
            false,
            80,
        );
        let footnote = lines
            .iter()
            .find(|l| text_of(l).contains("Output truncated."))
            .expect("truncation footnote");
        assert!(
            text_of(footnote).contains("/tmp/hand-bash-out.log"),
            "footnote names the path: {:?}",
            text_of(footnote)
        );
        assert!(
            footnote.spans.iter().any(|s| s.style.fg == Some(WARNING)),
            "footnote is yellow"
        );
    }

    #[test]
    fn no_footnote_when_output_was_not_truncated() {
        let lines = bash_block_lines(
            &normal("echo hi"),
            "hi",
            BashOutcome::Exited(Some(0)),
            None,
            false,
            80,
        );
        assert!(!joined(&lines).contains("Output truncated"));
    }

    // --- output sanitisation ---------------------------------------------

    #[test]
    fn output_is_ansi_stripped_and_cr_normalised() {
        let lines = bash_block_lines(
            &normal("cmd"),
            "hello\r\n\x1b[31mred\x1b[0m\rmore",
            BashOutcome::Exited(Some(0)),
            None,
            true,
            80,
        );
        let out = joined(&lines);
        assert!(!out.contains('\r'), "CR must be normalised: {out:?}");
        assert!(!out.contains("\x1b["), "ANSI must be stripped: {out:?}");
        assert!(out.contains("red"), "text survives strip: {out:?}");
    }
}
