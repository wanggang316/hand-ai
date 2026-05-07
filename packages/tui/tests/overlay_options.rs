//! Regression: `OverlayOptions` anchor / margin / border / dim semantics.
//!
//! The TS port had a richer `OverlayOptions` (absolute row/col, maxHeight,
//! offsetX, percentage widths, etc.). The Rust port is intentionally
//! simpler: anchor + margin + border + dim_background + capture_input. This
//! file pins the behaviour for the options the Rust port does support — the
//! ones with named regressions in pi-tui were anchor positioning, margin
//! application, and the stacked-overlays render order.

use hand_tui::utils::strip_ansi;
use hand_tui::{
    Component, HandleResult, InputEvent, OverlayAnchor, OverlayMargin, OverlayOptions,
    compose_overlays,
};

struct Static {
    lines: Vec<String>,
}

impl Static {
    fn new(lines: Vec<&str>) -> Self {
        Self {
            lines: lines.into_iter().map(String::from).collect(),
        }
    }
}

impl Component for Static {
    fn render(&self, _w: u16) -> Vec<String> {
        self.lines.clone()
    }
    fn handle_input(&mut self, _e: &InputEvent) -> HandleResult {
        HandleResult::Ignored
    }
}

fn opts(anchor: OverlayAnchor, margin: OverlayMargin, border: bool, dim: bool) -> OverlayOptions {
    OverlayOptions {
        anchor,
        margin,
        capture_input: false,
        dim_background: dim,
        border,
    }
}

fn empty_base(width: u16, height: u16) -> Vec<String> {
    (0..height as usize)
        .map(|_| " ".repeat(width as usize))
        .collect()
}

#[test]
fn regression_top_left_anchor_renders_at_row_0_col_0() {
    let comp = Static::new(vec!["TOP-LEFT"]);
    let o = opts(
        OverlayAnchor::TopLeft,
        OverlayMargin::default(),
        false,
        false,
    );
    let result = compose_overlays(&empty_base(80, 24), &[(&comp, &o)], 80, 24);
    let row0 = strip_ansi(&result[0]);
    assert!(
        row0.starts_with("TOP-LEFT"),
        "TopLeft overlay must render at row 0 col 0, got row 0: {row0:?}"
    );
}

#[test]
fn regression_bottom_right_anchor_renders_at_last_row_last_col() {
    let comp = Static::new(vec!["BTM-RIGHT"]);
    let o = opts(
        OverlayAnchor::BottomRight,
        OverlayMargin::default(),
        false,
        false,
    );
    let result = compose_overlays(&empty_base(80, 24), &[(&comp, &o)], 80, 24);
    let last = strip_ansi(&result[23]);
    assert!(
        last.contains("BTM-RIGHT"),
        "BottomRight overlay must land on the last row, got: {last:?}"
    );
    assert!(
        last.trim_end().ends_with("BTM-RIGHT"),
        "BottomRight overlay must end-align to the right edge, got: {last:?}"
    );
}

#[test]
fn regression_top_center_anchor_centers_horizontally() {
    let comp = Static::new(vec!["CENTERED"]);
    let o = opts(
        OverlayAnchor::TopCenter,
        OverlayMargin::default(),
        false,
        false,
    );
    let result = compose_overlays(&empty_base(80, 24), &[(&comp, &o)], 80, 24);
    let row0 = strip_ansi(&result[0]);
    let col = row0.find("CENTERED").expect("centered text on row 0");
    // 8-char text in 80-col viewport: ideal center col = 36.
    assert!(
        (33..=39).contains(&col),
        "TopCenter overlay must be roughly centered, got col {col}"
    );
}

#[test]
fn regression_margin_offsets_overlay_from_anchor_edge() {
    let comp = Static::new(vec!["MARGIN"]);
    let m = OverlayMargin {
        top: 2,
        left: 3,
        right: 0,
        bottom: 0,
    };
    let o = opts(OverlayAnchor::TopLeft, m, false, false);
    let result = compose_overlays(&empty_base(80, 24), &[(&comp, &o)], 80, 24);
    let row2 = strip_ansi(&result[2]);
    let col = row2
        .find("MARGIN")
        .unwrap_or_else(|| panic!("MARGIN text must land on row 2 (top: 2), full row: {row2:?}"));
    assert_eq!(col, 3, "MARGIN must start at left margin col 3, got {col}");
    // Earlier rows must NOT contain the overlay.
    for (i, row) in result.iter().enumerate().take(2) {
        assert!(
            !strip_ansi(row).contains("MARGIN"),
            "row {i} (above top margin) must not contain overlay: {row:?}"
        );
    }
}

#[test]
fn regression_uniform_margin_applies_on_all_sides() {
    let comp = Static::new(vec!["UM"]);
    let o = opts(
        OverlayAnchor::TopLeft,
        OverlayMargin::uniform(5),
        false,
        false,
    );
    let result = compose_overlays(&empty_base(80, 24), &[(&comp, &o)], 80, 24);
    for (i, row) in result.iter().enumerate().take(5) {
        assert!(
            !strip_ansi(row).contains("UM"),
            "row {i} (above uniform margin) must not contain overlay: {row:?}"
        );
    }
    let row5 = strip_ansi(&result[5]);
    let col = row5.find("UM").expect("UM text must land on row 5");
    assert_eq!(col, 5, "uniform margin must offset by 5 cols, got {col}");
}

#[test]
fn regression_border_wraps_overlay_with_box_drawing() {
    let comp = Static::new(vec!["X"]);
    let o = opts(
        OverlayAnchor::TopLeft,
        OverlayMargin::default(),
        true, // border
        false,
    );
    let result = compose_overlays(&empty_base(80, 24), &[(&comp, &o)], 80, 24);
    let top = strip_ansi(&result[0]);
    let mid = strip_ansi(&result[1]);
    let bot = strip_ansi(&result[2]);
    assert!(
        top.starts_with('┌') && top.contains('┐'),
        "border top must use ┌...┐, got: {top:?}"
    );
    assert!(
        mid.contains('│') && mid.contains('X'),
        "border middle must contain content between │ │, got: {mid:?}"
    );
    assert!(
        bot.starts_with('└') && bot.contains('┘'),
        "border bottom must use └...┘, got: {bot:?}"
    );
}

#[test]
fn regression_dim_background_wraps_base_rows_with_dim_sgr() {
    // Base with content; overlay at top-left with dim_background ON.
    let base: Vec<String> = vec!["base content".repeat(5); 6];
    let comp = Static::new(vec!["OVERLAY"]);
    let o = opts(
        OverlayAnchor::TopLeft,
        OverlayMargin::default(),
        false,
        true, // dim
    );
    let result = compose_overlays(&base, &[(&comp, &o)], 80, 6);
    // The row not touched by the overlay (last row) must carry the dim
    // wrapping `\x1b[2m...\x1b[22m`.
    let untouched = &result[5];
    assert!(
        untouched.contains("\x1b[2m") && untouched.contains("\x1b[22m"),
        "dim_background must wrap base rows with dim SGR, got: {untouched:?}"
    );
}

#[test]
fn regression_stacked_overlays_render_later_on_top() {
    // Two overlays at the same TopLeft position. The second one must paint
    // over the first because compose_overlays iterates in registration order
    // and stamps later overlays on top of earlier ones.
    let first = Static::new(vec!["FIRST-OVERLAY"]);
    let second = Static::new(vec!["SECOND"]);
    let o1 = opts(
        OverlayAnchor::TopLeft,
        OverlayMargin::default(),
        false,
        false,
    );
    let o2 = opts(
        OverlayAnchor::TopLeft,
        OverlayMargin::default(),
        false,
        false,
    );
    let result = compose_overlays(
        &empty_base(80, 24),
        &[(&first, &o1), (&second, &o2)],
        80,
        24,
    );
    let row0 = strip_ansi(&result[0]);
    assert!(
        row0.starts_with("SECOND"),
        "later overlay must render on top of earlier one, got row 0: {row0:?}"
    );
}

#[test]
fn regression_overlays_at_disjoint_positions_do_not_interfere() {
    let tl = Static::new(vec!["TOP-LEFT"]);
    let br = Static::new(vec!["BTM-RIGHT"]);
    let o_tl = opts(
        OverlayAnchor::TopLeft,
        OverlayMargin::default(),
        false,
        false,
    );
    let o_br = opts(
        OverlayAnchor::BottomRight,
        OverlayMargin::default(),
        false,
        false,
    );
    let result = compose_overlays(&empty_base(80, 24), &[(&tl, &o_tl), (&br, &o_br)], 80, 24);
    let row0 = strip_ansi(&result[0]);
    let row23 = strip_ansi(&result[23]);
    assert!(
        row0.starts_with("TOP-LEFT"),
        "TopLeft overlay must survive when a BottomRight one is also present, row 0: {row0:?}"
    );
    assert!(
        row23.contains("BTM-RIGHT"),
        "BottomRight overlay must survive when a TopLeft one is also present, row 23: {row23:?}"
    );
}
