//! Overlay view stack for the inline viewport.
//!
//! ratatui is a pure immediate-mode renderer: it has no layering, no modal
//! input, and no focus. This module is the thin application layer that gives the
//! rt stack a **codex-style in-viewport view stack** — overlays are layered
//! views drawn *inside* the inline viewport, not terminal-level floating windows.
//! It supplies the four things an interactive overlay needs and ratatui does not:
//!
//! - **Nine-anchor placement.** [`anchor_rect`] positions an overlay's rect by a
//!   nine-way [`OverlayAnchor`] (TopLeft … Center … BottomRight) plus an
//!   [`OverlayMargin`], then clamps it into the viewport so an oversized overlay
//!   never overflows the edges — and a full-bleed *bordered* child never pushes
//!   its right border past the last column (the historical border-overflow bug
//!   family). It reads no terminal state, so it is unit-tested as a pure function.
//! - **Modal capture with LIFO dispatch.** An [`OverlayStack`] routes a key to
//!   the topmost overlay first. A **capturing** (modal) overlay owns input: even
//!   a key it *ignores* is blocked from the layers below (lower overlays, the
//!   base view). A **non-capturing** overlay renders on top but lets a key it
//!   ignores fall through to the layer beneath — down to the base view.
//! - **Dimmed background.** A `dim_background` overlay marks the base cells
//!   outside it with the `DIM` modifier: the background stays visible but recedes,
//!   while the overlay itself paints crisp. On close the whole viewport repaints,
//!   so there is no dim residue, ghost border, or stale row (the residue family).
//! - **Async mounting.** An [`OverlayHandle`] is a cheap, cloneable, cross-task
//!   handle: a background task calls [`OverlayHandle::show`] / [`OverlayHandle::hide`]
//!   and the run loop drains the requests each tick via
//!   [`OverlayStack::drain_mounts`], mounting/unmounting and requesting a frame —
//!   so a background-mounted overlay paints itself with no keypress.

use std::sync::atomic::{AtomicU64, Ordering};

use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Clear, Widget};
use tokio::sync::mpsc;

use crate::rt::events::RtKey;
use crate::rt::view::{HandleOutcome, RtComponent};

/// Where an overlay is anchored within the viewport, before its margin is
/// applied.
///
/// The nine standard positions: three horizontal bands (left, center, right)
/// crossed with three vertical bands (top, center, bottom). The anchor fixes
/// which viewport edge(s) the overlay hugs; the [`OverlayMargin`] then offsets it
/// inward from the hugged edge(s). A centered axis ignores the perpendicular
/// margin (there is no edge to offset from).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayAnchor {
    /// Top edge, left edge.
    TopLeft,
    /// Top edge, horizontally centered.
    TopCenter,
    /// Top edge, right edge.
    TopRight,
    /// Vertically centered, left edge.
    CenterLeft,
    /// Vertically and horizontally centered.
    Center,
    /// Vertically centered, right edge.
    CenterRight,
    /// Bottom edge, left edge.
    BottomLeft,
    /// Bottom edge, horizontally centered.
    BottomCenter,
    /// Bottom edge, right edge.
    BottomRight,
}

impl OverlayAnchor {
    /// Whether this anchor hugs the top edge (so a `top` margin offsets it down).
    const fn is_top(self) -> bool {
        matches!(self, Self::TopLeft | Self::TopCenter | Self::TopRight)
    }

    /// Whether this anchor hugs the bottom edge (so a `bottom` margin offsets it
    /// up).
    const fn is_bottom(self) -> bool {
        matches!(
            self,
            Self::BottomLeft | Self::BottomCenter | Self::BottomRight
        )
    }

    /// Whether this anchor hugs the left edge (so a `left` margin offsets it
    /// right).
    const fn is_left(self) -> bool {
        matches!(self, Self::TopLeft | Self::CenterLeft | Self::BottomLeft)
    }

    /// Whether this anchor hugs the right edge (so a `right` margin offsets it
    /// left).
    const fn is_right(self) -> bool {
        matches!(self, Self::TopRight | Self::CenterRight | Self::BottomRight)
    }
}

/// Per-edge inset applied to an anchored overlay, in cells.
///
/// Each field offsets the overlay inward from the *anchored* edge: `top`/`left`
/// push a top/left-anchored overlay down/right, `bottom`/`right` push a
/// bottom/right-anchored overlay up/left. A centered axis ignores its margins
/// (there is no edge to inset from).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OverlayMargin {
    /// Inset from the top edge (applied only to a top-anchored overlay).
    pub top: u16,
    /// Inset from the right edge (applied only to a right-anchored overlay).
    pub right: u16,
    /// Inset from the bottom edge (applied only to a bottom-anchored overlay).
    pub bottom: u16,
    /// Inset from the left edge (applied only to a left-anchored overlay).
    pub left: u16,
}

impl OverlayMargin {
    /// A margin with the same inset on all four edges.
    #[must_use]
    pub const fn uniform(margin: u16) -> Self {
        Self {
            top: margin,
            right: margin,
            bottom: margin,
            left: margin,
        }
    }
}

/// Placement and presentation options for one overlay.
///
/// Deliberately the same vocabulary the codebase already uses for overlays —
/// anchor, margin, `capture_input`, `dim_background`, `border` — so the rt stack
/// reads familiarly. It is presentation + input policy only; the overlay's
/// content is an [`RtComponent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlayOptions {
    /// Which viewport edge(s) the overlay hugs before its margin is applied.
    pub anchor: OverlayAnchor,
    /// Per-edge inset from the anchored edge(s).
    pub margin: OverlayMargin,
    /// Whether this overlay is **modal**: while it is the topmost overlay it owns
    /// input, blocking every key (even one it ignores) from the layers below.
    pub capture_input: bool,
    /// Whether the base content outside this overlay is dimmed while it is open.
    pub dim_background: bool,
    /// Whether the overlay is wrapped in a box-drawing border.
    pub border: bool,
}

impl Default for OverlayOptions {
    /// A centered, modal, dimmed, bordered dialog — the common "dialog" default.
    fn default() -> Self {
        Self {
            anchor: OverlayAnchor::Center,
            margin: OverlayMargin::default(),
            capture_input: true,
            dim_background: true,
            border: true,
        }
    }
}

/// Position `content` inside `area` by `anchor` + `margin`, clamped to fit.
///
/// The pure geometry core of the overlay stack. Given the desired content
/// [`Size`], the viewport [`Rect`], the [`OverlayAnchor`], the [`OverlayMargin`],
/// and whether the overlay is bordered, it returns the overlay's rect in viewport
/// coordinates. Three guarantees hold, and are the reason this is a standalone,
/// backend-free function that is exhaustively unit-tested:
///
/// - **Anchor + margin.** The rect hugs the anchored edge(s) and is inset from
///   them by the margin; a centered axis is centered and ignores its margins.
/// - **Clamp into the viewport.** An overlay larger than the viewport (or pushed
///   past an edge by a margin) is first size-clamped to the viewport, then
///   position-clamped so its right/bottom never exceed the viewport's — it can
///   never overflow.
/// - **Full-bleed border safety.** Because the final rect is clamped to end at or
///   before the viewport's right/bottom edge, a bordered child that fills the
///   whole width still keeps its right border on the last column, never one past
///   it (the historical border-overflow bug family). The `bordered` flag is
///   accepted for symmetry and future per-border adjustments; the clamp already
///   makes the guarantee hold for any content.
#[must_use]
pub fn anchor_rect(
    content: Size,
    area: Rect,
    anchor: OverlayAnchor,
    margin: OverlayMargin,
    bordered: bool,
) -> Rect {
    // A bordered overlay still measures its *outer* rect here (the border is part
    // of the overlay's own area, painted by the component). The flag is kept so
    // the signature can grow per-border insets without a breaking change; the
    // clamp below is what actually guarantees the border never overflows.
    let _ = bordered;

    // Size-clamp first: an overlay can never be wider/taller than the viewport.
    let width = content.width.min(area.width);
    let height = content.height.min(area.height);

    // Horizontal placement: hug left, center, or hug right, then apply the
    // relevant margin. A centered axis ignores its margins.
    let x = if anchor.is_left() {
        area.x.saturating_add(margin.left)
    } else if anchor.is_right() {
        // Far edge minus the width, then inset by the right margin.
        area.right()
            .saturating_sub(width)
            .saturating_sub(margin.right)
    } else {
        // Centered: split the leftover width evenly.
        area.x.saturating_add(area.width.saturating_sub(width) / 2)
    };

    // Vertical placement: hug top, center, or hug bottom, then apply the margin.
    let y = if anchor.is_top() {
        area.y.saturating_add(margin.top)
    } else if anchor.is_bottom() {
        area.bottom()
            .saturating_sub(height)
            .saturating_sub(margin.bottom)
    } else {
        area.y
            .saturating_add(area.height.saturating_sub(height) / 2)
    };

    // Position-clamp: after the margin push, make sure the rect still ends inside
    // the viewport. This is the overflow guarantee — a right/bottom margin that
    // would push the rect past the far edge is absorbed here, and a full-bleed
    // bordered overlay keeps its right border on the last column.
    let max_x = area.right().saturating_sub(width);
    let max_y = area.bottom().saturating_sub(height);
    let x = x.clamp(area.x, max_x);
    let y = y.clamp(area.y, max_y);

    Rect::new(x, y, width, height)
}

/// A single layered view on the overlay stack.
///
/// Pairs an [`RtComponent`] (the content) with its [`OverlayOptions`] (placement
/// and input/presentation policy). The stack owns a `Vec<Overlay>`; the last
/// element is the topmost layer.
pub struct Overlay {
    /// The overlay's content, painted into its anchored rect each frame.
    component: Box<dyn RtComponent>,
    /// Placement, capture, dim, and border policy for this overlay.
    options: OverlayOptions,
}

impl Overlay {
    /// Build an overlay from a component and its options.
    #[must_use]
    pub fn new(component: Box<dyn RtComponent>, options: OverlayOptions) -> Self {
        Self { component, options }
    }

    /// This overlay's presentation/input options.
    #[must_use]
    pub const fn options(&self) -> &OverlayOptions {
        &self.options
    }

    /// Whether this overlay captures input (is modal).
    #[must_use]
    pub const fn captures_input(&self) -> bool {
        self.options.capture_input
    }
}

/// A cross-task, cloneable handle to mount and unmount overlays.
///
/// Cloning is trivial (an `mpsc::UnboundedSender` plus an atomic id counter). A
/// background task holds a clone and calls [`show`](OverlayHandle::show) /
/// [`hide`](OverlayHandle::hide); the run loop that owns the [`OverlayStack`]
/// drains the queued requests each tick via [`OverlayStack::drain_mounts`] and
/// applies them, requesting a frame when anything changed. This is what lets a
/// background-mounted overlay appear (and later disappear) with no user keypress.
///
/// The channel is unbounded and `show`/`hide` never block, so a producer on any
/// task can fire a mount request without awaiting the run loop.
#[derive(Clone)]
pub struct OverlayHandle {
    tx: mpsc::UnboundedSender<MountRequest>,
    next_id: std::sync::Arc<AtomicU64>,
}

/// An opaque identifier for a mounted overlay, returned by
/// [`OverlayHandle::show`] and passed back to [`OverlayHandle::hide`] to unmount
/// exactly that overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OverlayId(u64);

/// A queued mount/unmount request travelling from a background task to the run
/// loop over the [`OverlayHandle`]'s channel.
enum MountRequest {
    /// Mount this overlay on top, tagged with the id `show` handed back.
    Show(OverlayId, Overlay),
    /// Unmount the overlay with this id, wherever it is in the stack.
    Hide(OverlayId),
}

impl OverlayHandle {
    /// Request that `component` be mounted as a new topmost overlay with
    /// `options`, returning the [`OverlayId`] to later [`hide`](OverlayHandle::hide)
    /// it.
    ///
    /// Non-blocking: the request is queued on the channel and applied by the run
    /// loop's next [`OverlayStack::drain_mounts`]. If the stack has been dropped
    /// the send is silently ignored (a gone stack needs no overlays); the id is
    /// still returned so the caller's bookkeeping is uniform.
    pub fn show(&self, component: Box<dyn RtComponent>, options: OverlayOptions) -> OverlayId {
        let id = OverlayId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let _ = self
            .tx
            .send(MountRequest::Show(id, Overlay::new(component, options)));
        id
    }

    /// Request that the overlay with `id` be unmounted.
    ///
    /// Non-blocking and idempotent: unmounting an id that is not (or no longer)
    /// mounted is a no-op once the run loop drains it.
    pub fn hide(&self, id: OverlayId) {
        let _ = self.tx.send(MountRequest::Hide(id));
    }
}

/// A LIFO stack of layered overlays over the inline viewport.
///
/// Owns the overlays, routes input to them with modal-capture semantics, renders
/// them (with optional background dim and border) on top of an already-painted
/// base buffer, and drains cross-task mount requests. The last element is the
/// topmost layer.
pub struct OverlayStack {
    /// The overlays, bottom-to-top; `overlays.last()` is the topmost layer.
    overlays: Vec<(OverlayId, Overlay)>,
    /// Sender kept so [`mount_handle`](OverlayStack::mount_handle) can hand out
    /// cheap clones; the paired receiver is drained by
    /// [`drain_mounts`](OverlayStack::drain_mounts).
    tx: mpsc::UnboundedSender<MountRequest>,
    /// Receiver for cross-task mount/unmount requests.
    rx: mpsc::UnboundedReceiver<MountRequest>,
    /// Monotonic id source shared with every [`OverlayHandle`], so an id is
    /// unique across the process even as overlays come and go.
    next_id: std::sync::Arc<AtomicU64>,
    /// The last rect the topmost overlay was rendered at, used so a
    /// dispatch/hit-test could translate coordinates if needed. `None` until a
    /// render has happened.
    top_rect: Option<Rect>,
}

impl Default for OverlayStack {
    fn default() -> Self {
        Self::new()
    }
}

impl OverlayStack {
    /// An empty overlay stack with a fresh mount channel.
    #[must_use]
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            overlays: Vec::new(),
            tx,
            rx,
            next_id: std::sync::Arc::new(AtomicU64::new(0)),
            top_rect: None,
        }
    }

    /// Number of overlays currently mounted.
    #[must_use]
    pub fn len(&self) -> usize {
        self.overlays.len()
    }

    /// Whether the stack holds no overlays.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.overlays.is_empty()
    }

    /// Whether the topmost overlay captures input (the viewport is modal).
    ///
    /// `false` for an empty stack or a non-capturing top. Callers use this to
    /// decide whether the base view is currently reachable by keys at all.
    #[must_use]
    pub fn top_captures_input(&self) -> bool {
        self.overlays
            .last()
            .is_some_and(|(_, o)| o.captures_input())
    }

    /// Push an overlay onto the top of the stack, returning its id.
    ///
    /// The synchronous counterpart to [`OverlayHandle::show`], for callers on the
    /// run-loop thread that hold the stack directly.
    pub fn push(&mut self, overlay: Overlay) -> OverlayId {
        let id = OverlayId(self.next_id.fetch_add(1, Ordering::Relaxed));
        self.overlays.push((id, overlay));
        // Symmetry with pop/remove/drain_mounts: the new top has not rendered
        // yet, so the cached rect (belonging to the previous top) is stale until
        // the next render re-sets it.
        self.top_rect = None;
        id
    }

    /// Pop the topmost overlay, returning it (or `None` if the stack is empty).
    pub fn pop(&mut self) -> Option<Overlay> {
        self.top_rect = None;
        self.overlays.pop().map(|(_, o)| o)
    }

    /// Remove the overlay with `id`, if present, returning whether anything was
    /// removed.
    pub fn remove(&mut self, id: OverlayId) -> bool {
        let before = self.overlays.len();
        self.overlays.retain(|(existing, _)| *existing != id);
        let removed = self.overlays.len() != before;
        if removed {
            self.top_rect = None;
        }
        removed
    }

    /// A cross-task [`OverlayHandle`] for mounting/unmounting overlays from any
    /// task. Cheap to clone and hand to background work.
    #[must_use]
    pub fn mount_handle(&self) -> OverlayHandle {
        OverlayHandle {
            tx: self.tx.clone(),
            next_id: self.next_id.clone(),
        }
    }

    /// Drain every queued cross-task mount/unmount request, applying each in
    /// order, and report whether the stack changed.
    ///
    /// Called once per run-loop tick. A `true` return is the signal to
    /// `request_frame()`, so a background-mounted overlay is painted without a
    /// keypress; a `false` return (an empty drain) means nothing moved, so the
    /// run loop need not force a redraw.
    pub fn drain_mounts(&mut self) -> bool {
        let mut changed = false;
        while let Ok(request) = self.rx.try_recv() {
            match request {
                MountRequest::Show(id, overlay) => {
                    self.overlays.push((id, overlay));
                    changed = true;
                }
                MountRequest::Hide(id) => {
                    let before = self.overlays.len();
                    self.overlays.retain(|(existing, _)| *existing != id);
                    changed |= self.overlays.len() != before;
                }
            }
        }
        if changed {
            self.top_rect = None;
        }
        changed
    }

    /// Route a key through the overlay stack, then (if it falls through) the base.
    ///
    /// Dispatch walks the stack top-down:
    ///
    /// - The topmost overlay gets the key first. If it **consumes** it, dispatch
    ///   stops and returns [`HandleOutcome::Consumed`].
    /// - If that overlay **captures input** (is modal), dispatch stops *regardless*
    ///   of whether it consumed the key: even an ignored key is blocked from every
    ///   lower layer. The returned outcome mirrors what the overlay returned (so a
    ///   caller can still see it was ignored), but no lower layer — and not the
    ///   base — is consulted.
    /// - If the overlay is **non-capturing** and ignored the key, dispatch
    ///   continues to the next overlay down, and so on.
    /// - When every overlay has passed the key through (or there are none), it is
    ///   handed to `base` — the base view's handler — whose outcome is returned.
    ///
    /// This is the whole modal-capture / LIFO / passthrough contract in one pass.
    pub fn dispatch_key<F>(&mut self, key: &RtKey, base: F) -> HandleOutcome
    where
        F: FnOnce(&RtKey) -> HandleOutcome,
    {
        // Walk from the topmost overlay downward.
        for (_, overlay) in self.overlays.iter_mut().rev() {
            let outcome = overlay.component.handle_key(key);
            if outcome.is_consumed() {
                // Consumed anywhere stops the walk.
                return HandleOutcome::Consumed;
            }
            if overlay.captures_input() {
                // A modal layer blocks the key from everything below, even though
                // it ignored it. Report the ignore, but do not fall through.
                return HandleOutcome::Ignored;
            }
            // Non-capturing + ignored: fall through to the next layer down.
        }
        // No overlay captured or consumed it: the base view sees it.
        base(key)
    }

    /// Render every overlay in bottom-to-top order over the already-painted base
    /// `buf`, returning the topmost overlay's rect (or `None` for an empty stack).
    ///
    /// For each overlay, in stack order (so a later overlay paints over an earlier
    /// one):
    ///
    /// 1. If it dims the background, every base cell *outside* its rect gets the
    ///    `DIM` modifier — the background recedes but stays legible.
    /// 2. The overlay's rect is cleared (so no base content shows through) and, if
    ///    bordered, wrapped in a box; the component paints into the interior.
    ///
    /// Because the caller repaints the whole viewport each frame, closing an
    /// overlay (popping it) and re-rendering leaves no dim residue, ghost border,
    /// or stale row: the base is simply drawn crisp again with no overlay on top.
    /// The returned rect is the topmost overlay's outer rect, handy for hit-tests
    /// or focus placement.
    pub fn render(&mut self, area: Rect, buf: &mut Buffer) -> Option<Rect> {
        let mut top = None;
        // Collect the overlays' rects first (immutable borrow of options), so the
        // dim pass and the paint pass can each borrow `buf` mutably in turn.
        for (_, overlay) in &self.overlays {
            let opts = overlay.options;
            let content = overlay_content_size(area, opts);
            let rect = anchor_rect(content, area, opts.anchor, opts.margin, opts.border);

            if opts.dim_background {
                dim_outside(buf, area, rect);
            }

            // Clear the overlay's footprint so no base content bleeds through,
            // then paint the border (if any) and the component into the interior.
            Clear.render(rect, buf);
            let interior = if opts.border {
                let block = Block::bordered();
                let inner = block.inner(rect);
                block.render(rect, buf);
                inner
            } else {
                rect
            };
            overlay.component.render(interior, buf);

            top = Some(rect);
        }
        self.top_rect = top;
        top
    }

    /// The last rect the topmost overlay was rendered at, if any.
    #[must_use]
    pub const fn top_rect(&self) -> Option<Rect> {
        self.top_rect
    }
}

/// The desired outer size of an overlay within `area`.
///
/// The rt overlay model sizes an overlay to a comfortable fraction of the
/// viewport (a dialog is roughly 60% wide and short), clamped by [`anchor_rect`]
/// to never exceed the viewport. Kept small and deterministic so the anchor
/// geometry stays the single source of truth for placement; a component that
/// wants a specific size can be given a fixed-size wrapper in the future.
fn overlay_content_size(area: Rect, opts: OverlayOptions) -> Size {
    // A dialog spans ~60% of the width and a handful of rows; a bordered overlay
    // needs two extra rows/cols for the frame. Clamp to the viewport so a tiny
    // pane still yields a valid (possibly full-bleed) rect.
    let base_w = (area.width as u32 * 3 / 5) as u16;
    let width = base_w.max(if opts.border { 4 } else { 2 }).min(area.width);
    let height = 7u16.min(area.height);
    Size::new(width, height)
}

/// Apply the `DIM` modifier to every base cell of `area` that lies outside
/// `hole`.
///
/// This is the background-dim pass: the overlay's own footprint is left crisp,
/// while the surrounding base content is dimmed so the overlay stands out without
/// hiding what is underneath. Operates on the cells already painted by the base
/// pass, so it composes with any base styling rather than overwriting it.
fn dim_outside(buf: &mut Buffer, area: Rect, hole: Rect) {
    let dim = Style::default().add_modifier(Modifier::DIM);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let inside_hole =
                x >= hole.left() && x < hole.right() && y >= hole.top() && y < hole.bottom();
            if inside_hole {
                continue;
            }
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_style(dim);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchor_flags_partition_the_nine_positions() {
        // Top row hugs top, bottom row hugs bottom, middle row hugs neither.
        assert!(OverlayAnchor::TopCenter.is_top());
        assert!(!OverlayAnchor::TopCenter.is_bottom());
        assert!(OverlayAnchor::BottomCenter.is_bottom());
        assert!(!OverlayAnchor::Center.is_top() && !OverlayAnchor::Center.is_bottom());
        // Left column hugs left, right column hugs right, center hugs neither.
        assert!(OverlayAnchor::CenterLeft.is_left());
        assert!(OverlayAnchor::CenterRight.is_right());
        assert!(!OverlayAnchor::Center.is_left() && !OverlayAnchor::Center.is_right());
    }

    #[test]
    fn uniform_margin_sets_all_edges() {
        let m = OverlayMargin::uniform(3);
        assert_eq!((m.top, m.right, m.bottom, m.left), (3, 3, 3, 3));
    }

    #[test]
    fn anchor_rect_clamps_content_larger_than_area() {
        let area = Rect::new(0, 0, 10, 4);
        let rect = anchor_rect(
            Size::new(100, 100),
            area,
            OverlayAnchor::Center,
            OverlayMargin::default(),
            false,
        );
        assert_eq!(
            rect,
            Rect::new(0, 0, 10, 4),
            "clamped to the whole viewport"
        );
    }

    #[test]
    fn anchor_rect_absorbs_a_margin_that_would_overflow() {
        let area = Rect::new(0, 0, 20, 10);
        // A right anchor with a huge right margin cannot push the rect off-screen.
        let margin = OverlayMargin {
            right: 100,
            ..OverlayMargin::default()
        };
        let rect = anchor_rect(
            Size::new(6, 3),
            area,
            OverlayAnchor::TopRight,
            margin,
            false,
        );
        assert!(rect.left() >= area.left(), "clamped inside the left edge");
        assert!(
            rect.right() <= area.right(),
            "never overflows the right edge"
        );
    }
}
