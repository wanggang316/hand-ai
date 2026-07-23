//! Scrollback history insertion (skeleton).
//!
//! Will move finalized output above the live viewport into the terminal's
//! native scrollback. Content is pre-wrapped to the terminal width and
//! inserted via scroll regions (ratatui `scrolling-regions` feature) so the
//! viewport does not flicker.
