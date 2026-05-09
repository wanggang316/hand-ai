//! Regression: overlay must render correctly when base content is shorter
//! than the viewport.
//!
//! Historical bug (pi-tui): if the base component rendered fewer lines than
//! the terminal had rows, the overlay was anchored relative to the base
//! lines instead of the viewport. A centered overlay would land somewhere
//! around row 1 (between two short content lines) instead of the actual
//! middle of the screen — and at the limit could be clipped entirely.
//!
//! The fix made `compose_overlays` pad the base out to the full viewport
//! height before anchoring. This file pins that behaviour: an overlay placed
//! against a 3-line base in a 24-row viewport must still appear, and a
//! `Center`-anchored overlay must land near the middle of the viewport, not
//! near the middle of the (3-line) base.

use hand_tui::utils::strip_ansi;
use hand_tui::{
    Component, HandleResult, InputEvent, OverlayAnchor, OverlayMargin, OverlayOptions,
    compose_overlays,
};

struct Short;
impl Component for Short {
    fn render(&self, _w: u16) -> Vec<String> {
        vec!["Line 1".into(), "Line 2".into(), "Line 3".into()]
    }
}

struct OverlayBlock;
impl Component for OverlayBlock {
    fn render(&self, _w: u16) -> Vec<String> {
        vec![
            "OVERLAY_TOP".into(),
            "OVERLAY_MID".into(),
            "OVERLAY_BOT".into(),
        ]
    }
    fn handle_input(&mut self, _e: &InputEvent) -> HandleResult {
        HandleResult::Ignored
    }
}

fn opts() -> OverlayOptions {
    OverlayOptions {
        anchor: OverlayAnchor::Center,
        margin: OverlayMargin::default(),
        capture_input: false,
        dim_background: false,
        border: false,
    }
}

#[test]
fn regression_overlay_visible_when_base_shorter_than_viewport() {
    let base = Short.render(80);
    let overlay = OverlayBlock;
    let o = opts();
    let result = compose_overlays(&base, &[(&overlay, &o)], 80, 24);

    // Overlay text must appear somewhere in the composed frame.
    let any_overlay_row = result
        .iter()
        .any(|line| strip_ansi(line).contains("OVERLAY_MID"));
    assert!(
        any_overlay_row,
        "overlay must be visible when base content is shorter than the viewport; \
         composed frame: {result:?}"
    );
}

#[test]
fn regression_centered_overlay_anchors_to_viewport_not_base() {
    // 24-row viewport, 3-line base, 3-line overlay. With viewport-anchored
    // centering: middle row ≈ 10..=12. With base-anchored centering, the
    // overlay would land around row 0 (since (3-3)/2 = 0).
    let base = Short.render(80);
    let overlay = OverlayBlock;
    let o = opts();
    let result = compose_overlays(&base, &[(&overlay, &o)], 80, 24);

    let mid_row = result
        .iter()
        .position(|line| strip_ansi(line).contains("OVERLAY_MID"))
        .expect("OVERLAY_MID must be present");

    assert!(
        (9..=14).contains(&mid_row),
        "Center-anchored overlay must sit near viewport middle (rows 9..=14) \
         even when base is short, got row {mid_row}"
    );
}

#[test]
fn regression_compose_overlays_pads_to_height() {
    // The composed frame must have exactly `height` rows even when the base
    // is shorter — this is the structural property that keeps centering
    // viewport-anchored.
    let base = Short.render(80);
    let overlay = OverlayBlock;
    let o = opts();
    let result = compose_overlays(&base, &[(&overlay, &o)], 80, 24);
    assert_eq!(
        result.len(),
        24,
        "compose_overlays must pad output to viewport height, got {} rows",
        result.len()
    );
}

#[test]
fn regression_no_panic_when_base_is_empty_and_overlay_present() {
    // Degenerate case: an empty base with a viewport and an overlay must
    // still render without panicking. pi-tui crashed on this path before
    // because the slicing math underflowed.
    let base: Vec<String> = Vec::new();
    let overlay = OverlayBlock;
    let o = opts();
    let result = compose_overlays(&base, &[(&overlay, &o)], 80, 24);
    assert_eq!(result.len(), 24, "must pad empty base to viewport height");
    let visible = result
        .iter()
        .any(|line| strip_ansi(line).contains("OVERLAY_MID"));
    assert!(
        visible,
        "overlay must remain visible against an empty base, got: {result:?}"
    );
}
