//! Progress-bar primitive: a filled bar with a clamped numeric percentage.
//!
//! The rt counterpart to the legacy `ProgressBarComponent`, built on ratatui's
//! [`Gauge`], which draws the block-character bar and an inline percentage label
//! for us. The one behaviour this primitive owns is **clamping**: the ratio is
//! held to `[0.0, 1.0]` and the displayed percentage to `0..=100`, so an
//! out-of-range value (negative, or above 1.0 / 100) never paints past the bar or
//! prints a nonsensical percentage.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Gauge, Widget};

use crate::rt::events::RtKey;
use crate::rt::view::{HandleOutcome, RtComponent};

/// A horizontal progress bar with a clamped percentage readout.
pub struct ProgressBar {
    /// Progress ratio, always held in `[0.0, 1.0]`.
    ratio: f64,
    /// Optional label rendered before the percentage inside the gauge.
    label: Option<String>,
    /// Style of the filled portion of the bar.
    style: Style,
}

impl ProgressBar {
    /// A progress bar at 0%.
    pub fn new() -> Self {
        Self {
            ratio: 0.0,
            label: None,
            style: Style::default(),
        }
    }

    /// Set the progress ratio, clamped into `[0.0, 1.0]`.
    #[must_use]
    pub fn ratio(mut self, ratio: f64) -> Self {
        self.set_ratio(ratio);
        self
    }

    /// Set an optional leading label.
    #[must_use]
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Set the fill style of the bar.
    #[must_use]
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set the progress ratio at runtime, clamped into `[0.0, 1.0]`.
    ///
    /// A `NaN` input is treated as `0.0` (clamp cannot order it), so a bad
    /// computation never leaves the bar in an undefined fill state.
    pub fn set_ratio(&mut self, ratio: f64) {
        self.ratio = if ratio.is_nan() {
            0.0
        } else {
            ratio.clamp(0.0, 1.0)
        };
    }

    /// The clamped ratio currently held (`[0.0, 1.0]`).
    pub fn get_ratio(&self) -> f64 {
        self.ratio
    }

    /// The displayed integer percentage, clamped to `0..=100`.
    pub fn percent(&self) -> u16 {
        // `ratio` is already in `[0.0, 1.0]`, so this lands in `0..=100`; the
        // explicit `min` is belt-and-braces against any float rounding at the
        // boundary.
        ((self.ratio * 100.0).round() as u16).min(100)
    }
}

impl Default for ProgressBar {
    fn default() -> Self {
        Self::new()
    }
}

impl RtComponent for ProgressBar {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let mut gauge = Gauge::default()
            .gauge_style(self.style)
            .percent(self.percent());
        if let Some(label) = &self.label {
            gauge = gauge.label(label.clone());
        }
        gauge.render(area, buf);
    }

    fn handle_key(&mut self, _key: &RtKey) -> HandleOutcome {
        HandleOutcome::Ignored
    }
}
