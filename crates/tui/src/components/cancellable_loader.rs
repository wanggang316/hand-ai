//! Cancellable loader — animated spinner with progress and cancel support.
//
// audit: M3.T5 — surface superset of upstream TUI/cancellable-loader.ts on 2026-05-07.
// TS `CancellableLoader` extends `Loader` and exposes an `AbortController`/
// `signal` for upstream tasks. The Rust port surfaces an `is_cancelled()` flag
// instead — async cancellation is the host's concern (e.g. `tokio::select!`
// against the application's own `CancellationToken`).

use crate::tui::{Component, HandleResult, InputEvent};

/// A loader that supports cancellation and progress tracking.
#[derive(Debug)]
pub struct CancellableLoaderComponent {
    /// Spinner frames.
    frames: Vec<String>,
    /// Current frame index.
    frame_index: usize,
    /// Message displayed next to spinner.
    message: String,
    /// Cancel hint (e.g., "Press Escape to cancel").
    cancel_hint: String,
    /// Optional progress (0.0 to 1.0).
    progress: Option<f64>,
    /// Whether cancelled.
    cancelled: bool,
    /// Elapsed time display.
    elapsed: Option<String>,
}

impl CancellableLoaderComponent {
    /// Create a new cancellable loader with a message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            frames: vec![
                "⠋".to_string(),
                "⠙".to_string(),
                "⠹".to_string(),
                "⠸".to_string(),
                "⠼".to_string(),
                "⠴".to_string(),
                "⠦".to_string(),
                "⠧".to_string(),
                "⠇".to_string(),
                "⠏".to_string(),
            ],
            frame_index: 0,
            message: message.into(),
            cancel_hint: "Press Escape to cancel".to_string(),
            progress: None,
            cancelled: false,
            elapsed: None,
        }
    }

    /// Set the message.
    pub fn set_message(&mut self, message: impl Into<String>) {
        self.message = message.into();
    }

    /// Set the cancel hint text.
    pub fn set_cancel_hint(&mut self, hint: impl Into<String>) {
        self.cancel_hint = hint.into();
    }

    /// Set progress (0.0 to 1.0). None to hide progress.
    pub fn set_progress(&mut self, progress: Option<f64>) {
        self.progress = progress.map(|p| p.clamp(0.0, 1.0));
    }

    /// Set elapsed time display.
    pub fn set_elapsed(&mut self, elapsed: Option<String>) {
        self.elapsed = elapsed;
    }

    /// Advance the spinner animation.
    pub fn tick(&mut self) {
        self.frame_index = (self.frame_index + 1) % self.frames.len();
    }

    /// Check if cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    /// Reset cancelled state.
    pub fn reset(&mut self) {
        self.cancelled = false;
    }

    fn handle_raw(&mut self, data: &str) -> HandleResult {
        // Escape key cancels.
        if data == "\x1b" {
            self.cancelled = true;
            return HandleResult::Handled;
        }
        HandleResult::Ignored
    }
}

impl Component for CancellableLoaderComponent {
    fn render(&self, width: u16) -> Vec<String> {
        let width = width as usize;
        let spinner = &self.frames[self.frame_index % self.frames.len()];
        let mut lines = Vec::new();

        // Main line: spinner + message + optional elapsed
        let main = if let Some(ref elapsed) = self.elapsed {
            format!("{spinner} {} ({elapsed})", self.message)
        } else {
            format!("{spinner} {}", self.message)
        };

        let main = if main.len() > width {
            main[..width].to_string()
        } else {
            main
        };
        lines.push(main);

        // Progress bar if set
        if let Some(progress) = self.progress {
            let bar_width = (width - 8).min(40);
            let filled = (progress * bar_width as f64) as usize;
            let empty = bar_width - filled;
            let pct = (progress * 100.0) as u32;
            let bar = format!(
                "  [{}{}] {:>3}%",
                "█".repeat(filled),
                "░".repeat(empty),
                pct
            );
            lines.push(bar);
        }

        // Cancel hint
        if !self.cancel_hint.is_empty() {
            lines.push(format!("  {}", self.cancel_hint));
        }

        lines
    }

    fn handle_input(&mut self, event: &InputEvent) -> HandleResult {
        match event {
            InputEvent::Raw(data) | InputEvent::Paste(data) => self.handle_raw(data),
            _ => HandleResult::Ignored,
        }
    }

    fn invalidate(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_basic() {
        let loader = CancellableLoaderComponent::new("Loading...");
        let lines = loader.render(80);
        assert!(lines.len() >= 2);
        assert!(lines[0].contains("Loading..."));
        assert!(lines[1].contains("Escape"));
    }

    #[test]
    fn render_with_progress() {
        let mut loader = CancellableLoaderComponent::new("Working");
        loader.set_progress(Some(0.5));
        let lines = loader.render(80);
        assert!(lines.len() >= 3);
        assert!(lines[1].contains("50%"));
    }

    #[test]
    fn tick_advances_frame() {
        let mut loader = CancellableLoaderComponent::new("test");
        let lines1 = loader.render(80);
        loader.tick();
        let lines2 = loader.render(80);
        // Frames should differ (different spinner character)
        assert_ne!(lines1[0], lines2[0]);
    }

    #[test]
    fn escape_cancels() {
        let mut loader = CancellableLoaderComponent::new("test");
        assert!(!loader.is_cancelled());
        loader.handle_input(&InputEvent::Raw("\x1b".into()));
        assert!(loader.is_cancelled());
    }

    #[test]
    fn render_with_elapsed() {
        let mut loader = CancellableLoaderComponent::new("Working");
        loader.set_elapsed(Some("3.2s".to_string()));
        let lines = loader.render(80);
        assert!(lines[0].contains("3.2s"));
    }
}
