//! Toast/notification component — transient messages.

use crate::tui::{Component, HandleResult};

/// Toast severity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastLevel {
    Info,
    Success,
    Warning,
    Error,
}

impl ToastLevel {
    fn icon(self) -> &'static str {
        match self {
            ToastLevel::Info => "i",
            ToastLevel::Success => "*",
            ToastLevel::Warning => "!",
            ToastLevel::Error => "x",
        }
    }

    fn style(self) -> &'static str {
        match self {
            ToastLevel::Info => "\x1b[36m",     // cyan
            ToastLevel::Success => "\x1b[32m",  // green
            ToastLevel::Warning => "\x1b[33m",  // yellow
            ToastLevel::Error => "\x1b[31m",    // red
        }
    }
}

/// A toast/notification message.
pub struct ToastComponent {
    messages: Vec<(ToastLevel, String)>,
    max_visible: usize,
}

impl ToastComponent {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            max_visible: 3,
        }
    }

    /// Add a toast message.
    pub fn push(&mut self, level: ToastLevel, message: impl Into<String>) {
        self.messages.push((level, message.into()));
    }

    /// Add an info toast.
    pub fn info(&mut self, message: impl Into<String>) {
        self.push(ToastLevel::Info, message);
    }

    /// Add a success toast.
    pub fn success(&mut self, message: impl Into<String>) {
        self.push(ToastLevel::Success, message);
    }

    /// Add a warning toast.
    pub fn warning(&mut self, message: impl Into<String>) {
        self.push(ToastLevel::Warning, message);
    }

    /// Add an error toast.
    pub fn error(&mut self, message: impl Into<String>) {
        self.push(ToastLevel::Error, message);
    }

    /// Remove the oldest toast.
    pub fn dismiss_oldest(&mut self) {
        if !self.messages.is_empty() {
            self.messages.remove(0);
        }
    }

    /// Remove all toasts.
    pub fn clear(&mut self) {
        self.messages.clear();
    }

    /// Number of active toasts.
    pub fn count(&self) -> usize {
        self.messages.len()
    }

    /// Set maximum visible toasts.
    pub fn set_max_visible(&mut self, max: usize) {
        self.max_visible = max;
    }
}

impl Default for ToastComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl Component for ToastComponent {
    fn render(&self, width: u16) -> Vec<String> {
        let w = width as usize;
        let visible = self.messages.iter().rev().take(self.max_visible);

        visible
            .map(|(level, msg)| {
                let icon = level.icon();
                let style = level.style();
                let max_msg_len = w.saturating_sub(6); // "[x] " + padding
                let truncated = if msg.len() > max_msg_len {
                    format!("{}...", &msg[..max_msg_len.saturating_sub(3)])
                } else {
                    msg.clone()
                };
                format!("{}[{}]\x1b[0m {}", style, icon, truncated)
            })
            .collect()
    }

    fn handle_input(&mut self, _data: &str) -> HandleResult {
        HandleResult::Ignored
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_toast_empty() {
        let toast = ToastComponent::new();
        let lines = toast.render(80);
        assert!(lines.is_empty());
    }

    #[test]
    fn test_toast_push_and_render() {
        let mut toast = ToastComponent::new();
        toast.info("Hello");
        toast.error("Bad thing");

        let lines = toast.render(80);
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn test_toast_dismiss() {
        let mut toast = ToastComponent::new();
        toast.info("First");
        toast.info("Second");
        assert_eq!(toast.count(), 2);

        toast.dismiss_oldest();
        assert_eq!(toast.count(), 1);
    }

    #[test]
    fn test_toast_clear() {
        let mut toast = ToastComponent::new();
        toast.info("A");
        toast.warning("B");
        toast.error("C");
        toast.clear();
        assert_eq!(toast.count(), 0);
    }

    #[test]
    fn test_toast_max_visible() {
        let mut toast = ToastComponent::new();
        toast.set_max_visible(2);
        toast.info("1");
        toast.info("2");
        toast.info("3");

        let lines = toast.render(80);
        assert_eq!(lines.len(), 2); // Only 2 visible
    }

    #[test]
    fn test_toast_levels() {
        assert_eq!(ToastLevel::Info.icon(), "i");
        assert_eq!(ToastLevel::Success.icon(), "*");
        assert_eq!(ToastLevel::Warning.icon(), "!");
        assert_eq!(ToastLevel::Error.icon(), "x");
    }
}
