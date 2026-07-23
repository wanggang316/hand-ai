//! Spacer primitive: reserves exactly N blank rows.
//!
//! The rt counterpart to the legacy `SpacerComponent`. It paints nothing; its
//! sole purpose is to consume a fixed number of rows in a vertical layout so
//! sibling components are separated by a precise gap. The row count it *reports*
//! ([`Spacer::rows`]) is what a layout caller reserves for it; the render is a
//! no-op because the buffer's background already provides the blank rows.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::rt::events::RtKey;
use crate::rt::view::{HandleOutcome, RtComponent};

/// A vertical gap of exactly `rows` blank rows.
pub struct Spacer {
    rows: u16,
}

impl Spacer {
    /// A spacer occupying exactly `rows` rows.
    pub fn new(rows: u16) -> Self {
        Self { rows }
    }

    /// The number of rows this spacer reserves. A vertical layout gives the
    /// spacer a rect this many rows tall; the render itself paints nothing.
    pub fn rows(&self) -> u16 {
        self.rows
    }

    /// Change the reserved row count.
    pub fn set_rows(&mut self, rows: u16) {
        self.rows = rows;
    }
}

impl RtComponent for Spacer {
    fn render(&self, _area: Rect, _buf: &mut Buffer) {
        // Intentionally empty: a spacer is pure vertical space. The blank rows
        // come from the buffer background; painting them explicitly would be
        // redundant and would fight a background fill the container may have set.
    }

    fn handle_key(&mut self, _key: &RtKey) -> HandleOutcome {
        HandleOutcome::Ignored
    }
}
