//! Loader component — animated loading spinner.
//
// audit: M3.T5 — parity reviewed against pi-tui/loader.ts on 2026-05-07.
// non-goal: TS owns its animation timer (`setInterval`) inside the component;
// the Rust port keeps timing as an external concern (callers run `tick()` on
// their own cadence) so we don't need an interval/start/stop API.

use crate::tui::Component;

/// Default braille spinner frames.
pub const DEFAULT_SPINNER_FRAMES: &[&str] =
    &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Default frame interval in milliseconds (mirrors the TS default).
pub const DEFAULT_INDICATOR_INTERVAL_MS: u64 = 80;

/// Indicator options for the loader, mirroring the TS `LoaderIndicatorOptions`.
///
/// Pass an empty `frames` vector to hide the spinner indicator entirely.
#[derive(Debug, Clone)]
pub struct LoaderIndicatorOptions {
    /// Animation frames; empty hides the indicator.
    pub frames: Vec<String>,
    /// Frame interval (informational — the Rust port doesn't drive its own timer).
    pub interval_ms: u64,
    /// When true, frames are emitted verbatim without applying the spinner color.
    pub render_verbatim: bool,
}

impl Default for LoaderIndicatorOptions {
    fn default() -> Self {
        Self {
            frames: DEFAULT_SPINNER_FRAMES.iter().map(|s| s.to_string()).collect(),
            interval_ms: DEFAULT_INDICATOR_INTERVAL_MS,
            render_verbatim: false,
        }
    }
}

/// Animated loading spinner with message.
pub struct LoaderComponent {
    message: String,
    frame: usize,
    spinner_color: String,
    message_color: String,
    frames: Vec<String>,
    interval_ms: u64,
    render_verbatim: bool,
}

impl LoaderComponent {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            frame: 0,
            spinner_color: "\x1b[36m".to_string(), // cyan
            message_color: "\x1b[90m".to_string(), // dim
            frames: DEFAULT_SPINNER_FRAMES.iter().map(|s| s.to_string()).collect(),
            interval_ms: DEFAULT_INDICATOR_INTERVAL_MS,
            render_verbatim: false,
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

    /// Override the indicator (custom frames, interval, verbatim flag).
    pub fn with_indicator(mut self, indicator: LoaderIndicatorOptions) -> Self {
        self.set_indicator(indicator);
        self
    }

    /// Replace the current indicator options at runtime.
    pub fn set_indicator(&mut self, indicator: LoaderIndicatorOptions) {
        self.frames = if indicator.frames.is_empty() {
            Vec::new()
        } else {
            indicator.frames
        };
        self.interval_ms = if indicator.interval_ms > 0 {
            indicator.interval_ms
        } else {
            DEFAULT_INDICATOR_INTERVAL_MS
        };
        self.render_verbatim = indicator.render_verbatim;
        self.frame = 0;
    }

    /// Advance to the next spinner frame. Call this on a timer.
    pub fn tick(&mut self) {
        if self.frames.is_empty() {
            return;
        }
        self.frame = (self.frame + 1) % self.frames.len();
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

    /// Recommended interval between `tick()` calls (informational).
    pub fn interval_ms(&self) -> u64 {
        self.interval_ms
    }
}

impl Component for LoaderComponent {
    fn render(&self, _width: u16) -> Vec<String> {
        if self.frames.is_empty() {
            return vec![format!(
                "{}{}\x1b[0m",
                self.message_color, self.message
            )];
        }
        let spinner = &self.frames[self.frame % self.frames.len()];
        let rendered_spinner = if self.render_verbatim {
            spinner.clone()
        } else {
            format!("{}{spinner}\x1b[0m", self.spinner_color)
        };
        vec![format!(
            "{rendered_spinner} {}{}\x1b[0m",
            self.message_color, self.message
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
        for _ in 0..DEFAULT_SPINNER_FRAMES.len() {
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

    #[test]
    fn test_loader_indicator_custom_frames() {
        let mut loader = LoaderComponent::new("Working");
        loader.set_indicator(LoaderIndicatorOptions {
            frames: vec!["A".into(), "B".into(), "C".into()],
            interval_ms: 200,
            render_verbatim: true,
        });
        assert_eq!(loader.interval_ms(), 200);

        let lines = loader.render(80);
        assert!(lines[0].starts_with("A "));
        // verbatim => no spinner color escape attached to the frame
        assert!(!lines[0].starts_with("\x1b[36m"));
        loader.tick();
        let lines = loader.render(80);
        assert!(lines[0].starts_with("B "));
        loader.tick();
        loader.tick();
        let lines = loader.render(80);
        assert!(lines[0].starts_with("A ")); // wraps
    }

    #[test]
    fn test_loader_indicator_empty_hides_spinner() {
        let loader = LoaderComponent::new("Working").with_indicator(LoaderIndicatorOptions {
            frames: vec![],
            interval_ms: 0,
            render_verbatim: false,
        });
        let lines = loader.render(80);
        assert!(lines[0].contains("Working"));
        // No spinner glyph emitted before the message.
        assert!(!lines[0].contains("⠋"));
    }
}
