//! Status bar component — renders a fixed-width status line.

use crate::tui::Component;

/// A status bar with left, center, and right sections.
pub struct StatusBarComponent {
    left: String,
    center: String,
    right: String,
    style_prefix: String,
}

impl StatusBarComponent {
    pub fn new() -> Self {
        Self {
            left: String::new(),
            center: String::new(),
            right: String::new(),
            style_prefix: String::new(),
        }
    }

    /// Set the left section text.
    pub fn set_left(&mut self, text: impl Into<String>) {
        self.left = text.into();
    }

    /// Set the center section text.
    pub fn set_center(&mut self, text: impl Into<String>) {
        self.center = text.into();
    }

    /// Set the right section text.
    pub fn set_right(&mut self, text: impl Into<String>) {
        self.right = text.into();
    }

    /// Set ANSI style prefix applied to the entire bar.
    pub fn set_style(&mut self, style: impl Into<String>) {
        self.style_prefix = style.into();
    }
}

impl Default for StatusBarComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl Component for StatusBarComponent {
    fn render(&self, width: u16) -> Vec<String> {
        let w = width as usize;
        let left_len = crate::utils::visible_width(&self.left);
        let right_len = crate::utils::visible_width(&self.right);
        let center_len = crate::utils::visible_width(&self.center);

        // Calculate padding
        let used = left_len + right_len + center_len;
        let remaining = w.saturating_sub(used);

        let left_pad = remaining / 2;
        let right_pad = remaining.saturating_sub(left_pad);

        let line = format!(
            "{}{}{}{}{}{}{}",
            self.style_prefix,
            self.left,
            " ".repeat(left_pad),
            self.center,
            " ".repeat(right_pad),
            self.right,
            if self.style_prefix.is_empty() {
                ""
            } else {
                "\x1b[0m"
            },
        );

        vec![line]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_bar_render() {
        let mut bar = StatusBarComponent::new();
        bar.set_left("Model: gpt-4o");
        bar.set_right("Session: s_123");

        let lines = bar.render(80);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("Model: gpt-4o"));
        assert!(lines[0].contains("Session: s_123"));
    }

    #[test]
    fn test_status_bar_with_style() {
        let mut bar = StatusBarComponent::new();
        bar.set_style("\x1b[44m");
        bar.set_left("test");

        let lines = bar.render(40);
        assert!(lines[0].starts_with("\x1b[44m"));
        assert!(lines[0].ends_with("\x1b[0m"));
    }

    #[test]
    fn test_status_bar_default() {
        let bar = StatusBarComponent::default();
        let lines = bar.render(80);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn test_status_bar_center() {
        let mut bar = StatusBarComponent::new();
        bar.set_center("CENTERED");

        let lines = bar.render(80);
        assert!(lines[0].contains("CENTERED"));
    }
}
