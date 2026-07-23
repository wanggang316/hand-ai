//! Tests for the rt overlay view stack (`hand_tui::rt::overlay`).
//!
//! ratatui is a pure immediate-mode renderer with no layering, no modal input,
//! and no focus. [`OverlayStack`] is the application layer that gives the rt
//! stack a codex-style in-viewport view stack: overlays are layered views inside
//! the inline viewport (not terminal-level floats), anchored nine ways with a
//! margin and clamped into the viewport, routing input with modal-capture LIFO
//! semantics.
//!
//! This file pins the *unit-level* contract the external validator's runtime
//! probes have an in-process match for — assertion **VAL-OVERLAY-030**:
//!
//! - **LIFO capture.** The topmost capturing overlay owns input; a lower overlay
//!   (or the base view) never sees a key while it is open.
//! - **Ignore blocks.** A capturing overlay that *ignores* a key still blocks it
//!   from the layers below — capture is about the layer, not the key.
//! - **Non-capturing passthrough.** A non-capturing overlay renders on top but
//!   lets keys fall through to the layer beneath it (down to the base view).
//! - **Nine-anchor geometry.** Every anchor + margin lands the overlay at the
//!   right point, and an oversized overlay clamps into the viewport rather than
//!   overflowing it.
//! - **Full-bleed bordered child never overflows.** A bordered overlay sized to
//!   the full viewport width keeps its right border inside the viewport (the
//!   historical border-overflow bug family).
//!
//! The residue-free unmount and dim-background rendering are pinned end-to-end
//! over ratatui's `TestBackend`; the anchor geometry is pinned as a pure
//! function with no backend at all.

use std::cell::RefCell;
use std::rc::Rc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hand_tui::rt::events::RtKey;
use hand_tui::rt::overlay::{
    Overlay, OverlayAnchor, OverlayMargin, OverlayOptions, OverlayStack, anchor_rect,
};
use hand_tui::rt::view::{HandleOutcome, RtComponent};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Widget};
use ratatui::{Terminal, TerminalOptions, Viewport};

// --- test helpers -----------------------------------------------------------

/// Build an `RtKey` for a single printable character with no modifiers.
fn char_key(c: char) -> RtKey {
    RtKey {
        key_id: Some(c.to_string()),
        raw: KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
    }
}

/// A shared, cloneable record of the keys a mock component was handed, so a test
/// can assert which layer keys were routed to.
type KeyLog = Rc<RefCell<Vec<String>>>;

/// A mock component that records every key it is handed and consumes only the
/// characters in its `consume` set, ignoring the rest. The selective consume is
/// how a test drives the "capturing overlay ignores a key but still blocks it"
/// path: the overlay ignores the key, yet no lower layer sees it.
struct MockComponent {
    log: KeyLog,
    /// Characters this component consumes; any other key is ignored (bubbles).
    consume: Vec<char>,
    /// The label painted into the component's area, so a render test can find it.
    label: String,
}

impl MockComponent {
    fn new(log: KeyLog, consume: Vec<char>, label: &str) -> Self {
        Self {
            log,
            consume,
            label: label.to_string(),
        }
    }
}

impl RtComponent for MockComponent {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        buf.set_string(
            area.x,
            area.y,
            &self.label,
            ratatui::style::Style::default(),
        );
    }

    fn handle_key(&mut self, key: &RtKey) -> HandleOutcome {
        self.log
            .borrow_mut()
            .push(key.key_id.clone().unwrap_or_default());
        match key.raw.code {
            KeyCode::Char(c) if self.consume.contains(&c) => HandleOutcome::Consumed,
            _ => HandleOutcome::Ignored,
        }
    }
}

fn opts(anchor: OverlayAnchor, capture: bool) -> OverlayOptions {
    OverlayOptions {
        anchor,
        margin: OverlayMargin::default(),
        capture_input: capture,
        dim_background: false,
        border: false,
    }
}

// ===========================================================================
// VAL-OVERLAY-030 — modal capture / LIFO / passthrough dispatch
// ===========================================================================

/// The topmost capturing overlay owns input: neither a lower overlay nor the
/// base view sees the key. LIFO — the last-pushed capturing overlay wins.
#[test]
fn capturing_overlay_owns_input_lifo() {
    let base_log: KeyLog = Rc::new(RefCell::new(Vec::new()));
    let lower_log: KeyLog = Rc::new(RefCell::new(Vec::new()));
    let top_log: KeyLog = Rc::new(RefCell::new(Vec::new()));

    let mut stack = OverlayStack::new();
    // Two capturing overlays; the top (pushed last) must win.
    stack.push(Overlay::new(
        Box::new(MockComponent::new(lower_log.clone(), vec!['a'], "lower")),
        opts(OverlayAnchor::Center, true),
    ));
    stack.push(Overlay::new(
        Box::new(MockComponent::new(top_log.clone(), vec!['a'], "top")),
        opts(OverlayAnchor::Center, true),
    ));

    // The base view's handler: it records the key. It must never fire while a
    // capturing overlay is open.
    let base_seen = base_log.clone();
    let handled = stack.dispatch_key(&char_key('a'), |key| {
        base_seen
            .borrow_mut()
            .push(key.key_id.clone().unwrap_or_default());
        HandleOutcome::Consumed
    });

    assert_eq!(handled, HandleOutcome::Consumed);
    assert_eq!(
        top_log.borrow().as_slice(),
        &["a".to_string()],
        "the topmost capturing overlay must receive the key",
    );
    assert!(
        lower_log.borrow().is_empty(),
        "a lower overlay must not see the key while a higher one captures",
    );
    assert!(
        base_log.borrow().is_empty(),
        "the base view must not see the key while a capturing overlay is open",
    );
}

/// A capturing overlay that *ignores* a key still blocks it from the layers
/// below: capture is a property of the layer, not the individual key.
#[test]
fn capturing_overlay_ignore_still_blocks_lower_layers() {
    let base_log: KeyLog = Rc::new(RefCell::new(Vec::new()));
    let top_log: KeyLog = Rc::new(RefCell::new(Vec::new()));

    let mut stack = OverlayStack::new();
    // Capturing overlay that consumes nothing → it ignores every key.
    stack.push(Overlay::new(
        Box::new(MockComponent::new(top_log.clone(), vec![], "modal")),
        opts(OverlayAnchor::Center, true),
    ));

    let base_seen = base_log.clone();
    let handled = stack.dispatch_key(&char_key('z'), |key| {
        base_seen
            .borrow_mut()
            .push(key.key_id.clone().unwrap_or_default());
        HandleOutcome::Consumed
    });

    // The overlay saw and ignored the key…
    assert_eq!(top_log.borrow().as_slice(), &["z".to_string()]);
    // …but the base view still never saw it (capture blocks the fall-through)…
    assert!(
        base_log.borrow().is_empty(),
        "a capturing overlay must block a key it ignored from lower layers",
    );
    // …and the stack reports the key as absorbed by the modal layer.
    assert_eq!(
        handled,
        HandleOutcome::Ignored,
        "the overlay ignored the key, so the outcome bubbles as Ignored — but \
         dispatch stopped at the capturing layer",
    );
}

/// A non-capturing overlay renders on top but lets keys fall through: the key it
/// ignores reaches the base view.
#[test]
fn non_capturing_overlay_passes_keys_through() {
    let base_log: KeyLog = Rc::new(RefCell::new(Vec::new()));
    let overlay_log: KeyLog = Rc::new(RefCell::new(Vec::new()));

    let mut stack = OverlayStack::new();
    // Non-capturing overlay that consumes nothing.
    stack.push(Overlay::new(
        Box::new(MockComponent::new(overlay_log.clone(), vec![], "toast")),
        opts(OverlayAnchor::TopRight, false),
    ));

    let base_seen = base_log.clone();
    let handled = stack.dispatch_key(&char_key('k'), |key| {
        base_seen
            .borrow_mut()
            .push(key.key_id.clone().unwrap_or_default());
        HandleOutcome::Consumed
    });

    assert_eq!(
        overlay_log.borrow().as_slice(),
        &["k".to_string()],
        "a non-capturing overlay still gets first look at the key",
    );
    assert_eq!(
        base_log.borrow().as_slice(),
        &["k".to_string()],
        "a key the non-capturing overlay ignored must fall through to the base",
    );
    assert_eq!(
        handled,
        HandleOutcome::Consumed,
        "the base consumed the key"
    );
}

/// A non-capturing overlay that *consumes* a key stops it there — passthrough is
/// only for keys it ignores.
#[test]
fn non_capturing_overlay_consumes_and_stops() {
    let base_log: KeyLog = Rc::new(RefCell::new(Vec::new()));
    let overlay_log: KeyLog = Rc::new(RefCell::new(Vec::new()));

    let mut stack = OverlayStack::new();
    stack.push(Overlay::new(
        Box::new(MockComponent::new(overlay_log.clone(), vec!['x'], "toast")),
        opts(OverlayAnchor::TopRight, false),
    ));

    let base_seen = base_log.clone();
    let handled = stack.dispatch_key(&char_key('x'), |key| {
        base_seen
            .borrow_mut()
            .push(key.key_id.clone().unwrap_or_default());
        HandleOutcome::Consumed
    });

    assert_eq!(overlay_log.borrow().as_slice(), &["x".to_string()]);
    assert!(
        base_log.borrow().is_empty(),
        "a key the non-capturing overlay consumed must not fall through",
    );
    assert_eq!(handled, HandleOutcome::Consumed);
}

/// A non-capturing overlay above a capturing one: the key falls from the
/// non-capturing top, is blocked by the capturing layer, and never reaches the
/// base. The mixed-stack LIFO story.
#[test]
fn non_capturing_above_capturing_blocks_at_the_capturing_layer() {
    let base_log: KeyLog = Rc::new(RefCell::new(Vec::new()));
    let capturing_log: KeyLog = Rc::new(RefCell::new(Vec::new()));
    let toast_log: KeyLog = Rc::new(RefCell::new(Vec::new()));

    let mut stack = OverlayStack::new();
    stack.push(Overlay::new(
        Box::new(MockComponent::new(capturing_log.clone(), vec![], "modal")),
        opts(OverlayAnchor::Center, true),
    ));
    stack.push(Overlay::new(
        Box::new(MockComponent::new(toast_log.clone(), vec![], "toast")),
        opts(OverlayAnchor::TopRight, false),
    ));

    let base_seen = base_log.clone();
    stack.dispatch_key(&char_key('q'), |key| {
        base_seen
            .borrow_mut()
            .push(key.key_id.clone().unwrap_or_default());
        HandleOutcome::Consumed
    });

    assert_eq!(toast_log.borrow().as_slice(), &["q".to_string()]);
    assert_eq!(
        capturing_log.borrow().as_slice(),
        &["q".to_string()],
        "the key falls through the non-capturing top to the capturing layer",
    );
    assert!(
        base_log.borrow().is_empty(),
        "the capturing layer blocks the fall-through to the base",
    );
}

/// With no overlays open, dispatch routes straight to the base view.
#[test]
fn empty_stack_routes_to_base() {
    let base_log: KeyLog = Rc::new(RefCell::new(Vec::new()));
    let mut stack = OverlayStack::new();

    let base_seen = base_log.clone();
    let handled = stack.dispatch_key(&char_key('m'), |key| {
        base_seen
            .borrow_mut()
            .push(key.key_id.clone().unwrap_or_default());
        HandleOutcome::Consumed
    });

    assert_eq!(base_log.borrow().as_slice(), &["m".to_string()]);
    assert_eq!(handled, HandleOutcome::Consumed);
}

// ===========================================================================
// VAL-OVERLAY-030 — nine-anchor geometry (pure, no backend)
// ===========================================================================

/// Every one of the nine anchors places an un-margined, un-bordered content rect
/// at the expected corner/edge/center of the viewport.
#[test]
fn all_nine_anchors_place_at_expected_points() {
    let area = Rect::new(0, 0, 80, 24);
    let content = Size::new(10, 4);
    let margin = OverlayMargin::default();

    // (anchor, expected top-left x, y)
    let cases = [
        (OverlayAnchor::TopLeft, 0, 0),
        (OverlayAnchor::TopCenter, 35, 0),
        (OverlayAnchor::TopRight, 70, 0),
        (OverlayAnchor::CenterLeft, 0, 10),
        (OverlayAnchor::Center, 35, 10),
        (OverlayAnchor::CenterRight, 70, 10),
        (OverlayAnchor::BottomLeft, 0, 20),
        (OverlayAnchor::BottomCenter, 35, 20),
        (OverlayAnchor::BottomRight, 70, 20),
    ];

    for (anchor, ex, ey) in cases {
        let rect = anchor_rect(content, area, anchor, margin, false);
        assert_eq!(
            (rect.x, rect.y),
            (ex, ey),
            "anchor {anchor:?} must place content at ({ex}, {ey})",
        );
        assert_eq!(
            (rect.width, rect.height),
            (10, 4),
            "anchor {anchor:?} must preserve the content size",
        );
    }
}

/// A margin offsets the overlay inward from the anchored edge(s): from the top
/// and left for a top-left anchor, from the bottom and right for a bottom-right
/// anchor.
#[test]
fn margin_offsets_from_the_anchored_edges() {
    let area = Rect::new(0, 0, 80, 24);
    let content = Size::new(10, 4);
    let margin = OverlayMargin {
        top: 2,
        left: 3,
        right: 5,
        bottom: 1,
    };

    // Top-left: pushed in by (left, top).
    let tl = anchor_rect(content, area, OverlayAnchor::TopLeft, margin, false);
    assert_eq!((tl.x, tl.y), (3, 2), "top-left margin pushes right+down");

    // Bottom-right: pushed in by (right, bottom) from the far edges.
    let br = anchor_rect(content, area, OverlayAnchor::BottomRight, margin, false);
    // right edge = 80 - 5(right) - 10(w) = 65 ; bottom edge = 24 - 1(bottom) - 4(h) = 19
    assert_eq!(
        (br.x, br.y),
        (65, 19),
        "bottom-right margin pushes left+up from the far edges",
    );

    // Center is unaffected by symmetric-ish margins in this simple model: the
    // centered axis ignores the perpendicular margins (documented behaviour).
    let c = anchor_rect(content, area, OverlayAnchor::Center, margin, false);
    assert_eq!((c.x, c.y), (35, 10), "center ignores margins on both axes");
}

/// An overlay larger than the viewport clamps to fit inside it rather than
/// overflowing past the right/bottom edges — for every anchor.
#[test]
fn oversized_overlay_clamps_into_viewport() {
    let area = Rect::new(0, 0, 20, 8);
    // Content larger than the viewport on both axes.
    let content = Size::new(40, 16);
    let margin = OverlayMargin::default();

    for anchor in ALL_ANCHORS {
        let rect = anchor_rect(content, area, anchor, margin, false);
        assert!(
            rect.x >= area.x
                && rect.y >= area.y
                && rect.right() <= area.right()
                && rect.bottom() <= area.bottom(),
            "anchor {anchor:?} must clamp an oversized overlay inside the viewport, \
             got {rect:?} vs area {area:?}",
        );
        // Clamped to the viewport dimensions.
        assert_eq!(rect.width, 20, "clamped width fills the viewport");
        assert_eq!(rect.height, 8, "clamped height fills the viewport");
    }
}

/// A full-viewport-width bordered overlay keeps its right border strictly inside
/// the viewport: the rect never extends past the right edge, so the drawn `│`
/// lands on the last column, not one past it (the border-overflow bug family).
#[test]
fn full_bleed_bordered_overlay_never_overflows_right_edge() {
    let area = Rect::new(0, 0, 40, 10);
    // Request the *entire* viewport width for a bordered overlay.
    let content = Size::new(40, 5);
    let margin = OverlayMargin::default();

    let rect = anchor_rect(content, area, OverlayAnchor::TopCenter, margin, true);
    assert!(
        rect.right() <= area.right(),
        "a full-bleed bordered overlay must not overflow the right edge: \
         rect.right()={} area.right()={}",
        rect.right(),
        area.right(),
    );
    assert_eq!(rect.x, 0, "a full-width overlay hugs the left edge");
    assert_eq!(rect.width, 40, "and spans exactly the viewport width");

    // Render it and confirm the right border column is painted inside the buffer,
    // never clipped or pushed off-screen.
    let mut buf = Buffer::empty(area);
    Block::bordered().render(rect, &mut buf);
    let right_border = buf[(area.width - 1, rect.y)].symbol().to_string();
    assert_eq!(
        right_border, "┐",
        "the top-right border corner must land on the last column",
    );
}

const ALL_ANCHORS: [OverlayAnchor; 9] = [
    OverlayAnchor::TopLeft,
    OverlayAnchor::TopCenter,
    OverlayAnchor::TopRight,
    OverlayAnchor::CenterLeft,
    OverlayAnchor::Center,
    OverlayAnchor::CenterRight,
    OverlayAnchor::BottomLeft,
    OverlayAnchor::BottomCenter,
    OverlayAnchor::BottomRight,
];

// ===========================================================================
// VAL-CORE-025 / 026 — dim background + residue-free unmount (over TestBackend)
// ===========================================================================

/// Rendering a dim-background capturing overlay marks the base cells outside the
/// overlay with the DIM modifier (background stays visible but dimmed), while the
/// overlay's own cells are painted normally (not dimmed).
#[test]
fn dim_background_dims_base_but_not_overlay() {
    let area = Rect::new(0, 0, 40, 10);
    let mut buf = Buffer::empty(area);
    // Paint a base layer of 'B's so there is content to dim.
    for y in 0..area.height {
        buf.set_string(0, y, "B".repeat(area.width as usize), Style::default());
    }

    let mut stack = OverlayStack::new();
    let overlay_opts = OverlayOptions {
        anchor: OverlayAnchor::Center,
        margin: OverlayMargin::default(),
        capture_input: true,
        dim_background: true,
        border: true,
    };
    stack.push(Overlay::new(
        Box::new(MockComponent::new(
            Rc::new(RefCell::new(Vec::new())),
            vec![],
            "MODAL",
        )),
        overlay_opts,
    ));

    let overlay_rect = stack.render(area, &mut buf);
    let overlay_rect = overlay_rect.expect("a rendered overlay reports its rect");

    // A base cell well outside the overlay is dimmed.
    let corner = &buf[(0, 0)];
    assert!(
        corner.modifier.contains(Modifier::DIM),
        "a base cell outside the overlay must be dimmed",
    );

    // A cell inside the overlay's interior is NOT dimmed (the modal is crisp).
    let inside = &buf[(overlay_rect.x + 1, overlay_rect.y + 1)];
    assert!(
        !inside.modifier.contains(Modifier::DIM),
        "the overlay's own cells must not be dimmed",
    );
}

/// After an overlay is popped, re-rendering leaves the buffer with zero overlay
/// residue: no leftover border glyphs, no lingering DIM modifier — the base
/// content is restored crisp. This is the residue-free-unmount guarantee.
#[test]
fn popping_overlay_leaves_no_residue() {
    let area = Rect::new(0, 0, 40, 10);
    let mut terminal = Terminal::with_options(TestBackend::new(40, 10), inline(area)).unwrap();

    let mut stack = OverlayStack::new();
    stack.push(Overlay::new(
        Box::new(MockComponent::new(
            Rc::new(RefCell::new(Vec::new())),
            vec![],
            "MODAL",
        )),
        OverlayOptions {
            anchor: OverlayAnchor::Center,
            margin: OverlayMargin::default(),
            capture_input: true,
            dim_background: true,
            border: true,
        },
    ));

    // Frame 1: base painted, overlay open (dim + border present).
    draw_base_and_stack(&mut terminal, &mut stack);
    let open = terminal.backend().buffer().clone();
    assert!(
        buffer_has_dim(&open),
        "with the overlay open, the base must show DIM cells",
    );
    assert!(
        buffer_contains_border(&open),
        "with the overlay open, a border glyph must be present",
    );

    // Close the overlay and repaint.
    let popped = stack.pop();
    assert!(popped.is_some(), "pop returns the removed overlay");
    draw_base_and_stack(&mut terminal, &mut stack);
    let closed = terminal.backend().buffer().clone();

    assert!(
        !buffer_has_dim(&closed),
        "after close, no cell may keep the DIM modifier (no dim residue)",
    );
    assert!(
        !buffer_contains_border(&closed),
        "after close, no border glyph may linger (no ghost border)",
    );
}

/// The overlay stack survives a resize while open and still leaves no residue on
/// close: opening at one size, resizing (re-render at a new size), then closing
/// restores a clean base at the new size — the resize-then-close residue family.
#[test]
fn resize_while_open_then_close_leaves_no_residue() {
    let mut stack = OverlayStack::new();
    stack.push(Overlay::new(
        Box::new(MockComponent::new(
            Rc::new(RefCell::new(Vec::new())),
            vec![],
            "MODAL",
        )),
        OverlayOptions {
            anchor: OverlayAnchor::Center,
            margin: OverlayMargin::default(),
            capture_input: true,
            dim_background: true,
            border: true,
        },
    ));

    // Open at the big size: base painted, overlay re-anchored to it, border shown.
    let big = Rect::new(0, 0, 60, 16);
    assert!(
        buffer_contains_border(&render_base_and_stack(big, &mut stack)),
        "the overlay shows a border at the big size",
    );

    // Resize smaller *while the overlay is still open* and re-render: the overlay
    // re-anchors to the new, smaller viewport and still paints a border — the
    // whole-viewport repaint is the resize path a live session takes.
    let small = Rect::new(0, 0, 40, 10);
    let resized = render_base_and_stack(small, &mut stack);
    assert!(
        buffer_contains_border(&resized),
        "the overlay re-anchors to the new size and still shows a border",
    );

    // Close and repaint at the new size: a fresh buffer with only the base painted
    // (the overlay popped) has zero dim or border residue.
    stack.pop();
    let closed = render_base_and_stack(small, &mut stack);
    assert!(
        !buffer_has_dim(&closed) && !buffer_contains_border(&closed),
        "closing after a resize must leave no dim or border residue",
    );
}

// ===========================================================================
// VAL-CORE-027 — async mount channel: show/hide with no keypress
// ===========================================================================

/// A background task can `show` an overlay through the mount handle; draining the
/// channel mounts it onto the stack, and a subsequent render paints it — no key
/// was ever dispatched.
#[test]
fn async_mount_channel_shows_and_hides_without_keys() {
    let mut stack = OverlayStack::new();
    let handle = stack.mount_handle();

    assert_eq!(stack.len(), 0, "starts empty");

    // A "background task" asks to show an overlay. This is the cross-task handle.
    let id = handle.show(
        Box::new(MockComponent::new(
            Rc::new(RefCell::new(Vec::new())),
            vec![],
            "ASYNC",
        )),
        OverlayOptions {
            anchor: OverlayAnchor::Center,
            margin: OverlayMargin::default(),
            capture_input: false,
            dim_background: false,
            border: true,
        },
    );

    // Nothing is mounted until the run loop drains the channel.
    assert_eq!(stack.len(), 0, "show only queues; drain mounts");
    let mounted = stack.drain_mounts();
    assert!(
        mounted,
        "draining a pending show reports a change (request_frame)"
    );
    assert_eq!(stack.len(), 1, "the overlay is now mounted, no key needed");

    // It renders (paints a border) with no keypress at all.
    let area = Rect::new(0, 0, 40, 10);
    let mut buf = Buffer::empty(area);
    let rect = stack.render(area, &mut buf);
    assert!(rect.is_some());
    assert!(
        buffer_contains_border(&buf),
        "the async-mounted overlay paints itself with no key dispatched",
    );

    // The background task hides it by id; draining unmounts it.
    handle.hide(id);
    let changed = stack.drain_mounts();
    assert!(changed, "draining a pending hide reports a change");
    assert_eq!(stack.len(), 0, "the overlay unmounts, again with no key");
}

/// Draining an empty mount channel reports no change, so the run loop does not
/// request a needless frame.
#[test]
fn draining_empty_mount_channel_reports_no_change() {
    let mut stack = OverlayStack::new();
    let _handle = stack.mount_handle();
    assert!(
        !stack.drain_mounts(),
        "an empty drain must report no change",
    );
}

// --- render/draw helpers ----------------------------------------------------

fn inline(area: Rect) -> TerminalOptions {
    TerminalOptions {
        viewport: Viewport::Fixed(area),
    }
}

/// Paint a base layer of 'B's into a fresh `area`-sized buffer, then render the
/// overlay stack over it — the exact two-step the session draw path does (base
/// view, then overlays on top), but at an arbitrary size so a resize is just a
/// differently-sized fresh buffer (the whole-viewport repaint a resize triggers).
fn render_base_and_stack(area: Rect, stack: &mut OverlayStack) -> Buffer {
    let mut buf = Buffer::empty(area);
    for y in area.top()..area.bottom() {
        buf.set_string(area.x, y, "B".repeat(area.width as usize), Style::default());
    }
    stack.render(area, &mut buf);
    buf
}

/// Paint a base layer of 'B's then render the overlay stack over it — the exact
/// two-step the session draw path does (base view, then overlays on top).
fn draw_base_and_stack(terminal: &mut Terminal<TestBackend>, stack: &mut OverlayStack) {
    terminal
        .draw(|frame| {
            let area = frame.area();
            let buf = frame.buffer_mut();
            for y in 0..area.height {
                buf.set_string(0, y, "B".repeat(area.width as usize), Style::default());
            }
            stack.render(area, buf);
        })
        .unwrap();
}

/// Whether any cell in the buffer carries the DIM modifier.
fn buffer_has_dim(buf: &Buffer) -> bool {
    buf.content()
        .iter()
        .any(|c| c.modifier.contains(Modifier::DIM))
}

/// Whether any cell in the buffer paints a box-drawing border glyph.
fn buffer_contains_border(buf: &Buffer) -> bool {
    const BORDER: &[&str] = &["┌", "┐", "└", "┘", "│", "─"];
    buf.content().iter().any(|c| BORDER.contains(&c.symbol()))
}
