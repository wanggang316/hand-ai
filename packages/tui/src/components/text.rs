//! Text component — multi-line text display with word wrapping.

use crate::tui::Component;
use crate::utils;

/// Multi-line text display with ANSI-aware word wrapping.
pub struct TextComponent {
    text: String,
    padding_x: u16,
    padding_y: u16,
    cache: Option<(String, u16, Vec<String>)>,
}

impl TextComponent {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            padding_x: 0,
            padding_y: 0,
            cache: None,
        }
    }

    pub fn with_padding(mut self, x: u16, y: u16) -> Self {
        self.padding_x = x;
        self.padding_y = y;
        self
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        let new_text = text.into();
        if new_text != self.text {
            self.text = new_text;
            self.cache = None;
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

impl Component for TextComponent {
    fn render(&self, width: u16) -> Vec<String> {
        // Check cache
        if let Some((cached_text, cached_width, cached_lines)) = &self.cache
            && cached_text == &self.text
            && *cached_width == width
        {
            return cached_lines.clone();
        }

        let available = width.saturating_sub(self.padding_x * 2) as usize;
        let padding = " ".repeat(self.padding_x as usize);
        let blank_lines: Vec<String> = (0..self.padding_y).map(|_| String::new()).collect();

        let mut lines = blank_lines.clone();

        for source_line in self.text.lines() {
            let wrapped = utils::wrap_text(source_line, available);
            for line in wrapped {
                lines.push(format!("{}{}{}", padding, line, padding));
            }
        }

        // Handle empty text
        if self.text.is_empty() {
            lines.push(format!("{}{}", padding, padding));
        }

        lines.extend(blank_lines);
        lines
    }

    fn invalidate(&mut self) {
        self.cache = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_basic() {
        let comp = TextComponent::new("hello world");
        let lines = comp.render(80);
        assert_eq!(lines, vec!["hello world"]);
    }

    #[test]
    fn test_text_multiline() {
        let comp = TextComponent::new("line1\nline2\nline3");
        let lines = comp.render(80);
        assert_eq!(lines, vec!["line1", "line2", "line3"]);
    }

    #[test]
    fn test_text_wrapping() {
        let comp = TextComponent::new("hello world");
        let lines = comp.render(5);
        assert!(lines.len() >= 2);
    }

    #[test]
    fn test_text_padding() {
        let comp = TextComponent::new("hi").with_padding(2, 1);
        let lines = comp.render(80);
        // Should have padding_y blank lines at top and bottom
        assert!(lines.len() >= 3);
        assert_eq!(lines[0], ""); // top padding
        assert!(lines[1].starts_with("  ")); // left padding
    }

    #[test]
    fn test_text_set_text() {
        let mut comp = TextComponent::new("before");
        assert_eq!(comp.text(), "before");
        comp.set_text("after");
        assert_eq!(comp.text(), "after");
    }

    #[test]
    fn test_text_empty() {
        let comp = TextComponent::new("");
        let lines = comp.render(80);
        assert!(!lines.is_empty());
    }
}
