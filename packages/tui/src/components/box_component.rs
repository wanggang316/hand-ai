//! Box component — container with padding and optional background.

use crate::tui::{Component, Container, HandleResult};
use crate::utils;

/// Container that applies padding and optional background to children.
pub struct BoxComponent {
    container: Container,
    padding_x: u16,
    padding_y: u16,
    bg_code: Option<String>,
}

impl BoxComponent {
    pub fn new() -> Self {
        Self {
            container: Container::new(),
            padding_x: 0,
            padding_y: 0,
            bg_code: None,
        }
    }

    pub fn with_padding(mut self, x: u16, y: u16) -> Self {
        self.padding_x = x;
        self.padding_y = y;
        self
    }

    pub fn with_background(mut self, ansi_code: impl Into<String>) -> Self {
        self.bg_code = Some(ansi_code.into());
        self
    }

    pub fn add_child(&mut self, child: Box<dyn Component>) {
        self.container.add_child(child);
    }

    pub fn child_count(&self) -> usize {
        self.container.child_count()
    }
}

impl Default for BoxComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl Component for BoxComponent {
    fn render(&self, width: u16) -> Vec<String> {
        let inner_width = width.saturating_sub(self.padding_x * 2);
        let child_lines = self.container.render(inner_width);

        let padding = " ".repeat(self.padding_x as usize);
        let blank: Vec<String> = (0..self.padding_y)
            .map(|_| {
                let line = " ".repeat(width as usize);
                if let Some(bg) = &self.bg_code {
                    format!("{}{}\x1b[0m", bg, line)
                } else {
                    line
                }
            })
            .collect();

        let mut lines = blank.clone();
        for child_line in &child_lines {
            let line = format!("{}{}", padding, child_line);
            if let Some(bg) = &self.bg_code {
                lines.push(utils::apply_background(&line, width as usize, bg, "\x1b[0m"));
            } else {
                lines.push(line);
            }
        }
        lines.extend(blank);

        lines
    }

    fn handle_input(&mut self, data: &str) -> HandleResult {
        self.container.handle_input(data)
    }

    fn invalidate(&mut self) {
        self.container.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::TextComponent;

    #[test]
    fn test_box_empty() {
        let bx = BoxComponent::new();
        let lines = bx.render(80);
        assert!(lines.is_empty());
    }

    #[test]
    fn test_box_with_child() {
        let mut bx = BoxComponent::new();
        bx.add_child(Box::new(TextComponent::new("hello")));
        let lines = bx.render(80);
        assert!(!lines.is_empty());
        assert!(lines[0].contains("hello"));
    }

    #[test]
    fn test_box_with_padding() {
        let mut bx = BoxComponent::new().with_padding(2, 1);
        bx.add_child(Box::new(TextComponent::new("hi")));
        let lines = bx.render(80);
        // Should have top padding, content with left padding, bottom padding
        assert!(lines.len() >= 3);
    }

    #[test]
    fn test_box_with_background() {
        let mut bx = BoxComponent::new().with_background("\x1b[44m");
        bx.add_child(Box::new(TextComponent::new("hi")));
        let lines = bx.render(80);
        assert!(lines[0].contains("\x1b[44m"));
    }

    #[test]
    fn test_box_child_count() {
        let mut bx = BoxComponent::new();
        assert_eq!(bx.child_count(), 0);
        bx.add_child(Box::new(TextComponent::new("a")));
        assert_eq!(bx.child_count(), 1);
    }
}
