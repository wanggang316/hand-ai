//! Tinted `[label]` boxes and collapsible summaries on the rt stack.
//!
//! This is the rt-native port of the legacy `components/{custom_message,
//! compaction_summary_message, branch_summary_message, skill_invocation_message}`
//! renderers. Where the legacy components painted ANSI-escaped `Vec<String>`
//! through the old `hand_tui::Component` model, these render owned
//! [`Line<'static>`] blocks with a per-span ratatui [`Style`] — the model the rt
//! scheduler commits into native scrollback (like [`super::tools`] and the user
//! bubble).
//!
//! # Two shapes
//!
//! - **Labelled box** ([`labelled_box_lines`]) — a bold-magenta `[label]` header
//!   above a markdown body inside a muted-purple tinted box. `/skills`,
//!   `/extensions`, `/diagnostics`, and `/changelog` render through it, each with
//!   a sensible empty form when nothing is installed.
//! - **Collapsible summary** ([`summary_lines`]) — a compaction / branch / skill
//!   summary that renders either a short collapsed line carrying the
//!   `(ctrl+r to expand)` hint, or the full markdown body when expanded. The
//!   collapsed hint is *real*: the driver registers a Ctrl+R listener that flips
//!   the most-recent summary's state and re-commits it (native scrollback is
//!   immutable, so the flip appends the re-rendered block — the same discipline
//!   the Ctrl+T thinking toggle uses).

use hand_tui::rt::components::render_markdown;
use hand_tui::rt::components::syntax_highlight::default_markdown_theme;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::modes::interactive::theme::ThemePalette;

/// Dim foreground for the `(ctrl+r to expand)` hint parenthetical. Kept as a
/// fixed dim grey — a secondary hint colour with no dedicated theme slot.
const HINT_FG: Color = Color::Rgb(150, 150, 150);

/// The resolved box colours for a summary / custom-message box, derived from
/// the active palette. The default palette keeps the historical muted-purple
/// look (`#5f005f` box, `#ff78ff` label, `#eeeeee` body); a custom theme
/// retints them from its `customMessageBg` / `customMessageLabel` /
/// `customMessageText` slots.
#[derive(Debug, Clone, Copy)]
struct BoxColors {
    /// Box background tint, applied edge to edge on every row.
    bg: Color,
    /// Bold `[label]` header foreground.
    label: Color,
    /// Body-text foreground, readable on `bg`.
    body: Color,
}

impl BoxColors {
    fn from_palette(palette: &ThemePalette) -> Self {
        Self {
            bg: palette.custom_message_bg,
            label: palette.custom_message_label,
            body: palette.custom_message_text,
        }
    }
}

/// The key hint shown in a collapsed summary. Real: the driver's Ctrl+R
/// listener honours it (legacy had the hint but no listener).
pub const EXPAND_KEY: &str = "ctrl+r";

/// Which collapsible summary a committed block is, driving its `[label]` and
/// collapsed-line wording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SummaryKind {
    /// A `/compact` result: `[compaction]` + "Compacted from N tokens".
    Compaction {
        /// Token count before compaction, rendered with thousands separators in
        /// the collapsed line.
        tokens_before: u64,
    },
    /// A branch-navigation summary: `[branch]`.
    Branch,
    /// A parsed `<skill>` invocation: `[skill] <name>`.
    Skill {
        /// The skill name, shown in the collapsed line and the expanded header.
        name: String,
    },
}

impl SummaryKind {
    /// The bracketed label (without the brackets' surrounding styling).
    fn label(&self) -> &'static str {
        match self {
            SummaryKind::Compaction { .. } => "[compaction]",
            SummaryKind::Branch => "[branch]",
            SummaryKind::Skill { .. } => "[skill]",
        }
    }
}

/// A collapsible summary committed to scrollback, tracked so Ctrl+R can flip its
/// state and re-commit it. The `summary` markdown is the expanded body.
#[derive(Debug, Clone)]
pub struct CollapsibleSummary {
    /// The kind (drives the label + collapsed wording).
    pub kind: SummaryKind,
    /// The markdown body shown when expanded.
    pub summary: String,
    /// Whether the block currently renders expanded. Flipped by Ctrl+R.
    pub expanded: bool,
}

impl CollapsibleSummary {
    /// A fresh, collapsed compaction summary.
    #[must_use]
    pub fn compaction(summary: impl Into<String>, tokens_before: u64) -> Self {
        Self {
            kind: SummaryKind::Compaction { tokens_before },
            summary: summary.into(),
            expanded: false,
        }
    }

    /// Flip the expansion state and report the new value.
    pub fn toggle(&mut self) -> bool {
        self.expanded = !self.expanded;
        self.expanded
    }
}

/// Render a collapsible summary block into its tinted scrollback lines.
///
/// Collapsed: a single body line carrying the `(ctrl+r to expand)` hint, with
/// the summary body hidden. Expanded: the full markdown body under a bold header.
/// Either way the `[label]` header sits at the top and the whole box tints edge
/// to edge in the palette's custom-message background. `palette` colours the
/// box tint, label and body from the active theme (the default palette keeps
/// the historical muted-purple look).
#[must_use]
pub fn summary_lines(
    summary: &CollapsibleSummary,
    width: u16,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let colors = BoxColors::from_palette(palette);
    let mut out = vec![blank_row(width, colors)];
    out.push(label_row(summary.kind.label(), width, colors));
    out.push(blank_row(width, colors));

    if summary.expanded {
        let body = expanded_body(summary);
        for line in render_markdown(
            &body,
            width.max(2).saturating_sub(2),
            &default_markdown_theme(),
        ) {
            out.push(tint_existing(line, width, colors));
        }
    } else {
        out.push(collapsed_line_row(summary, width, colors));
    }

    out.push(blank_row(width, colors));
    out
}

/// The markdown body shown when a summary is expanded — a bold header specific
/// to the kind, then the summary text.
fn expanded_body(summary: &CollapsibleSummary) -> String {
    match &summary.kind {
        SummaryKind::Compaction { tokens_before } => format!(
            "**Compacted from {} tokens**\n\n{}",
            format_thousands(*tokens_before),
            summary.summary
        ),
        SummaryKind::Branch => format!("**Branch summary**\n\n{}", summary.summary),
        SummaryKind::Skill { name } => format!("**{name}**\n\n{}", summary.summary),
    }
}

/// The collapsed one-line body carrying the `(ctrl+r to expand)` hint, worded per
/// kind.
fn collapsed_line_row(
    summary: &CollapsibleSummary,
    width: u16,
    colors: BoxColors,
) -> Line<'static> {
    let body_style = Style::default().bg(colors.bg).fg(colors.body);
    let hint_style = Style::default().bg(colors.bg).fg(HINT_FG);

    let lead = match &summary.kind {
        SummaryKind::Compaction { tokens_before } => {
            format!(
                "Compacted from {} tokens ",
                format_thousands(*tokens_before)
            )
        }
        SummaryKind::Branch => "Branch summary ".to_string(),
        SummaryKind::Skill { name } => format!("{name} "),
    };
    padded_row(
        vec![
            Span::styled(lead, body_style),
            Span::styled(format!("({EXPAND_KEY} to expand)"), hint_style),
        ],
        width,
        colors,
    )
}

/// Render a `[label]`-headed tinted box carrying a markdown body — the
/// `/skills`, `/extensions`, `/diagnostics`, `/changelog` surface.
///
/// The body is rendered as markdown so a `- **name** — desc` list reads as a
/// list. An empty body still produces a well-formed box (the callers pass a
/// sensible `_(no …)_` empty form).
#[must_use]
pub fn labelled_box_lines(
    label: &str,
    body: &str,
    width: u16,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let colors = BoxColors::from_palette(palette);
    let bracketed = format!("[{label}]");
    let mut out = vec![blank_row(width, colors)];
    out.push(label_row(&bracketed, width, colors));
    if !body.trim().is_empty() {
        out.push(blank_row(width, colors));
        for line in render_markdown(
            body,
            width.max(2).saturating_sub(2),
            &default_markdown_theme(),
        ) {
            out.push(tint_existing(line, width, colors));
        }
    }
    out.push(blank_row(width, colors));
    out
}

/// The bold `[label]` header row, coloured from `colors`.
fn label_row(bracketed: &str, width: u16, colors: BoxColors) -> Line<'static> {
    let label_style = Style::default()
        .bg(colors.bg)
        .fg(colors.label)
        .add_modifier(Modifier::BOLD);
    padded_row(
        vec![Span::styled(bracketed.to_string(), label_style)],
        width,
        colors,
    )
}

/// A blank, fully-tinted row spanning the width.
fn blank_row(width: u16, colors: BoxColors) -> Line<'static> {
    Line::from(Span::styled(
        " ".repeat(usize::from(width.max(1))),
        Style::default().bg(colors.bg),
    ))
}

/// Wrap `spans` in a one-column-padded, right-filled row so the tint reaches
/// both edges.
fn padded_row(spans: Vec<Span<'static>>, width: u16, colors: BoxColors) -> Line<'static> {
    let visible: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let pad_bg = Style::default().bg(colors.bg);
    let inner_cols = usize::from(width.max(2)).saturating_sub(2);
    let right_fill = inner_cols.saturating_sub(visible);

    let mut out = vec![Span::styled(" ".to_string(), pad_bg)];
    out.extend(spans);
    out.push(Span::styled(" ".repeat(right_fill + 1), pad_bg));
    Line::from(out)
}

/// Tint an already-styled markdown row edge to edge: patch the box background
/// over every span (keeping any markdown fg), then pad it to the width so the
/// tint reaches both edges and the body sits one column in.
fn tint_existing(line: Line<'static>, width: u16, colors: BoxColors) -> Line<'static> {
    let bg = Style::default().bg(colors.bg);
    let spans: Vec<Span<'static>> = line
        .spans
        .into_iter()
        .map(|span| {
            let style = span.style.patch(bg);
            // Default the fg to the readable body color when the markdown span
            // carried none, so text is legible on the tint.
            let style = if style.fg.is_none() {
                style.fg(colors.body)
            } else {
                style
            };
            Span::styled(span.content.into_owned(), style)
        })
        .collect();
    padded_row(spans, width, colors)
}

/// Render a `u64` with comma thousands separators (`12345` → `"12,345"`).
fn format_thousands(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default palette — the historical muted-purple look — so the existing
    /// assertions keep pinning the same colours.
    fn pal() -> ThemePalette {
        ThemePalette::default()
    }

    /// The historical box background tint, re-declared for the tint assertions.
    const BOX_BG: Color = Color::Rgb(95, 0, 95);

    /// The plain concatenated text of a line.
    fn text_of(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// The joined plain text of every line.
    fn joined(lines: &[Line<'_>]) -> String {
        lines.iter().map(text_of).collect::<Vec<_>>().join("\n")
    }

    /// Every span across every line carries the box background tint.
    fn all_tinted(lines: &[Line<'_>]) -> bool {
        lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .all(|s| s.style.bg == Some(BOX_BG))
    }

    // --- labelled box (VAL-CHAT-030 / VAL-CHAT-044) ----------------------

    #[test]
    fn labelled_box_shows_label_and_body_tinted() {
        let lines = labelled_box_lines("changelog", "## 1.0\n\n- did a thing", 60, &pal());
        let out = joined(&lines);
        assert!(out.contains("[changelog]"), "label missing: {out:?}");
        assert!(out.contains("did a thing"), "body missing: {out:?}");
        assert!(all_tinted(&lines), "box must tint edge to edge: {lines:?}");
    }

    #[test]
    fn labelled_box_empty_body_still_well_formed() {
        // The empty form (no skills / extensions installed) still renders a
        // labelled box, just without a body block.
        let lines = labelled_box_lines("skills", "_(no skills discovered)_", 60, &pal());
        let out = joined(&lines);
        assert!(out.contains("[skills]"), "label present: {out:?}");
        assert!(
            out.contains("no skills discovered"),
            "empty notice present: {out:?}"
        );
        assert!(all_tinted(&lines));
    }

    #[test]
    fn custom_palette_retints_the_box() {
        // A custom palette recolours the box tint and the label, so a custom
        // theme colours the summary box (VAL-COMPAT-004); the default palette
        // keeps the historical muted-purple look.
        let neon = ThemePalette {
            custom_message_bg: Color::Rgb(0x1a, 0x00, 0x33),
            custom_message_label: Color::Rgb(0xff, 0x00, 0xff),
            ..ThemePalette::default()
        };
        let lines = labelled_box_lines("skills", "body", 60, &neon);
        assert!(
            lines
                .iter()
                .flat_map(|l| l.spans.iter())
                .all(|s| s.style.bg == Some(Color::Rgb(0x1a, 0x00, 0x33))),
            "custom box tint applied edge to edge"
        );
        let label = lines
            .iter()
            .find(|l| text_of(l).contains("[skills]"))
            .expect("label row");
        assert!(
            label
                .spans
                .iter()
                .any(|s| s.style.fg == Some(Color::Rgb(0xff, 0x00, 0xff))),
            "custom label colour applied"
        );
        // The default palette keeps the historical tint.
        assert!(all_tinted(&labelled_box_lines(
            "skills",
            "body",
            60,
            &pal()
        )));
    }

    #[test]
    fn labelled_box_bold_label() {
        let lines = labelled_box_lines("diagnostics", "ok", 40, &pal());
        let label = lines
            .iter()
            .find(|l| text_of(l).contains("[diagnostics]"))
            .expect("label row");
        assert!(
            label
                .spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::BOLD)),
            "the label must be bold: {label:?}"
        );
    }

    // --- collapsible summary collapsed form (VAL-CHAT-019) ----------------

    #[test]
    fn compaction_collapsed_shows_hint_and_hides_body() {
        let summary = CollapsibleSummary::compaction("secret summary body", 12_345);
        let lines = summary_lines(&summary, 60, &pal());
        let out = joined(&lines);
        assert!(out.contains("[compaction]"), "label missing: {out:?}");
        assert!(
            out.contains("Compacted from 12,345 tokens"),
            "collapsed count missing: {out:?}"
        );
        assert!(
            out.contains("(ctrl+r to expand)"),
            "expand hint missing: {out:?}"
        );
        assert!(
            !out.contains("secret summary body"),
            "collapsed must not leak the body: {out:?}"
        );
        assert!(all_tinted(&lines));
    }

    #[test]
    fn compaction_expanded_shows_body_and_header() {
        let mut summary = CollapsibleSummary::compaction("the full summary text", 1_000);
        summary.expanded = true;
        let out = joined(&summary_lines(&summary, 60, &pal()));
        assert!(out.contains("[compaction]"), "label: {out:?}");
        assert!(
            out.contains("Compacted from 1,000 tokens"),
            "expanded header: {out:?}"
        );
        assert!(
            out.contains("the full summary text"),
            "expanded body: {out:?}"
        );
        assert!(
            !out.contains("(ctrl+r to expand)"),
            "no expand hint once expanded: {out:?}"
        );
    }

    #[test]
    fn toggle_flips_and_reports_new_state() {
        let mut summary = CollapsibleSummary::compaction("x", 1);
        assert!(!summary.expanded, "starts collapsed");
        assert!(summary.toggle(), "first flip expands");
        assert!(summary.expanded);
        assert!(!summary.toggle(), "second flip collapses");
        assert!(!summary.expanded);
    }

    #[test]
    fn skill_summary_carries_the_name_in_the_collapsed_line() {
        let summary = CollapsibleSummary {
            kind: SummaryKind::Skill {
                name: "code-review".to_string(),
            },
            summary: "hidden body".to_string(),
            expanded: false,
        };
        let out = joined(&summary_lines(&summary, 60, &pal()));
        assert!(out.contains("[skill]"), "label: {out:?}");
        assert!(out.contains("code-review"), "name: {out:?}");
        assert!(out.contains("(ctrl+r to expand)"), "hint: {out:?}");
        assert!(!out.contains("hidden body"), "body hidden: {out:?}");
    }

    #[test]
    fn branch_summary_expanded_shows_body() {
        let summary = CollapsibleSummary {
            kind: SummaryKind::Branch,
            summary: "diverged here".to_string(),
            expanded: true,
        };
        let out = joined(&summary_lines(&summary, 60, &pal()));
        assert!(out.contains("[branch]"), "label: {out:?}");
        assert!(out.contains("diverged here"), "body: {out:?}");
    }

    #[test]
    fn formats_tokens_with_separators() {
        assert_eq!(format_thousands(0), "0");
        assert_eq!(format_thousands(999), "999");
        assert_eq!(format_thousands(1_000), "1,000");
        assert_eq!(format_thousands(1_234_567), "1,234,567");
    }
}
