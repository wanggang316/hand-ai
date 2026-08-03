//! Status-bar primitive: a single line split into left / center / right sections.
//!
//! The rt counterpart to the legacy `StatusBarComponent`. It lays three text
//! segments on **one** row: the left segment flush to the left edge, the right
//! segment flush to the right edge, and the center segment centered in the gap
//! between them. The single-line invariant is absolute — the bar never spills to
//! a second row. When the terminal is too narrow to hold all three, segments are
//! truncated (right, then center, then left) so the total always fits the width;
//! it is the truncation, not a wrap, that keeps the bar one row tall.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use super::{display_width, truncate_with_ellipsis};
use crate::rt::events::RtKey;
use crate::rt::view::{HandleOutcome, RtComponent};

/// A one-row status bar with left, center, and right sections.
pub struct StatusBar {
    left: String,
    center: String,
    right: String,
    style: Style,
}

impl StatusBar {
    /// An empty status bar with the default style.
    pub fn new() -> Self {
        Self {
            left: String::new(),
            center: String::new(),
            right: String::new(),
            style: Style::default(),
        }
    }

    /// Set the left section text.
    #[must_use]
    pub fn left(mut self, text: impl Into<String>) -> Self {
        self.left = text.into();
        self
    }

    /// Set the center section text.
    #[must_use]
    pub fn center(mut self, text: impl Into<String>) -> Self {
        self.center = text.into();
        self
    }

    /// Set the right section text.
    #[must_use]
    pub fn right(mut self, text: impl Into<String>) -> Self {
        self.right = text.into();
        self
    }

    /// Set the style applied to the whole bar.
    #[must_use]
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set the left section at runtime.
    pub fn set_left(&mut self, text: impl Into<String>) {
        self.left = text.into();
    }

    /// Set the center section at runtime.
    pub fn set_center(&mut self, text: impl Into<String>) {
        self.center = text.into();
    }

    /// Set the right section at runtime.
    pub fn set_right(&mut self, text: impl Into<String>) {
        self.right = text.into();
    }
}

impl Default for StatusBar {
    fn default() -> Self {
        Self::new()
    }
}

impl RtComponent for StatusBar {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let width = area.width as usize;
        let y = area.y;

        // Truncate the segments so all three fit on the single row. Priority when
        // the width is too small: the right segment yields first, then the center,
        // then the left — so the left (usually the identity/label) survives
        // longest. Each `truncate_with_ellipsis` clips to a column budget and
        // never overflows.
        let mut budget = width;

        let left = truncate_with_ellipsis(&self.left, budget);
        budget = budget.saturating_sub(display_width(&left));

        let right = truncate_with_ellipsis(&self.right, budget);
        budget = budget.saturating_sub(display_width(&right));

        let center = truncate_with_ellipsis(&self.center, budget);

        let left_w = display_width(&left);
        let center_w = display_width(&center);
        let right_w = display_width(&right);

        // Left segment hugs the left edge.
        buf.set_stringn(area.x, y, &left, width, self.style);

        // Right segment hugs the right edge: its start column is width - right_w.
        if right_w > 0 {
            let right_x = area.x + (width.saturating_sub(right_w)) as u16;
            buf.set_stringn(right_x, y, &right, right_w, self.style);
        }

        // Center segment sits centered in the gap between left and right. The gap
        // spans [left_w, width - right_w); the center is placed so it is
        // symmetric within that gap, clamped so it never overlaps the left
        // segment even in a tight fit.
        if center_w > 0 {
            let gap_start = left_w;
            let gap_end = width.saturating_sub(right_w);
            let gap = gap_end.saturating_sub(gap_start);
            let offset = gap.saturating_sub(center_w) / 2;
            let center_x = area.x + (gap_start + offset) as u16;
            buf.set_stringn(center_x, y, &center, center_w, self.style);
        }
    }

    fn handle_key(&mut self, _key: &RtKey) -> HandleOutcome {
        HandleOutcome::Ignored
    }
}
