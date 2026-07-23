//! Bottom-area geometry for the inline viewport.
//!
//! # Why the viewport height is *fixed*, not dynamic
//!
//! ratatui's inline viewport height (`Viewport::Inline(h)`) is set once at
//! `Terminal::with_options` time and only ever recomputed when the *backend*
//! size changes (a real terminal resize). There is no public API to change the
//! requested inline height at runtime (ratatui#984 open, PR#1964 unmerged). The
//! two viable workarounds are:
//!
//! - **A. Rebuild the `Terminal`** on every height change. Rejected: rebuilding
//!   re-runs `compute_inline_size` (which appends lines and may scroll), does
//!   *not* clear the old viewport rows, and so risks leaking the active area's
//!   border/spinner into scrollback — the loader-shrink-leak failure family. It
//!   also entangles the rebuild with the `insert_before` commit ordering the
//!   scheduler relies on.
//! - **B. Fix the viewport at its maximum height** and lay the bottom area out
//!   *inside* it, painting unused rows blank. Chosen. No `Terminal` churn, no
//!   rebuild flicker, no scrollback-leak path: every `terminal.draw` repaints
//!   the whole fixed buffer, so when the active area shrinks the vacated rows
//!   are diffed clear — no ghost border/spinner on screen or in scrollback. The
//!   `insert_before` height semantics established by the history sink are
//!   untouched (it keys off `viewport_area.width`, not the height).
//!
//! This module is the pure geometry core of strategy B: given the current input
//! row count, whether the loader is showing, and the terminal height, it decides
//! how tall the *active* bottom area is and how it partitions into loader / input
//! rows. It reads no terminal state, so it is unit-tested without a live backend.

use ratatui::layout::Rect;

/// The maximum number of rows the auto-growing input body may occupy. The input
/// grows one row per wrapped line of content from 1 up to this ceiling, then
/// stops growing (further content scrolls within the input body).
pub const MAX_INPUT_ROWS: u16 = 8;

/// The minimum number of input body rows: a single line, always visible.
pub const MIN_INPUT_ROWS: u16 = 1;

/// Rows consumed by the loader/spinner when it is showing (one row).
pub const LOADER_ROWS: u16 = 1;

/// Rows consumed by the surrounding border (top + bottom of the bordered block).
pub const BORDER_ROWS: u16 = 2;

/// The fixed inline-viewport height for strategy B: the tallest the bottom area
/// can ever be, so the viewport is reserved once at this size and the active
/// content is laid out within it.
///
/// That upper bound is: the border (top+bottom) + the loader row (assumed
/// present at the maximum) + the fully-grown input body. Fixing the viewport
/// here means a grow never needs to enlarge the viewport (which ratatui cannot
/// do at runtime) and a shrink never needs to move it — only the interior
/// layout changes, and the freed rows repaint blank.
pub const MAX_VIEWPORT_ROWS: u16 = BORDER_ROWS + LOADER_ROWS + MAX_INPUT_ROWS;

/// The concrete geometry of the bottom area for one frame, computed by
/// [`bottom_area_geometry`].
///
/// All rects are in viewport-local coordinates: `y = 0` is the top row of the
/// (fixed-height) viewport. The [`active`](BottomGeometry::active) rect is the
/// portion actually painted this frame; rows below it (up to the fixed viewport
/// height) are the blank remainder that repaints clear so a shrink leaves no
/// ghost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BottomGeometry {
    /// The fixed viewport height the terminal was built with — the ceiling every
    /// layout fits inside.
    pub viewport_height: u16,
    /// The rect actually occupied by the bordered bottom area this frame, always
    /// anchored to the *bottom* of the viewport so the freed rows are the blank
    /// band *above* it and the active UI hugs the history above it without a gap.
    pub active: Rect,
    /// The interior loader row, if the loader is showing this frame. `None` when
    /// the loader is hidden. Inside [`active`], below the top border.
    pub loader: Option<Rect>,
    /// The interior input-body rect (the auto-growing text area). Inside
    /// [`active`], below the loader (or below the top border when no loader).
    pub input: Rect,
}

/// Clamp the desired input row count into `[MIN_INPUT_ROWS, MAX_INPUT_ROWS]`.
///
/// The input auto-grows with its wrapped content: one row per visual line, from
/// a single line up to the ceiling. Zero content still shows one row so the
/// caret has a home; content past the ceiling stops growing the box (it scrolls
/// internally, which is not this module's concern).
#[must_use]
pub fn clamp_input_rows(desired: u16) -> u16 {
    desired.clamp(MIN_INPUT_ROWS, MAX_INPUT_ROWS)
}

/// Compute the bottom-area geometry for one frame under the fixed-max-viewport
/// strategy.
///
/// Inputs:
/// - `input_rows`: how many rows the input body *wants* (its wrapped line
///   count); clamped to `[MIN_INPUT_ROWS, MAX_INPUT_ROWS]`.
/// - `loader_visible`: whether the loader/spinner row is showing this frame.
/// - `width`: the viewport width (full terminal width for an inline viewport).
/// - `terminal_height`: the current terminal height, used to clamp the active
///   area so it always fits on a short pane (the 40×10 case).
///
/// The returned [`BottomGeometry::viewport_height`] is the *fixed* height the
/// terminal is (or should be) built with — [`MAX_VIEWPORT_ROWS`], itself clamped
/// to `terminal_height` so a pane shorter than the max never reserves more rows
/// than exist. The active rect is clamped to that same height, so on a tiny pane
/// the bottom UI is trimmed to fit rather than drawn stair-stepped past the
/// bottom edge.
#[must_use]
pub fn bottom_area_geometry(
    input_rows: u16,
    loader_visible: bool,
    width: u16,
    terminal_height: u16,
) -> BottomGeometry {
    // The fixed viewport can never be taller than the terminal itself.
    let viewport_height = MAX_VIEWPORT_ROWS.min(terminal_height.max(1));

    let input_rows = clamp_input_rows(input_rows);
    let loader_rows = if loader_visible { LOADER_ROWS } else { 0 };

    // Desired active height = border + optional loader + input body. Clamp it to
    // the fixed viewport height so a short pane trims the interior instead of
    // overflowing (no stair-step past the bottom edge).
    let desired_active = BORDER_ROWS
        .saturating_add(loader_rows)
        .saturating_add(input_rows);
    let active_height = desired_active.min(viewport_height);

    // Anchor the active area to the bottom of the fixed viewport so the blank
    // band left by a shrink sits *above* the box (between history and the box),
    // and the box stays flush with the history line above it.
    let active_y = viewport_height.saturating_sub(active_height);
    let active = Rect::new(0, active_y, width, active_height);

    // Partition the interior (inside the border) into an optional loader row and
    // the input body, recomputing against the *clamped* active height so the
    // interior never claims rows the clamp removed.
    let interior_top = active.y.saturating_add(1); // below the top border
    let interior_bottom = active.y.saturating_add(active_height).saturating_sub(1); // above the bottom border
    let interior_height = interior_bottom.saturating_sub(interior_top);

    let (loader, input) = partition_interior(
        active.x,
        interior_top,
        width,
        interior_height,
        loader_visible,
    );

    BottomGeometry {
        viewport_height,
        active,
        loader,
        input,
    }
}

/// Split the bordered interior into an optional loader row on top and the input
/// body beneath it, given the interior's origin, width, and total height.
///
/// When the interior is too short to hold the loader *and* at least one input
/// row (the tiny-pane clamp bit hard), the input body wins the available rows
/// and the loader is dropped for this frame — the caret must always have a home.
fn partition_interior(
    x: u16,
    top: u16,
    width: u16,
    interior_height: u16,
    loader_visible: bool,
) -> (Option<Rect>, Rect) {
    if loader_visible && interior_height > LOADER_ROWS {
        let loader = Rect::new(x, top, width, LOADER_ROWS);
        let input = Rect::new(
            x,
            top.saturating_add(LOADER_ROWS),
            width,
            interior_height.saturating_sub(LOADER_ROWS),
        );
        (Some(loader), input)
    } else {
        // No loader (hidden, or no room for it): the whole interior is input.
        let input = Rect::new(x, top, width, interior_height);
        (None, input)
    }
}
