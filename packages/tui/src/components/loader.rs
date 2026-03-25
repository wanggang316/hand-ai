//! Loader component — animated loading spinner.

use crate::tui::Component;

/// Braille spinner frames.
const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Animated loading spinner with message.
pub struct LoaderComponent {
    message: String,
    frame: usize,
    spinner_color: String,
    message_color: String,
}

impl LoaderComponent {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            frame: 0,
            spinner_color: "\x1b[36m".to_string(), // cyan
            message_color: "\x1b[90m".to_string(), // dim
        }
    }

    pub fn with_spinner_color(mut self, ansi_code: impl Into<String>) -> Self {
        self.spinner_color = ansi_code.into();
        self
    }

    pub fn with_message_color(mut self, ansi_code: impl Into<String>) -> Self {
        self.message_color = ansi_code.into();
        self
    }

    /// Advance to the next spinner frame. Call this on a timer.
    pub fn tick(&mut self) {
        self.frame = (self.frame + 1) % SPINNER_FRAMES.len();
    }

    pub fn set_message(&mut self, message: impl Into<String>) {
        self.message = message.into();
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    /// Get the current frame index.
    pub fn frame_index(&self) -> usize {
        self.frame
    }
}

impl Component for LoaderComponent {
    fn render(&self, _width: u16) -> Vec<String> {
        let spinner = SPINNER_FRAMES[self.frame];
        vec![format!(
            "{}{}\x1b[0m {}{}\x1b[0m",
            self.spinner_color, spinner, self.message_color, self.message
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_loader_render() {
        let loader = LoaderComponent::new("Loading...");
        let lines = loader.render(80);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("Loading..."));
        assert!(lines[0].contains("⠋")); // First frame
    }

    #[test]
    fn test_loader_tick() {
        let mut loader = LoaderComponent::new("test");
        assert_eq!(loader.frame_index(), 0);
        loader.tick();
        assert_eq!(loader.frame_index(), 1);
        let lines = loader.render(80);
        assert!(lines[0].contains("⠙")); // Second frame
    }

    #[test]
    fn test_loader_tick_wraps() {
        let mut loader = LoaderComponent::new("test");
        for _ in 0..SPINNER_FRAMES.len() {
            loader.tick();
        }
        assert_eq!(loader.frame_index(), 0); // Wrapped around
    }

    #[test]
    fn test_loader_set_message() {
        let mut loader = LoaderComponent::new("before");
        loader.set_message("after");
        assert_eq!(loader.message(), "after");
        let lines = loader.render(80);
        assert!(lines[0].contains("after"));
    }

    #[test]
    fn test_loader_custom_colors() {
        let loader = LoaderComponent::new("test")
            .with_spinner_color("\x1b[31m")
            .with_message_color("\x1b[32m");
        let lines = loader.render(80);
        assert!(lines[0].contains("\x1b[31m"));
        assert!(lines[0].contains("\x1b[32m"));
    }
}
