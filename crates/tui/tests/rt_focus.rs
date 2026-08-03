//! Tests for the rt component / focus / dispatch model (`hand_tui::rt::view`).
//!
//! ratatui is a pure immediate-mode renderer with no focus and no event system,
//! so [`FocusView`] and [`RtComponent`] are the application layer that gives the
//! rt stack the three interactive guarantees the external validator probes:
//!
//! - **VAL-CORE-028** — exclusive focus routing: a key reaches only the focused
//!   component; switching focus redirects subsequent keys and freezes the old
//!   component (it never sees another key).
//! - **VAL-CORE-005** — typed chars land only in the focused input even while a
//!   background stream is committing; an unfocused component's state is frozen.
//! - **VAL-CORE-023** — the hardware cursor follows focus: it sits at the focused
//!   component's caret, is hidden when the focused component is caret-less, and is
//!   reproduced when focus returns — never left stray in the output region.
//!
//! Routing and focus switching are pinned with mock components (a spy that
//! records the keys it is handed); the cursor-follows-focus behaviour is pinned
//! both at the view layer (the `Option<Position>` decision) and end-to-end over
//! ratatui's `TestBackend`, driving a real inline `Terminal::draw` and asserting
//! the position the backend cursor lands at.

use std::cell::RefCell;
use std::rc::Rc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hand_tui::rt::events::RtKey;
use hand_tui::rt::view::{FocusView, HandleOutcome, RtComponent};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::{Terminal, TerminalOptions, Viewport};

// --- test helpers -----------------------------------------------------------

/// Build an `RtKey` for a single printable character with no modifiers.
fn char_key(c: char) -> RtKey {
    let raw = KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
    RtKey {
        key_id: Some(c.to_string()),
        raw,
    }
}

/// Build an `RtKey` for a named key id (e.g. `"tab"`).
fn named_key(id: &str, code: KeyCode) -> RtKey {
    RtKey {
        key_id: Some(id.to_string()),
        raw: KeyEvent::new(code, KeyModifiers::NONE),
    }
}

/// A shared, cloneable record of the keys a mock component was handed, so a test
/// can assert which component keys were routed to.
type KeyLog = Rc<RefCell<Vec<String>>>;

/// A mock editable input component: it consumes printable characters (appending
/// them to a shared buffer) and ignores everything else, so an ignored key can
/// bubble to a view-level focus switch. It reports a caret one column past its
/// current text, translated into its own area's coordinate space.
struct MockInput {
    /// The keys this component was actually handed (spy for routing assertions).
    log: KeyLog,
    /// The text accumulated from consumed character keys.
    text: String,
}

impl MockInput {
    fn new(log: KeyLog) -> Self {
        Self {
            log,
            text: String::new(),
        }
    }
}

impl RtComponent for MockInput {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        buf.set_string(area.x, area.y, &self.text, ratatui::style::Style::default());
    }

    fn handle_key(&mut self, key: &RtKey) -> HandleOutcome {
        self.log
            .borrow_mut()
            .push(key.key_id.clone().unwrap_or_default());
        match key.raw.code {
            KeyCode::Char(c) => {
                self.text.push(c);
                HandleOutcome::Consumed
            }
            // Anything else bubbles (so e.g. Tab can drive a focus switch).
            _ => HandleOutcome::Ignored,
        }
    }

    fn cursor(&self) -> Option<Position> {
        // Caret sits just past the typed text, on the component's first row.
        Some(Position::new(self.text.chars().count() as u16, 0))
    }
}

/// A mock read-only block: it records nothing to type into, ignores every key,
/// and reports no caret — the caret-less focus target that must hide the
/// hardware cursor.
struct MockReadonly {
    log: KeyLog,
}

impl MockReadonly {
    fn new(log: KeyLog) -> Self {
        Self { log }
    }
}

impl RtComponent for MockReadonly {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        buf.set_string(area.x, area.y, "READONLY", ratatui::style::Style::default());
    }

    fn handle_key(&mut self, key: &RtKey) -> HandleOutcome {
        self.log
            .borrow_mut()
            .push(key.key_id.clone().unwrap_or_default());
        // A read-only block never consumes a key.
        HandleOutcome::Ignored
    }

    // No caret: default `cursor()` returns None.
}

// --- VAL-CORE-028: exclusive focus routing ---------------------------------

/// A key reaches only the focused component; the unfocused one is never handed a
/// key at all (its state is frozen).
#[test]
fn dispatch_routes_only_to_focused_component() {
    let input_log: KeyLog = Rc::new(RefCell::new(Vec::new()));
    let ro_log: KeyLog = Rc::new(RefCell::new(Vec::new()));

    let mut view = FocusView::new(vec![
        Box::new(MockInput::new(input_log.clone())),
        Box::new(MockReadonly::new(ro_log.clone())),
    ]);
    // Focus starts on the first (input) component.
    assert_eq!(view.focused(), 0);

    let outcome = view.dispatch_key(&char_key('a'));
    assert_eq!(outcome, HandleOutcome::Consumed);

    // The input saw the key; the read-only block saw nothing.
    assert_eq!(input_log.borrow().as_slice(), &["a".to_string()]);
    assert!(
        ro_log.borrow().is_empty(),
        "unfocused component must be frozen (receive no keys)"
    );
}

/// Switching focus redirects subsequent keys to the newly focused component and
/// freezes the previously focused one.
#[test]
fn focus_switch_redirects_routing_and_freezes_old() {
    let input_log: KeyLog = Rc::new(RefCell::new(Vec::new()));
    let ro_log: KeyLog = Rc::new(RefCell::new(Vec::new()));

    let mut view = FocusView::new(vec![
        Box::new(MockInput::new(input_log.clone())),
        Box::new(MockReadonly::new(ro_log.clone())),
    ]);

    // Type into the input while it is focused.
    view.dispatch_key(&char_key('x'));
    assert_eq!(input_log.borrow().len(), 1);

    // Switch focus to the read-only block.
    view.focus_next();
    assert_eq!(view.focused(), 1);

    // Subsequent keys now go to the read-only block, not the input.
    view.dispatch_key(&char_key('y'));
    assert_eq!(
        input_log.borrow().len(),
        1,
        "the previously focused input must be frozen after focus leaves it"
    );
    assert_eq!(ro_log.borrow().as_slice(), &["y".to_string()]);

    // Wrapping past the last returns to the first.
    view.focus_next();
    assert_eq!(view.focused(), 0);
}

/// An ignored key bubbles: `dispatch_key` returns `Ignored` so the caller can
/// treat it as a view-level action (e.g. a focus-switch key).
#[test]
fn ignored_key_bubbles_for_view_level_handling() {
    let input_log: KeyLog = Rc::new(RefCell::new(Vec::new()));
    let ro_log: KeyLog = Rc::new(RefCell::new(Vec::new()));

    let mut view = FocusView::new(vec![
        Box::new(MockInput::new(input_log.clone())),
        Box::new(MockReadonly::new(ro_log)),
    ]);

    // Tab is not a printable char, so the focused input ignores it.
    let outcome = view.dispatch_key(&named_key("tab", KeyCode::Tab));
    assert_eq!(
        outcome,
        HandleOutcome::Ignored,
        "a key the focused component does not consume must bubble"
    );
    // The focused input still saw the key (it just chose not to consume it).
    assert_eq!(input_log.borrow().as_slice(), &["tab".to_string()]);

    // A printable char is consumed and does not bubble.
    assert_eq!(view.dispatch_key(&char_key('z')), HandleOutcome::Consumed);
}

// --- VAL-CORE-005: typing stays in the focused input -----------------------

/// The focused input accumulates only the characters routed to it; a component
/// that is not focused never receives a character, so it stays frozen — the
/// invariant behind "typed chars land only in the focused input, other regions
/// frozen".
#[test]
fn typed_chars_land_only_in_focused_input() {
    let a_log: KeyLog = Rc::new(RefCell::new(Vec::new()));
    let b_log: KeyLog = Rc::new(RefCell::new(Vec::new()));

    // Two inputs so we can watch typing move with focus.
    let mut view = FocusView::new(vec![
        Box::new(MockInput::new(a_log.clone())),
        Box::new(MockInput::new(b_log.clone())),
    ]);

    for c in ['h', 'i'] {
        view.dispatch_key(&char_key(c));
    }
    // Second input is frozen: it received nothing.
    assert!(b_log.borrow().is_empty());
    assert_eq!(
        a_log.borrow().as_slice(),
        &["h".to_string(), "i".to_string()]
    );

    // Move focus and keep typing: only the now-focused input accumulates.
    view.focus_next();
    view.dispatch_key(&char_key('!'));
    assert_eq!(a_log.borrow().len(), 2, "old input frozen after focus left");
    assert_eq!(b_log.borrow().as_slice(), &["!".to_string()]);
}

// --- VAL-CORE-023: cursor follows focus (view layer) -----------------------

/// The view reports the focused component's caret, offset by the rect it was
/// rendered at, and `None` when the focused component is caret-less.
#[test]
fn cursor_follows_focus_and_hides_on_caretless() {
    let input_log: KeyLog = Rc::new(RefCell::new(Vec::new()));
    let ro_log: KeyLog = Rc::new(RefCell::new(Vec::new()));

    let mut view = FocusView::new(vec![
        Box::new(MockInput::new(input_log)),
        Box::new(MockReadonly::new(ro_log)),
    ]);

    // Before any render there is no recorded area, so no cursor to place.
    assert_eq!(view.cursor(), None);

    // Lay the two components out at distinct origins and paint.
    let input_area = Rect::new(2, 5, 20, 1);
    let ro_area = Rect::new(2, 7, 20, 1);
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 10));
    view.render(&[input_area, ro_area], &mut buf);

    // Focused input has an empty caret at offset (0,0) → the area origin.
    assert_eq!(view.cursor(), Some(Position::new(2, 5)));

    // Type two chars; the caret advances two columns within the input area.
    view.dispatch_key(&char_key('a'));
    view.dispatch_key(&char_key('b'));
    view.render(&[input_area, ro_area], &mut buf);
    assert_eq!(view.cursor(), Some(Position::new(4, 5)));

    // Focus the read-only block: it has no caret, so the hardware cursor hides.
    view.focus_next();
    assert_eq!(
        view.cursor(),
        None,
        "a caret-less focus must hide the hardware cursor"
    );

    // Focus back to the input: the caret reappears at its remembered position —
    // never stranded in the read-only region.
    view.focus_next();
    assert_eq!(view.cursor(), Some(Position::new(4, 5)));
}

/// The reported caret is clamped inside the focused component's own area, so a
/// caret past the end of a narrow area can never stray into another region.
#[test]
fn cursor_is_clamped_inside_the_component_area() {
    let log: KeyLog = Rc::new(RefCell::new(Vec::new()));
    let mut view = FocusView::new(vec![Box::new(MockInput::new(log))]);

    // A one-cell-wide area: even after typing, the caret cannot leave column x.
    let narrow = Rect::new(3, 4, 1, 1);
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 10));
    for c in ['a', 'b', 'c'] {
        view.dispatch_key(&char_key(c));
    }
    view.render(&[narrow], &mut buf);
    assert_eq!(
        view.cursor(),
        Some(Position::new(3, 4)),
        "caret must clamp to the component's single cell, never past it"
    );
}

// --- VAL-CORE-023: cursor follows focus (end-to-end over TestBackend) ------

/// Drive a real inline `Terminal::draw` over `TestBackend`, feeding the focused
/// component's caret to `Frame::set_cursor_position`, and assert the backend
/// cursor lands exactly there — the hardware-cursor-follows-focus path end to
/// end. Focusing a caret-less component leaves the cursor where it was rather
/// than moving it into the read-only region.
#[test]
fn hardware_cursor_tracks_focus_end_to_end() {
    let input_log: KeyLog = Rc::new(RefCell::new(Vec::new()));
    let ro_log: KeyLog = Rc::new(RefCell::new(Vec::new()));

    let mut view = FocusView::new(vec![
        Box::new(MockInput::new(input_log)),
        Box::new(MockReadonly::new(ro_log)),
    ]);

    let backend = TestBackend::new(40, 10);
    let mut terminal = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Fixed(Rect::new(0, 0, 40, 10)),
        },
    )
    .unwrap();

    let input_area = Rect::new(1, 2, 20, 1);
    let ro_area = Rect::new(1, 4, 20, 1);

    // Type a character, then draw: the frame cursor is set to the focused
    // input's caret, so the backend cursor lands one column past the origin.
    view.dispatch_key(&char_key('q'));
    draw_view(&mut terminal, &mut view, input_area, ro_area);
    terminal
        .backend_mut()
        .assert_cursor_position(Position::new(2, 2));

    // Switch focus to the read-only block and draw. It reports no caret, so the
    // draw does not call `set_cursor_position`; the cursor stays put (not moved
    // into the read-only region) and ratatui hides it.
    view.focus_next();
    draw_view(&mut terminal, &mut view, input_area, ro_area);
    terminal
        .backend_mut()
        .assert_cursor_position(Position::new(2, 2));

    // Switch focus back to the input and draw: the caret is reproduced.
    view.focus_next();
    draw_view(&mut terminal, &mut view, input_area, ro_area);
    terminal
        .backend_mut()
        .assert_cursor_position(Position::new(2, 2));
}

/// Render the view and drive the hardware cursor from its reported caret — the
/// exact glue the session's draw path uses.
fn draw_view(
    terminal: &mut Terminal<TestBackend>,
    view: &mut FocusView,
    input_area: Rect,
    ro_area: Rect,
) {
    terminal
        .draw(|frame| {
            let buf = frame.buffer_mut();
            view.render(&[input_area, ro_area], buf);
            if let Some(pos) = view.cursor() {
                frame.set_cursor_position(pos);
            }
        })
        .unwrap();
}
