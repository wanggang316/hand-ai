//! Text component — multi-line text display with word wrapping.
//
// audit: M3.T5 — parity reviewed against pi-tui/text.ts on 2026-05-07.

use crate::tui::Component;
use crate::utils;

/// Multi-line text display with ANSI-aware word wrapping.
pub struct TextComponent {
    text: String,
    padding_x: u16,
    padding_y: u16,
    /// Optional ANSI prefix (e.g. `"\x1b[44m"`) wrapped around every output
    /// line, mirroring TS's `customBgFn`. We use a code rather than a closure
    /// so the component remains `Send + 'static` without lifetime gymnastics.
    bg_code: Option<String>,
    cache: Option<(String, u16, Vec<String>)>,
}

impl TextComponent {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            padding_x: 0,
            padding_y: 0,
            bg_code: None,
            cache: None,
        }
    }

    pub fn with_padding(mut self, x: u16, y: u16) -> Self {
        self.padding_x = x;
        self.padding_y = y;
        self
    }

    /// Set a background ANSI prefix applied to every rendered line. The line
    /// is also padded to the full width and reset with `\x1b[0m`.
    pub fn with_bg_code(mut self, ansi_code: impl Into<String>) -> Self {
        self.bg_code = Some(ansi_code.into());
        self
    }

    /// Replace the background ANSI prefix at runtime. Pass `None` to clear.
    pub fn set_bg_code(&mut self, ansi_code: Option<String>) {
        self.bg_code = ansi_code;
        self.cache = None;
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

        let blank_line = || -> String {
            if let Some(bg) = &self.bg_code {
                utils::apply_background("", width as usize, bg, "\x1b[0m")
            } else {
                String::new()
            }
        };
        let blank_lines: Vec<String> = (0..self.padding_y).map(|_| blank_line()).collect();

        let mut lines = blank_lines.clone();

        for source_line in self.text.lines() {
            let wrapped = utils::wrap_text(source_line, available);
            for line in wrapped {
                let raw = format!("{}{}{}", padding, line, padding);
                if let Some(bg) = &self.bg_code {
                    lines.push(utils::apply_background(&raw, width as usize, bg, "\x1b[0m"));
                } else {
                    lines.push(raw);
                }
            }
        }

        // Handle empty text
        if self.text.is_empty() {
            let raw = format!("{}{}", padding, padding);
            if let Some(bg) = &self.bg_code {
                lines.push(utils::apply_background(&raw, width as usize, bg, "\x1b[0m"));
            } else {
                lines.push(raw);
            }
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

    #[test]
    fn test_text_bg_code() {
        let comp = TextComponent::new("hi").with_bg_code("\x1b[44m");
        let lines = comp.render(20);
        assert!(lines[0].contains("\x1b[44m"));
        assert!(lines[0].contains("\x1b[0m"));
    }

    #[test]
    fn test_text_set_bg_code_runtime() {
        let mut comp = TextComponent::new("hi");
        let lines = comp.render(20);
        assert!(!lines[0].contains("\x1b[44m"));
        comp.set_bg_code(Some("\x1b[44m".into()));
        let lines = comp.render(20);
        assert!(lines[0].contains("\x1b[44m"));
        comp.set_bg_code(None);
        let lines = comp.render(20);
        assert!(!lines[0].contains("\x1b[44m"));
    }
}
