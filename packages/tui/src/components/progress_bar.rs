//! Progress bar component — renders a horizontal progress indicator.

use crate::tui::Component;

/// A horizontal progress bar.
pub struct ProgressBarComponent {
    /// Progress value between 0.0 and 1.0.
    progress: f64,
    /// Optional label shown before the bar.
    label: Option<String>,
    /// Fill character.
    fill_char: char,
    /// Empty character.
    empty_char: char,
    /// Whether to show percentage text.
    show_percentage: bool,
    /// ANSI color for the filled portion.
    fill_style: String,
}

impl ProgressBarComponent {
    pub fn new() -> Self {
        Self {
            progress: 0.0,
            label: None,
            fill_char: '█',
            empty_char: '░',
            show_percentage: true,
            fill_style: "\x1b[32m".to_string(), // green
        }
    }

    /// Set progress (clamped to 0.0..=1.0).
    pub fn set_progress(&mut self, value: f64) {
        self.progress = value.clamp(0.0, 1.0);
    }

    /// Set the label.
    pub fn set_label(&mut self, label: impl Into<String>) {
        self.label = Some(label.into());
    }

    /// Set whether to show the percentage.
    pub fn set_show_percentage(&mut self, show: bool) {
        self.show_percentage = show;
    }

    /// Set the fill style (ANSI escape).
    pub fn set_fill_style(&mut self, style: impl Into<String>) {
        self.fill_style = style.into();
    }

    /// Set fill and empty characters.
    pub fn set_chars(&mut self, fill: char, empty: char) {
        self.fill_char = fill;
        self.empty_char = empty;
    }
}

impl Default for ProgressBarComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl Component for ProgressBarComponent {
    fn render(&self, width: u16) -> Vec<String> {
        let w = width as usize;

        let prefix = self.label.as_deref().unwrap_or("");
        let prefix_len = if prefix.is_empty() {
            0
        } else {
            prefix.len() + 1 // +1 for space
        };

        let suffix = if self.show_percentage {
            format!(" {:>3}%", (self.progress * 100.0) as u32)
        } else {
            String::new()
        };
        let suffix_len = suffix.len();

        let bar_width = w.saturating_sub(prefix_len + suffix_len + 2); // 2 for [ ]
        let filled = (bar_width as f64 * self.progress) as usize;
        let empty = bar_width.saturating_sub(filled);

        let bar = format!(
            "[{}{}{}{}]",
            self.fill_style,
            self.fill_char.to_string().repeat(filled),
            "\x1b[0m",
            self.empty_char.to_string().repeat(empty),
        );

        let line = if prefix.is_empty() {
            format!("{}{}", bar, suffix)
        } else {
            format!("{} {}{}", prefix, bar, suffix)
        };

        vec![line]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_progress_bar_zero() {
        let bar = ProgressBarComponent::new();
        let lines = bar.render(40);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("0%"));
    }

    #[test]
    fn test_progress_bar_half() {
        let mut bar = ProgressBarComponent::new();
        bar.set_progress(0.5);
        let lines = bar.render(40);
        assert!(lines[0].contains("50%"));
    }

    #[test]
    fn test_progress_bar_full() {
        let mut bar = ProgressBarComponent::new();
        bar.set_progress(1.0);
        let lines = bar.render(40);
        assert!(lines[0].contains("100%"));
    }

    #[test]
    fn test_progress_bar_clamped() {
        let mut bar = ProgressBarComponent::new();
        bar.set_progress(1.5);
        assert!((bar.progress - 1.0).abs() < f64::EPSILON);
        bar.set_progress(-0.5);
        assert!(bar.progress.abs() < f64::EPSILON);
    }

    #[test]
    fn test_progress_bar_with_label() {
        let mut bar = ProgressBarComponent::new();
        bar.set_label("Loading");
        bar.set_progress(0.75);
        let lines = bar.render(60);
        assert!(lines[0].contains("Loading"));
        assert!(lines[0].contains("75%"));
    }

    #[test]
    fn test_progress_bar_no_percentage() {
        let mut bar = ProgressBarComponent::new();
        bar.set_show_percentage(false);
        bar.set_progress(0.5);
        let lines = bar.render(40);
        assert!(!lines[0].contains('%'));
    }
}
