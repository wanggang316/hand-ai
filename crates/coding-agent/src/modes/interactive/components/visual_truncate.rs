//! Tail-truncation helper aware of terminal-line wrapping.
//!
//! Ported from
//! `pi-mono/packages/coding-agent/src/modes/interactive/components/visual-truncate.ts`.
//!
//! Truncates text to at most `max_visual_lines` rendered lines (visual, not
//! logical), keeping the *tail*. Wrapping is computed at the supplied width so
//! the result is what the user would actually see in a terminal of that
//! width. A `padding_x` parameter lets callers reserve horizontal padding
//! before wrapping — pass `0` when the result will be placed in a `Box`
//! component (which adds its own padding) and `1` for plain containers.
//!
//! pi-mono implements this by instantiating a temporary `Text` component and
//! slicing its rendered output. We delegate to [`hand_tui::TextComponent`]
//! for the same reason: it guarantees identical wrapping/padding semantics
//! to whatever rendering the caller would otherwise have done.

use hand_tui::{Component, TextComponent};

/// Result of [`truncate_to_visual_lines`]: the kept tail and the count of
/// dropped leading visual lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualTruncateResult {
    /// Tail of visual lines actually kept.
    pub visual_lines: Vec<String>,
    /// Number of leading visual lines that were dropped.
    pub skipped_count: usize,
}

/// Truncate `text` to at most `max_visual_lines` lines after wrapping at
/// `width`, keeping the tail.
///
/// Returns an empty result when `text` is empty (matching the TS guard) and
/// passes the input through unchanged when it already fits within the budget.
pub fn truncate_to_visual_lines(
    text: &str,
    max_visual_lines: usize,
    width: u16,
    padding_x: u16,
) -> VisualTruncateResult {
    if text.is_empty() {
        return VisualTruncateResult {
            visual_lines: Vec::new(),
            skipped_count: 0,
        };
    }

    let temp = TextComponent::new(text).with_padding(padding_x, 0);
    let all_lines = temp.render(width);

    if all_lines.len() <= max_visual_lines {
        return VisualTruncateResult {
            visual_lines: all_lines,
            skipped_count: 0,
        };
    }

    let skipped_count = all_lines.len() - max_visual_lines;
    let visual_lines = all_lines.into_iter().skip(skipped_count).collect();

    VisualTruncateResult {
        visual_lines,
        skipped_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_returns_empty_result() {
        let r = truncate_to_visual_lines("", 5, 20, 0);
        assert!(r.visual_lines.is_empty());
        assert_eq!(r.skipped_count, 0);
    }

    #[test]
    fn fits_within_budget_returns_input_unchanged() {
        let r = truncate_to_visual_lines("a\nb\nc", 5, 20, 0);
        assert_eq!(r.visual_lines, vec!["a", "b", "c"]);
        assert_eq!(r.skipped_count, 0);
    }

    #[test]
    fn over_budget_keeps_tail_and_reports_skipped() {
        let text = "1\n2\n3\n4\n5";
        let r = truncate_to_visual_lines(text, 3, 20, 0);
        assert_eq!(r.visual_lines, vec!["3", "4", "5"]);
        assert_eq!(r.skipped_count, 2);
    }

    #[test]
    fn wrapping_counts_against_visual_budget() {
        // "abcdefghij" wraps to two lines at width 5, so two source lines
        // produce four visual lines. Keeping only the last 2 visual lines
        // should drop the first source line entirely.
        let text = "abcdefghij\nklmnopqrst";
        let r = truncate_to_visual_lines(text, 2, 5, 0);
        assert_eq!(r.visual_lines.len(), 2);
        assert_eq!(r.skipped_count, 2);
        // Surviving lines came from the second source row.
        let joined = r.visual_lines.join("");
        assert!(joined.contains("klmno") || joined.contains("pqrst"));
    }

    #[test]
    fn padding_x_reduces_available_width() {
        // Padding of 1 each side at width 5 leaves 3 cells, so a 6-char string
        // wraps into at least 2 lines; the budget of 1 keeps just the tail.
        let r = truncate_to_visual_lines("abcdef", 1, 5, 1);
        assert_eq!(r.visual_lines.len(), 1);
        assert!(r.skipped_count >= 1);
    }
}
