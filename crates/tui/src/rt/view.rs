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
//!
//! Strategy B has one controlled exception (the M6 decision-log revision):
//! while a modal overlay panel is mounted, the driver rebuilds the terminal
//! taller via
//! [`set_inline_viewport_height`](crate::rt::session::set_inline_viewport_height)
//! — an erase-first rebuild that avoids the leak family strategy A was rejected
//! for — and lays the bottom area out inside the grown viewport with
//! [`bottom_area_geometry_within`], which takes the *actual* viewport height
//! instead of clamping to [`MAX_VIEWPORT_ROWS`]. On unmount the viewport
//! shrinks back and everything above holds again.

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};

use crate::rt::events::RtKey;

/// The maximum number of rows the auto-growing input body may occupy. The input
/// grows one row per wrapped line of content from 1 up to this ceiling, then
/// stops growing (further content scrolls within the input body).
///
/// `u16` for viewport-layout arithmetic. Shares its name/value with the editor's
/// `usize` [`crate::rt::components::editor::MAX_INPUT_ROWS`] by design — same 1→8
/// input-height policy, expressed in each module's natural integer type.
pub const MAX_INPUT_ROWS: u16 = 8;

/// The minimum number of input body rows: a single line, always visible.
///
/// `u16` counterpart of the editor's `usize`
/// [`crate::rt::components::editor::MIN_INPUT_ROWS`]; keep the two in step.
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

impl BottomGeometry {
    /// Translate every rect down by `dy` rows, mapping this viewport-local
    /// geometry into the viewport's absolute screen coordinates.
    ///
    /// [`bottom_area_geometry`] lays the bottom area out relative to the top of
    /// the viewport (`y = 0`). An inline viewport, though, is not pinned to the
    /// screen top: `insert_before` slides it downward as scrollback fills, so its
    /// origin (`Frame::area().y`) can be greater than zero — notably after an
    /// oversized commit re-anchors it near the bottom of the pane. Adding that
    /// origin here places the box, loader, and input body on the viewport's real
    /// rows; skipping it would paint the whole bottom UI at absolute row 0, off
    /// the bottom of a drifted viewport, and the box would appear to vanish.
    #[must_use]
    pub fn offset_y(self, dy: u16) -> Self {
        let shift = |rect: Rect| Rect {
            y: rect.y.saturating_add(dy),
            ..rect
        };
        Self {
            viewport_height: self.viewport_height,
            active: shift(self.active),
            loader: self.loader.map(shift),
            input: shift(self.input),
        }
    }
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
    bottom_area_geometry_within(
        input_rows,
        loader_visible,
        width,
        MAX_VIEWPORT_ROWS.min(terminal_height.max(1)),
    )
}

/// [`bottom_area_geometry`] against an explicit viewport height, without the
/// [`MAX_VIEWPORT_ROWS`] clamp.
///
/// The fixed-max callers derive their height budget from the terminal size and
/// go through [`bottom_area_geometry`]; this variant exists for the one caller
/// whose viewport is *taller* than the fixed maximum — the driver laying the
/// bottom area out inside a viewport grown for the modal overlay panel (built
/// via
/// [`set_inline_viewport_height`](crate::rt::session::set_inline_viewport_height)).
/// `viewport_rows` is the caller's real frame height (already bounded by the
/// terminal), so the active area still bottom-anchors against it and a short
/// pane still trims the interior instead of overflowing.
#[must_use]
pub fn bottom_area_geometry_within(
    input_rows: u16,
    loader_visible: bool,
    width: u16,
    viewport_rows: u16,
) -> BottomGeometry {
    let viewport_height = viewport_rows.max(1);

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

/// The current terminal geometry the runtime lays the bottom area out against.
///
/// A [`RtInputEvent::Resize { cols, rows }`](crate::rt::events::RtInputEvent)
/// carries the *whole* new size, so tracking is a plain overwrite — there is no
/// accumulation to get wrong. Holding it in one small value keeps the resize
/// hot path pure and cheap: a resize event folds into this via
/// [`TerminalSize::apply_resize`], which reports whether the size *actually*
/// changed so a resize storm that ends where it started (or a burst of
/// same-size events) drives no geometry churn — the scheduler still coalesces
/// the redraw, but the state update itself is a no-op when nothing moved.
///
/// It is deliberately just the two dimensions: the bottom-area layout is
/// recomputed from it (plus the input/loader state) by [`bottom_area_geometry`],
/// so there is a single source of truth for "how big is the terminal right now"
/// and re-anchoring after a resize is nothing more than laying the fixed
/// viewport out again against the new numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TerminalSize {
    /// Terminal width in columns. Drives the history pre-wrap width and the
    /// bottom-area rect width.
    pub cols: u16,
    /// Terminal height in rows. Bounds the fixed viewport and clamps the active
    /// bottom area on a short pane.
    pub rows: u16,
}

impl TerminalSize {
    /// A size with the given dimensions.
    #[must_use]
    pub const fn new(cols: u16, rows: u16) -> Self {
        Self { cols, rows }
    }

    /// Fold a resize event's `(cols, rows)` into this size, returning `true`
    /// when the size actually changed.
    ///
    /// A resize event carries the complete new geometry, so applying it is an
    /// overwrite. The returned flag is the whole "avoid redundant reflow" story:
    /// a duplicate or storm-settling event whose numbers match the current size
    /// changes nothing and returns `false`, so a caller can skip re-deriving
    /// geometry (the scheduler's frame coalescing already collapses the redraws;
    /// this collapses the *state* churn behind them).
    #[must_use]
    pub fn apply_resize(&mut self, cols: u16, rows: u16) -> bool {
        let next = Self { cols, rows };
        if *self == next {
            return false;
        }
        *self = next;
        true
    }

    /// Recompute the bottom-area geometry against this size.
    ///
    /// Thin convenience over [`bottom_area_geometry`] that pairs the tracked
    /// size with the current input/loader state, so a resize handler reads as
    /// "fold the event, then re-lay-out": exactly the two steps re-anchoring the
    /// fixed viewport after a resize takes.
    #[must_use]
    pub fn bottom_geometry(&self, input_rows: u16, loader_visible: bool) -> BottomGeometry {
        bottom_area_geometry(input_rows, loader_visible, self.cols, self.rows)
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

// ---------------------------------------------------------------------------
// Component / focus / dispatch model
// ---------------------------------------------------------------------------
//
// ratatui is a pure immediate-mode renderer with no event system, no widget
// identity, and no focus. This is the thin application layer that gives the rt
// stack the three things an interactive viewport needs and ratatui does not
// provide:
//
// - a **component** abstraction that both paints into a ratatui `Buffer` and
//   consumes keys (unlike the legacy `Component`, which only renders to
//   `Vec<String>` and never sees input);
// - **exclusive focus routing**, so exactly one component receives keys and the
//   rest are frozen — a background stream can flood the viewport while typed
//   characters land only in the focused input; and
// - **hardware-cursor-follows-focus**: the focused component reports where its
//   caret is, the view surfaces that as an `Option<Position>`, and the draw glue
//   feeds it to ratatui's `Frame::set_cursor_position` — a caret-less focus
//   (`None`) hides the real cursor rather than stranding it in the output.

/// Whether a component consumed a key or let it bubble.
///
/// Returned by [`RtComponent::handle_key`]. [`FocusView`] routes a key to the
/// focused component and inspects this to decide whether the key was handled or
/// should fall through to view-level handling (e.g. a focus-switch key the
/// focused component ignored). It is the dispatch layer's "stop propagation"
/// signal, kept deliberately binary — richer routing (capture/overlay) is a
/// later concern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleOutcome {
    /// The component handled the key; it must not bubble further.
    Consumed,
    /// The component did not handle the key; the dispatcher may act on it (e.g.
    /// a view-level focus switch).
    Ignored,
}

impl HandleOutcome {
    /// Whether the key was consumed (handled) by the component.
    #[must_use]
    pub const fn is_consumed(self) -> bool {
        matches!(self, HandleOutcome::Consumed)
    }
}

/// A focusable, key-consuming piece of UI painted into a ratatui [`Buffer`].
///
/// The rt-native counterpart to the legacy `Component` (which renders to
/// `Vec<String>` and never handles input). An `RtComponent`:
///
/// - **renders** itself into a sub-rect of the frame buffer ([`render`]) —
///   immediate mode, called every frame by the draw path;
/// - **handles keys** ([`handle_key`]) only while it is the focused component,
///   returning a [`HandleOutcome`] so the dispatcher knows whether the key was
///   consumed or should bubble; and
/// - optionally **reports its caret** ([`cursor`]) as a viewport-local
///   [`Position`], so the view can drive the real hardware cursor to the
///   insertion point. A component with no caret (a read-only block) returns
///   `None`, and the view hides the hardware cursor while it is focused.
///
/// [`render`]: RtComponent::render
/// [`handle_key`]: RtComponent::handle_key
/// [`cursor`]: RtComponent::cursor
pub trait RtComponent {
    /// Paint this component into `area` of the frame buffer. Immediate mode:
    /// called every frame, must not retain the buffer.
    fn render(&self, area: Rect, buf: &mut Buffer);

    /// Handle a key while focused, reporting whether it was consumed.
    ///
    /// Only ever called by [`FocusView`] on the currently focused component, so
    /// an unfocused component's state is frozen — it never sees a key. Returning
    /// [`HandleOutcome::Ignored`] lets the key bubble to the view (e.g. so a
    /// focus-switch key works even while an input is focused).
    fn handle_key(&mut self, key: &RtKey) -> HandleOutcome;

    /// The component's caret position, if it has one, in the same coordinate
    /// space its [`render`](RtComponent::render) area was given.
    ///
    /// Returns the *content-local* offset within the last rendered area (see
    /// [`FocusView::cursor`], which translates it to viewport coordinates). A
    /// component with no caret (a read-only block) returns `None`, which the
    /// view turns into a hidden hardware cursor.
    fn cursor(&self) -> Option<Position> {
        None
    }
}

/// A container of focusable [`RtComponent`]s with exactly one focused at a time
/// and exclusive key routing.
///
/// This is the whole focus/dispatch model:
///
/// - **Exactly one focus.** [`focused`](FocusView::focused) names the component
///   that receives keys; it is always a valid index while the view is non-empty.
/// - **Exclusive routing.** [`dispatch_key`](FocusView::dispatch_key) hands a key
///   *only* to the focused component. Unfocused components never see a key, so
///   their state is frozen — the guarantee behind "typed chars land only in the
///   focused component" even while a background stream floods the viewport.
/// - **Cursor follows focus.** [`cursor`](FocusView::cursor) reports the focused
///   component's caret (translated into viewport coordinates via the rect it was
///   last laid out at), or `None` when the focused component is caret-less — the
///   signal the draw glue feeds to `Frame::set_cursor_position` so the hardware
///   cursor tracks focus and is hidden on a caret-less focus.
///
/// Focus is switched with [`focus_next`](FocusView::focus_next) /
/// [`focus`](FocusView::focus). The view stores, per component, the rect it was
/// last rendered at, so `cursor` can turn a content-local caret into an absolute
/// viewport position without the caller re-deriving geometry.
pub struct FocusView {
    /// The focusable components, in focus-cycle order.
    components: Vec<Box<dyn RtComponent>>,
    /// The rect each component was last rendered at, parallel to `components`.
    /// Used to translate a focused component's content-local caret into an
    /// absolute viewport position. `None` until the component has been rendered
    /// at least once.
    areas: Vec<Option<Rect>>,
    /// Index of the focused component. Invariant: `< components.len()` whenever
    /// the view is non-empty.
    focused: usize,
}

impl FocusView {
    /// Build a view over `components`, focusing the first one.
    ///
    /// An empty view is legal (it routes nothing and reports no cursor); a
    /// non-empty view always has a valid focused index.
    #[must_use]
    pub fn new(components: Vec<Box<dyn RtComponent>>) -> Self {
        let areas = vec![None; components.len()];
        Self {
            components,
            areas,
            focused: 0,
        }
    }

    /// Number of components in the view.
    #[must_use]
    pub fn len(&self) -> usize {
        self.components.len()
    }

    /// Whether the view holds no components.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.components.is_empty()
    }

    /// Index of the currently focused component (`0` for an empty view, which
    /// has no component to focus).
    #[must_use]
    pub fn focused(&self) -> usize {
        self.focused
    }

    /// Focus the component at `index`, if it is in range. A no-op otherwise, so
    /// an out-of-range request never leaves focus in an invalid state.
    pub fn focus(&mut self, index: usize) {
        if index < self.components.len() {
            self.focused = index;
        }
    }

    /// Move focus to the next component, wrapping past the last back to the
    /// first. A no-op on an empty view.
    ///
    /// This is the mechanism a focus-switch key (Tab, F2, …) drives. Because
    /// routing is exclusive, switching focus is exactly what redirects
    /// subsequent keys from one component to another — the old component's
    /// state freezes the instant focus leaves it.
    pub fn focus_next(&mut self) {
        if self.components.is_empty() {
            return;
        }
        self.focused = (self.focused + 1) % self.components.len();
    }

    /// Borrow the focused component immutably, if any.
    #[must_use]
    pub fn focused_component(&self) -> Option<&dyn RtComponent> {
        self.components.get(self.focused).map(Box::as_ref)
    }

    /// Route a key to the focused component **exclusively**.
    ///
    /// Only the focused component's [`handle_key`](RtComponent::handle_key) runs;
    /// every other component is untouched (frozen). Returns the focused
    /// component's [`HandleOutcome`] so the caller can act on an ignored key
    /// (e.g. treat it as a view-level focus switch). An empty view ignores every
    /// key.
    pub fn dispatch_key(&mut self, key: &RtKey) -> HandleOutcome {
        match self.components.get_mut(self.focused) {
            Some(component) => component.handle_key(key),
            None => HandleOutcome::Ignored,
        }
    }

    /// Render every component into its slice of `layout`, recording each rect so
    /// [`cursor`](FocusView::cursor) can later place the hardware cursor.
    ///
    /// `layout` must yield one rect per component, in component order. Rects are
    /// in viewport coordinates (the space `Frame::set_cursor_position` expects),
    /// so the recorded rect plus the focused component's content-local caret give
    /// the absolute cursor position.
    pub fn render(&mut self, layout: &[Rect], buf: &mut Buffer) {
        for (i, component) in self.components.iter().enumerate() {
            let Some(&area) = layout.get(i) else { break };
            self.areas[i] = Some(area);
            component.render(area, buf);
        }
    }

    /// The absolute viewport position of the focused component's caret, or `None`
    /// when there is no caret to show.
    ///
    /// Returns `None` — meaning "hide the hardware cursor" — when the view is
    /// empty, when the focused component has not been rendered yet, or when the
    /// focused component reports no caret (a read-only block). Otherwise it
    /// offsets the component's content-local caret by the origin of the rect it
    /// was last rendered at, clamped inside that rect so a caret can never stray
    /// outside the component's own area (and thus never into another region's
    /// output).
    #[must_use]
    pub fn cursor(&self) -> Option<Position> {
        let component = self.components.get(self.focused)?;
        let area = (*self.areas.get(self.focused)?)?;
        let caret = component.cursor()?;
        Some(clamp_cursor_into(area, caret))
    }
}

/// Translate a component-local caret into an absolute viewport position, clamped
/// inside `area`.
///
/// The caret is an offset within the component's own render area; adding the
/// area's origin gives the viewport position, and clamping to the area's last
/// paintable cell guarantees the hardware cursor can never land outside the
/// focused component — the "never a stray cursor in the output" guarantee. A
/// zero-sized area collapses to its origin.
fn clamp_cursor_into(area: Rect, caret: Position) -> Position {
    let max_x = area.x.saturating_add(area.width.saturating_sub(1));
    let max_y = area.y.saturating_add(area.height.saturating_sub(1));
    Position::new(
        area.x.saturating_add(caret.x).min(max_x),
        area.y.saturating_add(caret.y).min(max_y),
    )
}
