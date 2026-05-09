//! Regression: `capture_input: false` overlays must let input through.
//!
//! The TS port called this "non-capturing" overlays and tied it to a
//! Focusable system that tracked which overlay had keyboard focus and which
//! had been bumped to the top of the visual stack. The Rust port has a
//! simpler model: there is no focus-tracking on overlays. `capture_input` is
//! the single switch that decides whether an overlay is modal (true) or
//! transparent to input (false).
//!
//! This file pins the input-pass-through guarantee, which is the load-bearing
//! piece of the historical regression: a non-capturing overlay must not eat
//! events that the underlying focused component would otherwise receive.

use std::sync::{Arc, Mutex};

use hand_tui::{
    Component, HandleResult, InputEvent, OverlayAnchor, OverlayMargin, OverlayOptions,
    TestTerminal, Tui,
};
use tokio::sync::mpsc;

/// Component that records every event it receives. Returns `Handled` so any
/// dispatch reaching it shows up in the recording.
struct Recorder {
    events: Arc<Mutex<Vec<InputEvent>>>,
}

impl Recorder {
    fn new() -> (Self, Arc<Mutex<Vec<InputEvent>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                events: events.clone(),
            },
            events,
        )
    }
}

impl Component for Recorder {
    fn render(&self, _w: u16) -> Vec<String> {
        Vec::new()
    }
    fn handle_input(&mut self, e: &InputEvent) -> HandleResult {
        self.events.lock().unwrap().push(e.clone());
        HandleResult::Handled
    }
}

fn opts(capture: bool) -> OverlayOptions {
    OverlayOptions {
        anchor: OverlayAnchor::Center,
        margin: OverlayMargin::default(),
        capture_input: capture,
        dim_background: false,
        border: false,
    }
}

fn make_tui() -> Tui {
    Tui::new(Box::new(TestTerminal::new(80, 24)))
}

/// The core regression: with a non-capturing overlay on top, input still
/// reaches the underlying root component. A previous bug had non-capturing
/// overlays still eating events because the modal-routing branch did not
/// check `capture_input` on every overlay.
#[tokio::test]
async fn regression_non_capturing_overlay_lets_input_pass_through() {
    let mut tui = make_tui();

    // Root receives events.
    let (root_recorder, root_events) = Recorder::new();
    let root_id = tui.root_mut().add_child_with_id(Box::new(root_recorder));
    tui.set_focus(Some(root_id));

    // Non-capturing overlay above it: must not block input.
    let (overlay_recorder, overlay_events) = Recorder::new();
    let _h = tui.show_overlay(Box::new(overlay_recorder), opts(false));

    // Drive a single input event through the run loop.
    let (tx, rx) = mpsc::unbounded_channel();
    tx.send(hand_tui::StdinBufferEvent::Data("a".to_string()))
        .unwrap();
    drop(tx);

    tokio::time::timeout(
        std::time::Duration::from_millis(200),
        tui.run_with_events(rx),
    )
    .await
    .expect("run did not exit on stdin close")
    .expect("run errored");

    let root_received = root_events.lock().unwrap().clone();
    let overlay_received = overlay_events.lock().unwrap().clone();

    assert!(
        !root_received.is_empty(),
        "non-capturing overlay swallowed input — root saw nothing: {root_received:?}"
    );
    assert!(
        overlay_received.is_empty(),
        "non-capturing overlay must not see input via the dispatch path, got: {overlay_received:?}"
    );

    // The event the root saw must be the one we sent.
    let saw_a = root_received
        .iter()
        .any(|e| matches!(e, InputEvent::Raw(s) if s == "a"));
    assert!(
        saw_a,
        "expected root to receive Raw(\"a\"), got: {root_received:?}"
    );
}

/// Symmetric counter-test: a *capturing* overlay must eat the event so the
/// root sees nothing. Locks the other half of the dispatch contract — the
/// historical bug had the two flavours blurred and either both saw input or
/// neither did.
#[tokio::test]
async fn regression_capturing_overlay_blocks_input_to_root() {
    let mut tui = make_tui();

    let (root_recorder, root_events) = Recorder::new();
    let root_id = tui.root_mut().add_child_with_id(Box::new(root_recorder));
    tui.set_focus(Some(root_id));

    let (overlay_recorder, overlay_events) = Recorder::new();
    let _h = tui.show_overlay(Box::new(overlay_recorder), opts(true));

    let (tx, rx) = mpsc::unbounded_channel();
    tx.send(hand_tui::StdinBufferEvent::Data("a".to_string()))
        .unwrap();
    drop(tx);

    tokio::time::timeout(
        std::time::Duration::from_millis(200),
        tui.run_with_events(rx),
    )
    .await
    .expect("run did not exit on stdin close")
    .expect("run errored");

    assert!(
        root_events.lock().unwrap().is_empty(),
        "capturing overlay must block input from reaching root: {:?}",
        root_events.lock().unwrap()
    );
    assert!(
        !overlay_events.lock().unwrap().is_empty(),
        "capturing overlay must receive input directly: {:?}",
        overlay_events.lock().unwrap()
    );
}

/// Stack regression: a non-capturing overlay underneath a capturing overlay
/// must remain transparent — the capturing one is the only modal blocker.
/// This pins the overlay iteration order: only `capture_input == true`
/// overlays count for routing, regardless of where they sit in the stack.
#[tokio::test]
async fn regression_stack_with_non_capturing_below_still_routes_to_capturing() {
    let mut tui = make_tui();

    let (root_recorder, root_events) = Recorder::new();
    let root_id = tui.root_mut().add_child_with_id(Box::new(root_recorder));
    tui.set_focus(Some(root_id));

    // Non-capturing overlay at the bottom of the overlay stack.
    let (nc_recorder, nc_events) = Recorder::new();
    let _h_nc = tui.show_overlay(Box::new(nc_recorder), opts(false));

    // Capturing overlay on top.
    let (cap_recorder, cap_events) = Recorder::new();
    let _h_cap = tui.show_overlay(Box::new(cap_recorder), opts(true));

    let (tx, rx) = mpsc::unbounded_channel();
    tx.send(hand_tui::StdinBufferEvent::Data("z".to_string()))
        .unwrap();
    drop(tx);

    tokio::time::timeout(
        std::time::Duration::from_millis(200),
        tui.run_with_events(rx),
    )
    .await
    .expect("run did not exit on stdin close")
    .expect("run errored");

    assert!(
        cap_events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, InputEvent::Raw(s) if s == "z")),
        "topmost capturing overlay must receive input: {:?}",
        cap_events.lock().unwrap()
    );
    assert!(
        nc_events.lock().unwrap().is_empty(),
        "non-capturing overlay must never see input via dispatch, got: {:?}",
        nc_events.lock().unwrap()
    );
    assert!(
        root_events.lock().unwrap().is_empty(),
        "root must not see input when a capturing overlay is up: {:?}",
        root_events.lock().unwrap()
    );
}
