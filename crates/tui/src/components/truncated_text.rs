//! Truncated text component — single-line text with truncation.
//
// audit: M3.T5 — parity reviewed against pi-tui/truncated-text.ts on 2026-05-07.

use crate::tui::Component;
use crate::utils;

/// Single-line text that truncates with "…" when too wide.
pub struct TruncatedTextComponent {
    text: String,
    padding_x: u16,
    padding_y: u16,
}

impl TruncatedTextComponent {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            padding_x: 0,
            padding_y: 0,
        }
    }

    /// Set horizontal/vertical padding (mirrors the TS `paddingX`/`paddingY` ctor args).
    pub fn with_padding(mut self, padding_x: u16, padding_y: u16) -> Self {
        self.padding_x = padding_x;
        self.padding_y = padding_y;
        self
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

impl Component for TruncatedTextComponent {
    fn render(&self, width: u16) -> Vec<String> {
        let width = width as usize;
        let pad_x = self.padding_x as usize;
        let blank = " ".repeat(width);
        let mut lines: Vec<String> = (0..self.padding_y).map(|_| blank.clone()).collect();

        // Take only the first source line — TS stops at the first `\n`.
        let first = self.text.split('\n').next().unwrap_or("");
        let avail = width.saturating_sub(pad_x * 2).max(1);
        let truncated = utils::truncate_to_width(first, avail);
        let pad = " ".repeat(pad_x);
        let core = format!("{pad}{truncated}{pad}");
        let visible = utils::visible_width(&core);
        let extra = width.saturating_sub(visible);
        lines.push(format!("{core}{}", " ".repeat(extra)));

        for _ in 0..self.padding_y {
            lines.push(blank.clone());
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncated_text_short() {
        let comp = TruncatedTextComponent::new("hi");
        let lines = comp.render(80);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("hi"));
        assert_eq!(utils::visible_width(&lines[0]), 80);
    }

    #[test]
    fn test_truncated_text_long() {
        let comp = TruncatedTextComponent::new("a very long text that should be truncated");
        let lines = comp.render(10);
        let line = &lines[0];
        assert!(utils::visible_width(&utils::strip_ansi(line)) <= 10);
    }

    #[test]
    fn test_truncated_text_set() {
        let mut comp = TruncatedTextComponent::new("before");
        comp.set_text("after");
        assert_eq!(comp.text(), "after");
    }

    #[test]
    fn test_truncated_text_padding() {
        let comp = TruncatedTextComponent::new("hi").with_padding(2, 1);
        let lines = comp.render(20);
        assert_eq!(lines.len(), 3);
        // Top padding row is fully blank.
        assert!(lines[0].chars().all(|c| c == ' '));
        // Middle line has left padding then text.
        assert!(lines[1].starts_with("  hi"));
        // Bottom padding row.
        assert!(lines[2].chars().all(|c| c == ' '));
    }

    #[test]
    fn test_truncated_text_first_line_only() {
        let comp = TruncatedTextComponent::new("first\nsecond");
        let lines = comp.render(20);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("first"));
        assert!(!lines[0].contains("second"));
    }
}
