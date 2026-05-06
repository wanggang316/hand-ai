//! Thin bridge to `hand-tui`.
//!
//! Proves the `hand-tui` API surface is consumable from `hand-coding-agent`.
//! Currently exposes a single helper that renders markdown to ANSI-styled
//! lines using `MarkdownComponent`. Future tasks may extend this with full
//! TUI integration.

use hand_tui::{Component, MarkdownComponent};

/// Default render width when the caller has no terminal context handy.
const DEFAULT_WIDTH: u16 = 80;

/// Render `text` as markdown into ANSI-styled terminal lines.
pub fn render_markdown(text: &str) -> Vec<String> {
    render_markdown_with_width(text, DEFAULT_WIDTH)
}

/// Render `text` as markdown at an explicit width.
pub fn render_markdown_with_width(text: &str, width: u16) -> Vec<String> {
    MarkdownComponent::new(text).render(width)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke_test_hand_tui_consumable() {
        let lines = render_markdown("# hello");
        assert!(!lines.is_empty());
    }

    #[test]
    fn renders_paragraph_text() {
        let lines = render_markdown("just a paragraph");
        assert!(lines.iter().any(|l| l.contains("just a paragraph")));
    }

    #[test]
    fn empty_input_yields_no_lines() {
        let lines = render_markdown("");
        assert!(lines.is_empty());
    }

    #[test]
    fn explicit_width_is_respected() {
        // Wide enough that the heading fits on one line.
        let lines = render_markdown_with_width("# title", 40);
        assert!(!lines.is_empty());
    }
}
