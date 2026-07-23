//! Generic tool-execution rendering on the rt stack.
//!
//! The rt-native port of the legacy `components/{tool_execution, diff}`
//! renderers. A tool call is rendered as a **state-tinted box**: a tinted
//! background that flips between pending / success / failure, carrying the tool
//! name (bold), its args as pretty JSON, and the result text. `edit` / `write`
//! tools instead render their unified diff with `+`/`-` foreground coloring
//! (green added, red removed, dim context) so a code change reads at a glance.
//!
//! Where the legacy components painted ANSI-escaped `Vec<String>`, these render
//! owned [`Line<'static>`] blocks with a ratatui [`Style`] — the model the rt
//! scheduler commits into native scrollback. The background tint is carried as a
//! per-span `bg`, edge to edge on every row (padding included), so the box reads
//! as one continuous block the same way the user bubble does.
//!
//! # Image parity (VAL-IMG-019, Decision Log ⑤)
//!
//! A tool result carrying an image block is **not** shown as graphics in the
//! chat. The result text is produced through
//! [`get_text_output`](crate::tools::render_utils::get_text_output), which — on a
//! graphics-capable (kitty/iTerm2) terminal — *excludes* the image block from
//! the text entirely (zero graphics bytes reach the chat), and on a plain
//! terminal replaces it with a `[mime WxH]` indicator box. This is threaded here
//! so both personas stay at parity: kitty emits no image, plain shows the
//! indicator.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;
use similar::{ChangeTag, TextDiff};

/// Lifecycle state of a tool call, driving the box background tint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolState {
    /// Args streaming or the tool executing — a neutral tint.
    #[default]
    Pending,
    /// Result received without an error flag — a green-ish tint.
    Success,
    /// Result received with `is_error` — a red-ish tint.
    Failure,
}

// State backgrounds — muted truecolor tints so an explicit light body fg always
// wins on contrast, matching the legacy dark-theme values.
/// In-flight tool call background (`#282832`).
const PENDING_BG: Color = Color::Rgb(40, 40, 50);
/// Successful tool call background (`#283228`).
const SUCCESS_BG: Color = Color::Rgb(40, 50, 40);
/// Failed tool call background (`#3c2828`).
const FAILURE_BG: Color = Color::Rgb(60, 40, 40);
/// Bright cyan title, bold — the tool name.
const TITLE_FG: Color = Color::Rgb(120, 220, 220);
/// Light-grey body — args JSON and result text, readable on any tint.
const BODY_FG: Color = Color::Rgb(220, 220, 220);

/// Diff added-line foreground (green).
const DIFF_ADDED: Color = Color::Green;
/// Diff removed-line foreground (red).
const DIFF_REMOVED: Color = Color::Red;
/// Diff context-line foreground (dim gray).
const DIFF_CONTEXT: Color = Color::DarkGray;

impl ToolState {
    /// Resolve the state from the tool result's error flag. `None` (no result
    /// yet) is [`ToolState::Pending`].
    #[must_use]
    pub fn from_result(is_error: Option<bool>) -> Self {
        match is_error {
            None => ToolState::Pending,
            Some(true) => ToolState::Failure,
            Some(false) => ToolState::Success,
        }
    }

    fn background(self) -> Color {
        match self {
            ToolState::Pending => PENDING_BG,
            ToolState::Success => SUCCESS_BG,
            ToolState::Failure => FAILURE_BG,
        }
    }
}

/// Whether a tool renders its result as a unified diff (edit / write tools)
/// rather than the generic name/args/result box.
#[must_use]
pub fn is_diff_tool(tool_name: &str) -> bool {
    matches!(tool_name.to_ascii_lowercase().as_str(), "edit" | "write")
}

/// Render a tool call into its finalized scrollback block.
///
/// The box tints edge to edge in [`ToolState::background`] and carries the tool
/// name (bold cyan), the args as pretty JSON (skipped when the args are an empty
/// object — a bare `{}` is noise), and the result text (a diff for edit / write
/// tools, plain text otherwise). `result_text` is the already-parity-resolved
/// output (see the module docs on image parity).
#[must_use]
pub fn tool_box_lines(
    tool_name: &str,
    args: &Value,
    result_text: &str,
    state: ToolState,
    width: u16,
) -> Vec<Line<'static>> {
    let bg = state.background();
    let title_style = Style::default()
        .bg(bg)
        .fg(TITLE_FG)
        .add_modifier(Modifier::BOLD);
    let body_style = Style::default().bg(bg).fg(BODY_FG);

    // Frame the tinted rows with a blank tinted row top and bottom so the box
    // reads as one continuous block (the legacy BoxComponent padding).
    let mut out: Vec<Line<'static>> = vec![blank_row(bg, width)];
    out.push(padded_row(
        vec![Span::styled(tool_name.to_string(), title_style)],
        bg,
        width,
    ));

    let args_text = pretty_args(args);
    if !args_text.is_empty() {
        out.push(blank_row(bg, width));
        for line in args_text.split('\n') {
            out.push(padded_row(
                vec![Span::styled(line.to_string(), body_style)],
                bg,
                width,
            ));
        }
    }

    if !result_text.is_empty() {
        out.push(blank_row(bg, width));
        if is_diff_tool(tool_name) {
            for line in diff_lines(result_text, bg) {
                out.push(pad_existing(line, bg, width));
            }
        } else {
            for line in result_text.split('\n') {
                out.push(padded_row(
                    vec![Span::styled(line.to_string(), body_style)],
                    bg,
                    width,
                ));
            }
        }
    }

    out.push(blank_row(bg, width));
    out
}

/// Pretty-print args as JSON, or an empty string for an empty object (a bare
/// `{}` conveys nothing and is skipped).
fn pretty_args(args: &Value) -> String {
    match args {
        Value::Object(map) if map.is_empty() => String::new(),
        Value::Null => String::new(),
        _ => serde_json::to_string_pretty(args).unwrap_or_default(),
    }
}

/// A blank, fully-tinted row spanning the width.
fn blank_row(bg: Color, width: u16) -> Line<'static> {
    Line::from(Span::styled(
        " ".repeat(usize::from(width.max(1))),
        Style::default().bg(bg),
    ))
}

/// Wrap `spans` in a one-column-padded, right-filled row so the tint reaches
/// both edges.
fn padded_row(spans: Vec<Span<'static>>, bg: Color, width: u16) -> Line<'static> {
    let visible: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let pad_bg = Style::default().bg(bg);
    let inner_cols = usize::from(width.max(2)).saturating_sub(2);
    let right_fill = inner_cols.saturating_sub(visible);

    let mut out = vec![Span::styled(" ".to_string(), pad_bg)];
    out.extend(spans);
    out.push(Span::styled(" ".repeat(right_fill + 1), pad_bg));
    Line::from(out)
}

/// Ensure an already-styled row is padded / right-filled to the width so its
/// tint reaches both edges (used for pre-styled diff rows).
fn pad_existing(line: Line<'static>, bg: Color, width: u16) -> Line<'static> {
    // If the row is already a padded blank (equal width), leave it; otherwise
    // rebuild it padded. Detect a padded row by a leading single-space pad span.
    let visible: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
    let target = usize::from(width.max(1));
    if visible >= target {
        return line;
    }
    padded_row(line.spans, bg, width)
}

/// Render unified-diff `text` into styled rows: green `+` added, red `-`
/// removed, dim context. Each row keeps the tinted background of the box so the
/// diff sits inside the frame rather than punching a hole in the tint.
///
/// Intra-line word highlighting (inverse video on changed tokens) is applied
/// when a change block is exactly one removed + one added line — a single-line
/// modification — matching the legacy renderer.
fn diff_lines(text: &str, bg: Color) -> Vec<Line<'static>> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<Line<'static>> = Vec::new();

    let mut i = 0;
    while i < lines.len() {
        let Some(parsed) = parse_diff_line(lines[i]) else {
            out.push(styled_row(lines[i], DIFF_CONTEXT, bg));
            i += 1;
            continue;
        };

        match parsed.prefix {
            '-' => {
                let mut removed: Vec<ParsedLine> = Vec::new();
                while i < lines.len() {
                    match parse_diff_line(lines[i]) {
                        Some(p) if p.prefix == '-' => {
                            removed.push(p);
                            i += 1;
                        }
                        _ => break,
                    }
                }
                let mut added: Vec<ParsedLine> = Vec::new();
                while i < lines.len() {
                    match parse_diff_line(lines[i]) {
                        Some(p) if p.prefix == '+' => {
                            added.push(p);
                            i += 1;
                        }
                        _ => break,
                    }
                }

                if removed.len() == 1 && added.len() == 1 {
                    let (rspans, aspans) = intra_line_diff(
                        &removed[0].line_num,
                        &removed[0].content,
                        &added[0].line_num,
                        &added[0].content,
                        bg,
                    );
                    out.push(Line::from(rspans));
                    out.push(Line::from(aspans));
                } else {
                    for p in &removed {
                        out.push(styled_row(
                            &format!("-{} {}", p.line_num, replace_tabs(&p.content)),
                            DIFF_REMOVED,
                            bg,
                        ));
                    }
                    for p in &added {
                        out.push(styled_row(
                            &format!("+{} {}", p.line_num, replace_tabs(&p.content)),
                            DIFF_ADDED,
                            bg,
                        ));
                    }
                }
            }
            '+' => {
                out.push(styled_row(
                    &format!("+{} {}", parsed.line_num, replace_tabs(&parsed.content)),
                    DIFF_ADDED,
                    bg,
                ));
                i += 1;
            }
            _ => {
                out.push(styled_row(
                    &format!(" {} {}", parsed.line_num, replace_tabs(&parsed.content)),
                    DIFF_CONTEXT,
                    bg,
                ));
                i += 1;
            }
        }
    }

    out
}

/// A single styled diff/context row: `fg` foreground on the box `bg`.
fn styled_row(text: &str, fg: Color, bg: Color) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::default().fg(fg).bg(bg),
    ))
}

/// Render a single-line modification's removed/added rows with inverse video on
/// the changed tokens (a word-level diff). The equal portions render plainly;
/// the changed tokens carry the [`Modifier::REVERSED`] highlight.
fn intra_line_diff(
    rnum: &str,
    rcontent: &str,
    anum: &str,
    acontent: &str,
    bg: Color,
) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
    let old = replace_tabs(rcontent);
    let new = replace_tabs(acontent);
    let diff = TextDiff::configure().diff_words(old.as_str(), new.as_str());

    let removed_base = Style::default().fg(DIFF_REMOVED).bg(bg);
    let added_base = Style::default().fg(DIFF_ADDED).bg(bg);
    let removed_hi = removed_base.add_modifier(Modifier::REVERSED);
    let added_hi = added_base.add_modifier(Modifier::REVERSED);

    let mut rspans = vec![Span::styled(format!("-{rnum} "), removed_base)];
    let mut aspans = vec![Span::styled(format!("+{anum} "), added_base)];

    for change in diff.iter_all_changes() {
        let value = change.value().to_string();
        if value.is_empty() {
            continue;
        }
        match change.tag() {
            ChangeTag::Delete => rspans.push(Span::styled(value, removed_hi)),
            ChangeTag::Insert => aspans.push(Span::styled(value, added_hi)),
            ChangeTag::Equal => {
                rspans.push(Span::styled(value.clone(), removed_base));
                aspans.push(Span::styled(value, added_base));
            }
        }
    }

    (rspans, aspans)
}

/// Parsed pieces of a unified-diff line.
struct ParsedLine {
    prefix: char,
    line_num: String,
    content: String,
}

/// Parse `([+\- ])(\s*\d*)\s(.*)`; `None` for non-diff lines (headers, hunk
/// markers), which render as context.
fn parse_diff_line(line: &str) -> Option<ParsedLine> {
    let bytes = line.as_bytes();
    let first = *bytes.first()? as char;
    if !matches!(first, '+' | '-' | ' ') {
        return None;
    }

    let mut num_end = 1;
    let mut saw = false;
    while num_end < bytes.len() {
        let b = bytes[num_end];
        if b == b' ' || b.is_ascii_digit() {
            saw = true;
            num_end += 1;
        } else {
            break;
        }
    }
    if !saw || num_end >= bytes.len() {
        return None;
    }
    let sep_idx = num_end - 1;
    if bytes[sep_idx] != b' ' {
        return None;
    }

    Some(ParsedLine {
        prefix: first,
        line_num: line[1..sep_idx].trim_end_matches(' ').to_string(),
        content: line[num_end..].to_string(),
    })
}

/// Replace tabs with three spaces so terminal-width math stays sane on
/// tab-indented diffs.
fn replace_tabs(s: &str) -> String {
    s.replace('\t', "   ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn text_of(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn joined(lines: &[Line<'_>]) -> String {
        lines.iter().map(text_of).collect::<Vec<_>>().join("\n")
    }

    fn has_bg(lines: &[Line<'_>], bg: Color) -> bool {
        lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .any(|s| s.style.bg == Some(bg))
    }

    // --- state tint (VAL-CHAT-011) ---------------------------------------

    #[test]
    fn pending_state_uses_pending_tint() {
        let lines = tool_box_lines("read", &json!({"path": "/x"}), "", ToolState::Pending, 60);
        assert!(
            has_bg(&lines, PENDING_BG),
            "pending tint: {:?}",
            joined(&lines)
        );
    }

    #[test]
    fn success_state_uses_success_tint() {
        let lines = tool_box_lines("read", &json!({}), "ok", ToolState::Success, 60);
        assert!(has_bg(&lines, SUCCESS_BG));
        assert!(joined(&lines).contains("ok"));
    }

    #[test]
    fn failure_state_uses_failure_tint() {
        let lines = tool_box_lines("read", &json!({}), "boom", ToolState::Failure, 60);
        assert!(has_bg(&lines, FAILURE_BG));
        assert!(joined(&lines).contains("boom"));
    }

    #[test]
    fn state_from_result_maps_error_flag() {
        assert_eq!(ToolState::from_result(None), ToolState::Pending);
        assert_eq!(ToolState::from_result(Some(false)), ToolState::Success);
        assert_eq!(ToolState::from_result(Some(true)), ToolState::Failure);
    }

    // --- name / args / result (VAL-CHAT-011) -----------------------------

    #[test]
    fn renders_tool_name_bold() {
        let lines = tool_box_lines(
            "bash",
            &json!({"command": "ls"}),
            "",
            ToolState::Pending,
            60,
        );
        let title = lines
            .iter()
            .find(|l| text_of(l).contains("bash"))
            .expect("title row");
        assert!(title.spans.iter().any(|s| {
            s.content.contains("bash") && s.style.add_modifier.contains(Modifier::BOLD)
        }));
    }

    #[test]
    fn renders_args_as_pretty_json() {
        let lines = tool_box_lines(
            "bash",
            &json!({"command": "ls"}),
            "",
            ToolState::Pending,
            60,
        );
        let out = joined(&lines);
        assert!(out.contains("\"command\""), "args JSON key: {out:?}");
        assert!(out.contains("\"ls\""), "args JSON value: {out:?}");
    }

    #[test]
    fn empty_args_object_is_skipped() {
        let lines = tool_box_lines("noop", &json!({}), "done", ToolState::Success, 60);
        let out = joined(&lines);
        assert!(!out.contains("{}"), "empty args must be skipped: {out:?}");
        assert!(out.contains("done"), "result still shown");
    }

    #[test]
    fn result_text_renders_below_args() {
        let lines = tool_box_lines(
            "read",
            &json!({"path": "/x"}),
            "file body",
            ToolState::Success,
            60,
        );
        let out = joined(&lines);
        assert!(out.contains("file body"));
        // args precede the result.
        let args_idx = lines.iter().position(|l| text_of(l).contains("path"));
        let result_idx = lines.iter().position(|l| text_of(l).contains("file body"));
        assert!(args_idx < result_idx, "args must precede result");
    }

    #[test]
    fn tint_reaches_both_edges_on_every_row() {
        let width = 40u16;
        let lines = tool_box_lines(
            "read",
            &json!({"path": "/x"}),
            "body",
            ToolState::Success,
            width,
        );
        for line in &lines {
            let row_width: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
            assert!(
                row_width >= usize::from(width),
                "row must fill to width so the tint reaches the edge: {row_width} < {width}"
            );
            assert!(
                line.spans.iter().all(|s| s.style.bg == Some(SUCCESS_BG)),
                "every span carries the tint: {line:?}"
            );
        }
    }

    // --- edit/write diff (VAL-CHAT-039) ----------------------------------

    #[test]
    fn is_diff_tool_matches_edit_and_write() {
        assert!(is_diff_tool("edit"));
        assert!(is_diff_tool("write"));
        assert!(is_diff_tool("Edit"));
        assert!(!is_diff_tool("read"));
        assert!(!is_diff_tool("bash"));
    }

    #[test]
    fn edit_tool_renders_added_line_green() {
        let lines = tool_box_lines(
            "edit",
            &json!({"path": "/x"}),
            "+ 3 new content",
            ToolState::Success,
            60,
        );
        let added = lines
            .iter()
            .find(|l| text_of(l).contains("new content"))
            .expect("added row");
        assert!(
            added.spans.iter().any(|s| s.style.fg == Some(DIFF_ADDED)),
            "added line must be green: {added:?}"
        );
    }

    #[test]
    fn edit_tool_renders_removed_line_red() {
        let lines = tool_box_lines(
            "edit",
            &json!({"path": "/x"}),
            "- 3 old content\n  4 kept",
            ToolState::Success,
            60,
        );
        let removed = lines
            .iter()
            .find(|l| text_of(l).contains("old content"))
            .expect("removed row");
        assert!(
            removed
                .spans
                .iter()
                .any(|s| s.style.fg == Some(DIFF_REMOVED)),
            "removed line must be red: {removed:?}"
        );
    }

    #[test]
    fn edit_tool_context_line_is_dim() {
        let lines = tool_box_lines(
            "edit",
            &json!({}),
            "  1 unchanged line",
            ToolState::Success,
            60,
        );
        let ctx = lines
            .iter()
            .find(|l| text_of(l).contains("unchanged"))
            .expect("context row");
        assert!(ctx.spans.iter().any(|s| s.style.fg == Some(DIFF_CONTEXT)));
    }

    #[test]
    fn single_line_modification_highlights_changed_tokens() {
        let lines = tool_box_lines(
            "edit",
            &json!({}),
            "- 1 hello world\n+ 1 hello rust",
            ToolState::Success,
            60,
        );
        // Both a red and a green row are present.
        assert!(
            lines
                .iter()
                .any(|l| l.spans.iter().any(|s| s.style.fg == Some(DIFF_REMOVED)))
        );
        assert!(
            lines
                .iter()
                .any(|l| l.spans.iter().any(|s| s.style.fg == Some(DIFF_ADDED)))
        );
        // A changed token carries inverse video.
        assert!(
            lines.iter().any(|l| l
                .spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::REVERSED))),
            "changed tokens must be highlighted with inverse video"
        );
    }

    #[test]
    fn write_tool_diff_keeps_box_tint_on_diff_rows() {
        let lines = tool_box_lines(
            "write",
            &json!({"path": "/new"}),
            "+ 1 created by mock",
            ToolState::Success,
            60,
        );
        let added = lines
            .iter()
            .find(|l| text_of(l).contains("created by mock"))
            .expect("added row");
        assert!(
            added.spans.iter().all(|s| s.style.bg == Some(SUCCESS_BG)),
            "diff rows keep the box tint: {added:?}"
        );
    }
}
