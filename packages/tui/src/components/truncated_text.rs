//! Truncated text component — single-line text with truncation.

use crate::tui::Component;
use crate::utils;

/// Single-line text that truncates with "…" when too wide.
pub struct TruncatedTextComponent {
    text: String,
}

impl TruncatedTextComponent {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
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
        vec![utils::truncate_to_width(&self.text, width as usize)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncated_text_short() {
        let comp = TruncatedTextComponent::new("hi");
        let lines = comp.render(80);
        assert_eq!(lines, vec!["hi"]);
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
}
